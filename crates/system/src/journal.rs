//! Structured journal records preserve recorded priority and actual unit attribution.
#[cfg(any(target_os = "linux", test))]
use super::*;

#[cfg(any(target_os = "linux", test))]
pub(super) fn parse(line: &str) -> Option<LogEntry> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let field = |key: &str| value.get(key)?.as_str();
    let message = match value.get("MESSAGE")? {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(bytes) => String::from_utf8_lossy(
            &bytes
                .iter()
                .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect::<Option<Vec<_>>>()?,
        )
        .into_owned(),
        _ => return None,
    };
    let timestamp = field("__REALTIME_TIMESTAMP")
        .and_then(|raw| raw.parse::<i128>().ok())
        .and_then(|micros| micros.checked_mul(1000))
        .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos).ok())
        .and_then(|time| {
            time.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default();
    let unit = ["UNIT", "USER_UNIT", "_SYSTEMD_USER_UNIT", "_SYSTEMD_UNIT"]
        .into_iter()
        .find_map(field)
        .filter(|unit| !unit.chars().any(char::is_control))
        .map(str::to_owned);
    let priority = match field("PRIORITY") {
        Some("0" | "1" | "2") => Some("critical"),
        Some("3") => Some("error"),
        Some("4") => Some("warning"),
        Some("5") => Some("notice"),
        Some("6") => Some("info"),
        Some("7") => Some("debug"),
        _ => None,
    }
    .map(str::to_owned);
    Some(LogEntry {
        timestamp,
        source: "systemd-journal".into(),
        unit,
        priority,
        message,
        repeated: 1,
    })
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn push(entries: &mut Vec<LogEntry>, entry: LogEntry) {
    if let Some(previous) = entries.last_mut().filter(|p| {
        p.message == entry.message
            && p.unit == entry.unit
            && p.priority == entry.priority
            && p.source == entry.source
    }) {
        previous.repeated = previous.repeated.saturating_add(entry.repeated.max(1));
    } else {
        entries.push(entry);
    }
}

pub(super) fn display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_priority_overrides_message_words_and_manager_unit_is_preserved() {
        let record=parse(r#"{"__REALTIME_TIMESTAMP":"1700000000000000","PRIORITY":"6","MESSAGE":"no error occurred","_SYSTEMD_UNIT":"init.scope","UNIT":"worker.service"}"#).expect("record");
        assert_eq!(record.priority.as_deref(), Some("info"));
        assert_eq!(record.unit.as_deref(), Some("worker.service"));
        assert!(record.timestamp.starts_with("2023-11-14T"));
        let mut next = record.clone();
        let mut entries = vec![record.clone()];
        push(&mut entries, next.clone());
        assert_eq!(entries[0].repeated, 2);
        next.unit = Some("other.service".into());
        push(&mut entries, next);
        assert_eq!(entries.len(), 2);
        let mut next = record;
        next.priority = Some("error".into());
        push(&mut entries, next);
        assert_eq!(entries.len(), 3);
    }
    #[test]
    fn binary_missing_and_untrusted_messages_are_explicit() {
        assert_eq!(
            parse(r#"{"MESSAGE":[65,255],"PRIORITY":"3"}"#)
                .expect("binary")
                .message,
            "A�"
        );
        assert!(parse("broken").is_none());
        assert!(parse(r#"{"MESSAGE":null}"#).is_none());
        assert_eq!(display("hello\x1b[2J\nworld"), "hello [2J world");
    }
}
