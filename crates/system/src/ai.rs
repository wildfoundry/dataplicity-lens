use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use lens_model::{AiDiagnostics, AiUnavailableField, AiUnavailableReason, Timestamp};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

pub const DEFAULT_AGENT_STATE_PATH: &str = "/run/dataplicity/agent/ai-state.json";
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;
const STALE_AFTER: Duration = Duration::minutes(5);

pub fn collect(path: Option<&Path>) -> AiDiagnostics {
    let path = path.unwrap_or_else(|| Path::new(DEFAULT_AGENT_STATE_PATH));
    match read_state(path) {
        Ok(mut state) => {
            if state.source.is_empty() {
                state.source = path.display().to_string();
            }
            mark_stale(&mut state);
            state
        }
        Err(error) => unavailable(path, error),
    }
}

fn read_state(path: &Path) -> io::Result<AiDiagnostics> {
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state exceeds {MAX_STATE_BYTES} bytes"),
        ));
    }
    let mut contents = String::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_string(&mut contents)?;
    serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn unavailable(path: &Path, error: io::Error) -> AiDiagnostics {
    let reason = match error.kind() {
        io::ErrorKind::NotFound => AiUnavailableReason::Absent,
        io::ErrorKind::PermissionDenied => AiUnavailableReason::PermissionDenied,
        io::ErrorKind::InvalidData => AiUnavailableReason::Malformed,
        _ => AiUnavailableReason::NotReported,
    };
    AiDiagnostics {
        source: path.display().to_string(),
        unavailable: vec![AiUnavailableField {
            field: "agent_state".into(),
            reason,
            detail: Some(error.to_string()),
        }],
        ..AiDiagnostics::default()
    }
}

fn mark_stale(state: &mut AiDiagnostics) {
    let Some(Timestamp(observed_at)) = &state.observed_at else {
        state.unavailable.push(AiUnavailableField {
            field: "observed_at".into(),
            reason: AiUnavailableReason::NotReported,
            detail: Some("agent state did not include an observation timestamp".into()),
        });
        return;
    };
    let Ok(observed_at) = OffsetDateTime::parse(observed_at, &Rfc3339) else {
        state.unavailable.push(AiUnavailableField {
            field: "observed_at".into(),
            reason: AiUnavailableReason::Malformed,
            detail: Some("timestamp is not RFC 3339".into()),
        });
        return;
    };
    if OffsetDateTime::now_utc() - observed_at > STALE_AFTER
        && !state
            .unavailable
            .iter()
            .any(|item| item.reason == AiUnavailableReason::Stale)
    {
        state.unavailable.push(AiUnavailableField {
            field: "agent_state".into(),
            reason: AiUnavailableReason::Stale,
            detail: Some(format!("last observed at {observed_at}")),
        });
    }
}

pub fn validate_override(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("--agent-state must be an absolute path".into());
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn absent_state_has_explicit_reason() {
        let path = Path::new("/definitely/not/a/lens-agent-state.json");
        let state = collect(Some(path));
        assert_eq!(state.unavailable[0].reason, AiUnavailableReason::Absent);
        assert_eq!(state.unavailable[0].field, "agent_state");
    }

    #[test]
    fn parses_authoritative_state_without_deriving_a_second_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("lens-ai-{unique}.json"));
        fs::write(
            &path,
            r#"{
              "source":"dataplicity-agent",
              "observed_at":"2999-01-01T00:00:00Z",
              "accelerators":[{"stable_id":"pci-0000:01:00.0","driver_version":"550.1"}],
              "model_store":{"desired":"vision-v4","current":"vision-v3"},
              "runtimes":[{
                "id":"inference-main",
                "active_model":"vision-v4",
                "loaded_model":"vision-v3",
                "input_age_seconds":12,
                "queue_depth":2
              }]
            }"#,
        )
        .expect("fixture");
        let state = collect(Some(&path));
        let _ = fs::remove_file(path);
        assert_eq!(state.source, "dataplicity-agent");
        assert_eq!(state.accelerators[0].stable_id, "pci-0000:01:00.0");
        assert!(state.runtimes[0].active_loaded_diverge());
    }

    #[test]
    fn override_must_be_absolute() {
        assert!(validate_override(Path::new("relative.json")).is_err());
    }
}
