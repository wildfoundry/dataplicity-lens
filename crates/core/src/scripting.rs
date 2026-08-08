//! Shared scripting helpers: match mode, field projection, and assertion policy.

use std::{error::Error, fmt};

use clap::ValueEnum;
use lens_model::{Severity, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ASSERTION: i32 = 3;

/// CLI validation or ambiguous action targeting before collection/mutation.
#[derive(Debug, Clone)]
pub struct UsageError {
    pub message: String,
}

impl UsageError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for UsageError {}

/// Opt-in assertion / expect policy failed after a successful collection.
#[derive(Debug, Clone)]
pub struct AssertionError {
    pub message: String,
}

impl AssertionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AssertionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for AssertionError {}

pub fn exit_code_from_error(error: &(dyn Error + 'static)) -> i32 {
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(err) = current {
        if err.downcast_ref::<UsageError>().is_some() {
            return EXIT_USAGE;
        }
        if err.downcast_ref::<AssertionError>().is_some() {
            return EXIT_ASSERTION;
        }
        current = err.source();
    }
    EXIT_FAILURE
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    Contains,
    Exact,
}

impl MatchMode {
    pub fn matches(self, haystack: &str, needle: &str) -> bool {
        let haystack = haystack.to_ascii_lowercase();
        let needle = needle.to_ascii_lowercase();
        match self {
            Self::Contains => haystack.contains(&needle),
            Self::Exact => haystack == needle,
        }
    }
}

/// Domain whose filtered row count drives `--fail-if-*` / `--expect-count*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryDomain {
    Processes,
    Services,
    Containers,
    Logs,
    Mounts,
    Sockets,
    HardwareDevices,
    Findings,
    SystemRows,
    CockpitProcesses,
}

impl PrimaryDomain {
    pub fn count(self, snapshot: &Snapshot) -> usize {
        match self {
            Self::Processes | Self::CockpitProcesses => snapshot.processes.len(),
            Self::Services => snapshot.services.len(),
            Self::Containers => snapshot.containers.len(),
            Self::Logs => snapshot.logs.len(),
            Self::Mounts => snapshot.mounts.len(),
            Self::Sockets => snapshot.sockets.len(),
            Self::HardwareDevices => snapshot.hardware_devices.len(),
            Self::Findings => snapshot.findings.len(),
            Self::SystemRows => {
                snapshot.accounts.len() + snapshot.groups.len() + snapshot.certificates.len()
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Processes | Self::CockpitProcesses => "processes",
            Self::Services => "services",
            Self::Containers => "containers",
            Self::Logs => "logs",
            Self::Mounts => "mounts",
            Self::Sockets => "sockets",
            Self::HardwareDevices => "hardware_devices",
            Self::Findings => "findings",
            Self::SystemRows => "system rows",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssertionPolicy {
    pub fail_if_empty: bool,
    pub fail_if_any: bool,
    pub expect_count: Option<usize>,
    pub expect_count_min: Option<usize>,
    pub expect_count_max: Option<usize>,
    pub fail_on: Option<FailOnSeverity>,
    pub fail_on_collection_warnings: bool,
}

impl AssertionPolicy {
    pub fn is_active(&self) -> bool {
        self.fail_if_empty
            || self.fail_if_any
            || self.expect_count.is_some()
            || self.expect_count_min.is_some()
            || self.expect_count_max.is_some()
            || self.fail_on.is_some()
            || self.fail_on_collection_warnings
    }

    pub fn validate(&self) -> Result<(), UsageError> {
        if self.fail_if_empty && self.fail_if_any {
            return Err(UsageError::new(
                "--fail-if-empty and --fail-if-any cannot be combined",
            ));
        }
        if let (Some(min), Some(max)) = (self.expect_count_min, self.expect_count_max)
            && min > max
        {
            return Err(UsageError::new(
                "--expect-count-min cannot be greater than --expect-count-max",
            ));
        }
        if let (Some(exact), Some(min)) = (self.expect_count, self.expect_count_min)
            && exact < min
        {
            return Err(UsageError::new(
                "--expect-count conflicts with --expect-count-min",
            ));
        }
        if let (Some(exact), Some(max)) = (self.expect_count, self.expect_count_max)
            && exact > max
        {
            return Err(UsageError::new(
                "--expect-count conflicts with --expect-count-max",
            ));
        }
        Ok(())
    }

    pub fn evaluate(
        &self,
        snapshot: &Snapshot,
        domain: PrimaryDomain,
    ) -> Result<(), AssertionError> {
        if !self.is_active() {
            return Ok(());
        }
        let count = domain.count(snapshot);
        let label = domain.label();
        if self.fail_if_empty && count == 0 {
            return Err(AssertionError::new(format!(
                "assertion failed: no matching {label}"
            )));
        }
        if self.fail_if_any && count > 0 {
            return Err(AssertionError::new(format!(
                "assertion failed: found {count} matching {label}"
            )));
        }
        if let Some(expected) = self.expect_count
            && count != expected
        {
            return Err(AssertionError::new(format!(
                "assertion failed: expected {expected} {label}, found {count}"
            )));
        }
        if let Some(minimum) = self.expect_count_min
            && count < minimum
        {
            return Err(AssertionError::new(format!(
                "assertion failed: expected at least {minimum} {label}, found {count}"
            )));
        }
        if let Some(maximum) = self.expect_count_max
            && count > maximum
        {
            return Err(AssertionError::new(format!(
                "assertion failed: expected at most {maximum} {label}, found {count}"
            )));
        }
        if self.fail_on_collection_warnings && !snapshot.collection_warnings.is_empty() {
            return Err(AssertionError::new(format!(
                "assertion failed: {} collection warning(s)",
                snapshot.collection_warnings.len()
            )));
        }
        if let Some(threshold) = self.fail_on {
            let offenders: Vec<_> = snapshot
                .findings
                .iter()
                .filter(|finding| threshold.matches(finding.severity))
                .map(|finding| finding.id.as_str())
                .collect();
            if !offenders.is_empty() {
                return Err(AssertionError::new(format!(
                    "assertion failed: {} finding(s) at or above {}: {}",
                    offenders.len(),
                    threshold.label(),
                    offenders.join(", ")
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOnSeverity {
    Warning,
    Critical,
}

impl FailOnSeverity {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn matches(self, severity: Severity) -> bool {
        match self {
            // "warning" maps to Attention and above (Lens severity names).
            Self::Warning => matches!(severity, Severity::Attention | Severity::Critical),
            Self::Critical => matches!(severity, Severity::Critical),
        }
    }
}

/// Top-level snapshot fields that may appear in `--fields`.
pub const PROJECTABLE_FIELDS: &[&str] = &[
    "processes",
    "services",
    "containers",
    "log_sources",
    "logs",
    "mounts",
    "filesystems",
    "deleted_open_files",
    "block_devices",
    "interfaces",
    "routes",
    "sockets",
    "cellular_modems",
    "clock",
    "dns",
    "certificates",
    "accounts",
    "groups",
    "hardware",
    "temperatures",
    "hardware_devices",
    "findings",
    "relationships",
    "build",
    "collection_warnings",
];

pub fn parse_fields_list(raw: &str) -> Result<Vec<String>, UsageError> {
    let mut fields = Vec::new();
    for part in raw.split(',') {
        let field = part.trim();
        if field.is_empty() {
            continue;
        }
        if matches!(
            field,
            "schema_version" | "generated_at" | "host" | "schema" | "hostname"
        ) {
            // Always retained; ignore if repeated in the list.
            continue;
        }
        if !PROJECTABLE_FIELDS.contains(&field) {
            return Err(UsageError::new(format!(
                "unknown --fields entry '{field}'; allowed: {}",
                PROJECTABLE_FIELDS.join(", ")
            )));
        }
        if !fields.iter().any(|existing| existing == field) {
            fields.push(field.to_owned());
        }
    }
    if fields.is_empty() {
        return Err(UsageError::new(
            "--fields requires at least one projectable snapshot field",
        ));
    }
    Ok(fields)
}

pub fn project_snapshot_value(
    snapshot: &Snapshot,
    fields: &[String],
) -> Result<Value, serde_json::Error> {
    let full = serde_json::to_value(snapshot)?;
    let Value::Object(mut map) = full else {
        return Ok(full);
    };
    let mut projected = Map::new();
    for key in ["schema_version", "generated_at", "host"] {
        if let Some(value) = map.remove(key) {
            projected.insert(key.to_owned(), value);
        }
    }
    for field in fields {
        if let Some(value) = map.remove(field.as_str()) {
            projected.insert(field.clone(), value);
        }
    }
    Ok(Value::Object(projected))
}

#[cfg(test)]
mod tests {
    use lens_model::{
        Finding, Host, LoadAverage, Memory, ProcessCounts, SchemaVersion, Snapshot, Timestamp,
    };

    use super::*;

    fn empty_snapshot() -> Snapshot {
        Snapshot {
            schema_version: SchemaVersion::default(),
            generated_at: Timestamp::now(),
            host: Host {
                hostname: "demo".into(),
                kernel: "test".into(),
                os_name: None,
                uptime_seconds: 1,
                cpu_count: 1,
                cpu_percent: 0.0,
                load: LoadAverage::default(),
                memory: Memory::default(),
                process_counts: ProcessCounts::default(),
                refresh_interval_ms: 1000,
                total_cpu_ticks: 0,
                idle_cpu_ticks: 0,
            },
            processes: Vec::new(),
            services: Vec::new(),
            containers: Vec::new(),
            containers_runtime_live: false,
            log_sources: Vec::new(),
            logs: Vec::new(),
            mounts: Vec::new(),
            filesystems: Vec::new(),
            deleted_open_files: Vec::new(),
            block_devices: Vec::new(),
            interfaces: Vec::new(),
            routes: Vec::new(),
            sockets: Vec::new(),
            cellular_modems: Vec::new(),
            clock: Default::default(),
            dns: Default::default(),
            certificates: Vec::new(),
            accounts: Vec::new(),
            groups: Vec::new(),
            hardware: Default::default(),
            temperatures: Vec::new(),
            hardware_devices: Vec::new(),
            findings: Vec::new(),
            relationships: Vec::new(),
            build: None,
            collection_warnings: Vec::new(),
        }
    }

    #[test]
    fn fail_if_empty_triggers() {
        let policy = AssertionPolicy {
            fail_if_empty: true,
            ..AssertionPolicy::default()
        };
        let error = policy
            .evaluate(&empty_snapshot(), PrimaryDomain::Services)
            .expect_err("empty");
        assert!(error.message.contains("no matching services"));
    }

    #[test]
    fn fail_on_critical_findings() {
        let mut snapshot = empty_snapshot();
        snapshot.findings.push(Finding {
            id: "disk-full".into(),
            severity: Severity::Critical,
            title: "Disk".into(),
            summary: "full".into(),
            evidence: Vec::new(),
            related_entities: Vec::new(),
            suggested_actions: Vec::new(),
        });
        let policy = AssertionPolicy {
            fail_on: Some(FailOnSeverity::Critical),
            ..AssertionPolicy::default()
        };
        assert!(policy.evaluate(&snapshot, PrimaryDomain::Findings).is_err());
    }

    #[test]
    fn parses_fields_and_rejects_unknown() {
        let fields = parse_fields_list("services, findings").expect("fields");
        assert_eq!(fields, vec!["services".to_owned(), "findings".to_owned()]);
        assert!(parse_fields_list("nope").is_err());
    }
}
