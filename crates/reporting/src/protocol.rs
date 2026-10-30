use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const TTL_MS: u64 = 30 * 60 * 1000;
pub const MAX_REPORT_BYTES: usize = 256 * 1024;
pub const MAX_BATCH_BYTES: usize = 1024 * 1024;
pub const MAX_BATCH_REPORTS: usize = 4;
pub const DOMAINS: &[&str] = &[
    "host",
    "processes",
    "services",
    "containers",
    "logs",
    "storage",
    "network",
    "hardware",
    "system",
    "health",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub protocol_version: u32,
    pub schema_version: String,
    pub session_id: String,
    pub boot_id: String,
    pub sequence: u64,
    pub collected_at_ms: u64,
    pub expires_at_ms: u64,
    pub capabilities: Vec<String>,
    pub coverage: BTreeMap<String, String>,
    pub snapshot: Value,
}

impl Report {
    pub fn id(&self) -> String {
        format!("{}:{}", self.session_id, self.sequence)
    }

    pub fn validate(&self, now: u64) -> Result<()> {
        if self.protocol_version != 1 || self.schema_version != "2" {
            bail!("unsupported report version");
        }
        if uuid::Uuid::parse_str(&self.session_id)?.to_string() != self.session_id {
            bail!("noncanonical session identity");
        }
        if self.sequence == 0
            || self.boot_id.is_empty()
            || self.boot_id.len() > 128
            || self.expires_at_ms != self.collected_at_ms.saturating_add(TTL_MS)
            || self.expires_at_ms <= now
            || self.collected_at_ms > now.saturating_add(60_000)
            || !self.snapshot.is_object()
            || self.snapshot.get("schema_version").and_then(Value::as_str) != Some("2")
            || self.capabilities.len() > 64
            || self.coverage.len() > 64
            || self.capabilities.iter().any(|name| name.len() > 64)
            || self.coverage.keys().any(|name| name.len() > 64)
            || self
                .coverage
                .values()
                .any(|v| !["complete", "partial", "unavailable"].contains(&v.as_str()))
        {
            bail!("invalid or expired report");
        }
        Ok(())
    }

    pub fn from_snapshot(
        snapshot: lens_model::Snapshot,
        session: &str,
        boot: &str,
        sequence: u64,
        collected: u64,
    ) -> Result<Self> {
        let partial = !snapshot.collection_warnings.is_empty();
        let mut value = serde_json::to_value(snapshot)?;
        let truncated = bound_values(&mut value);
        let mut report = Self {
            protocol_version: 1,
            schema_version: "2".into(),
            session_id: session.into(),
            boot_id: boot.into(),
            sequence,
            collected_at_ms: collected,
            expires_at_ms: collected.saturating_add(TTL_MS),
            capabilities: DOMAINS.iter().map(|x| (*x).into()).collect(),
            coverage: DOMAINS
                .iter()
                .map(|x| {
                    (
                        (*x).into(),
                        if partial || truncated {
                            "partial"
                        } else {
                            "complete"
                        }
                        .into(),
                    )
                })
                .collect(),
            snapshot: value,
        };
        // Bounded reports are self-contained snapshots, not deltas requiring an unbounded replay.
        loop {
            if serde_json::to_vec(&report)?.len() <= MAX_REPORT_BYTES {
                return Ok(report);
            }
            let reduced = reduce_arrays(&mut report.snapshot);
            if !reduced {
                bail!("scalar snapshot exceeds reporting limit");
            }
            for status in report.coverage.values_mut() {
                *status = "partial".into();
            }
        }
    }
}

fn reduce_arrays(value: &mut Value) -> bool {
    match value {
        Value::Array(values) if !values.is_empty() => {
            values.truncate(values.len() / 2);
            for value in values {
                reduce_arrays(value);
            }
            true
        }
        Value::Object(values) => values
            .values_mut()
            .fold(false, |changed, value| reduce_arrays(value) || changed),
        _ => false,
    }
}

fn bound_values(value: &mut Value) -> bool {
    let mut truncated = false;
    match value {
        Value::String(s) if s.len() > 8192 => {
            let mut end = 8192;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            truncated = true;
        }
        Value::Array(values) => {
            if values.len() > 1000 {
                values.truncate(1000);
                truncated = true;
            }
            for value in values {
                truncated |= bound_values(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                truncated |= bound_values(value);
            }
        }
        _ => {}
    }
    truncated
}

#[derive(Debug, Default)]
pub struct Decoder {
    pending: Vec<u8>,
    discard: bool,
}
impl Decoder {
    // JSON Lines: newlines within strings are escaped. An interrupted frame is discarded at EOF.
    pub fn feed(&mut self, bytes: &[u8], now: u64) -> Vec<Report> {
        let mut reports = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if !self.discard && !self.pending.is_empty() {
                    if let Ok(report) = serde_json::from_slice::<Report>(&self.pending) {
                        if report.validate(now).is_ok() {
                            reports.push(report);
                        }
                    }
                }
                self.reset();
            } else if !self.discard {
                if self.pending.len() >= MAX_REPORT_BYTES {
                    self.pending.clear();
                    self.discard = true;
                } else {
                    self.pending.push(*byte);
                }
            }
        }
        reports
    }
    pub fn reset(&mut self) {
        self.pending.clear();
        self.discard = false;
    }
}

#[cfg(test)]
pub(crate) fn fixture(sequence: u64, now: u64) -> Report {
    Report::from_snapshot(
        lens_model::Snapshot::empty("fixture"),
        "00000000-0000-4000-8000-000000000001",
        "boot-fixture",
        sequence,
        now,
    )
    .expect("valid fixture")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_accepts_split_records_and_recovers_after_oversize_or_eof() {
        let report = fixture(1, 1000);
        let mut bytes = serde_json::to_vec(&report).expect("fixture JSON");
        bytes.push(b'\n');
        let mut decoder = Decoder::default();
        let mut received = Vec::new();
        for chunk in bytes.chunks(17) {
            received.extend(decoder.feed(chunk, 1000));
        }
        assert_eq!(received.len(), 1);
        let junk = vec![b'x'; MAX_REPORT_BYTES + 100];
        assert!(decoder.feed(&junk, 1000).is_empty());
        decoder.feed(b"\n", 1000);
        assert_eq!(decoder.feed(&bytes, 1000).len(), 1);
        decoder.feed(b"{broken", 1000);
        decoder.reset();
        assert_eq!(decoder.feed(&bytes, 1000).len(), 1);
    }
    #[test]
    fn rejects_expired_future_wrong_version_and_unbounded_retention() {
        let mut report = fixture(1, 1000);
        assert!(report.validate(1000 + TTL_MS).is_err());
        assert!(report.validate(0).is_ok());
        report.collected_at_ms = 100_000;
        report.expires_at_ms = 100_000 + TTL_MS;
        assert!(report.validate(0).is_err());
        report = fixture(1, 1000);
        report.protocol_version = 2;
        assert!(report.validate(1000).is_err());
        report = fixture(1, 1000);
        report.expires_at_ms += 1;
        assert!(report.validate(1000).is_err());
    }
    #[test]
    fn truncation_is_bounded_and_never_claims_complete_coverage() {
        let mut snapshot = lens_model::Snapshot::empty("fixture");
        let process: lens_model::Process = serde_json::from_value(serde_json::json!({
            "pid":1,"parent_pid":null,"name":"process","command_line":"a".repeat(20000),
            "executable":null,"user":{"uid":1,"name":null},"state":"running",
            "cpu_percent":0.0,"memory_percent":0.0,"rss_bytes":0,"virtual_memory_bytes":0,
            "threads":1,"io":{"read_bytes":0,"write_bytes":0,"read_bytes_per_second":0.0,"write_bytes_per_second":0.0},
            "runtime_seconds":0,"cgroup":null,"service":null,"container":null,
            "file_descriptor_count":null,"child_pids":[],"unavailable_fields":[],"start_time_ticks":10
        })).expect("typed process fixture");
        snapshot.processes = vec![process; 2000];
        let report = Report::from_snapshot(
            snapshot,
            "00000000-0000-4000-8000-000000000001",
            "boot",
            1,
            1000,
        )
        .expect("bounded report");
        assert!(serde_json::to_vec(&report).expect("JSON").len() <= MAX_REPORT_BYTES);
        assert!(report.coverage.values().all(|value| value == "partial"));
        assert!(
            report.snapshot["processes"]
                .as_array()
                .expect("processes")
                .len()
                < 1000
        );
    }

    #[test]
    fn unknown_additive_fields_are_ignored() {
        let mut value = serde_json::to_value(fixture(1, 1000)).expect("JSON");
        value["future"] = serde_json::json!({"optional":true});
        let report: Report = serde_json::from_value(value).expect("additive fixture");
        assert!(report.validate(1000).is_ok());
    }
}
