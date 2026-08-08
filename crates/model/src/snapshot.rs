use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{Cgroup, ContainerReference, Finding, ProcessId, Relationship, ServiceReference, User};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(pub String);

impl Default for SchemaVersion {
    fn default() -> Self {
        Self("2".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub String);

impl Timestamp {
    pub fn now() -> Self {
        let value = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

impl Memory {
    pub fn used_percent(self) -> f64 {
        percent(self.used_bytes, self.total_bytes)
    }

    pub fn swap_used_percent(self) -> f64 {
        percent(self.swap_used_bytes, self.swap_total_bytes)
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64) * 100.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCounts {
    pub total: usize,
    pub running: usize,
    pub sleeping: usize,
    pub stopped: usize,
    pub zombie: usize,
    pub other: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub hostname: String,
    pub kernel: String,
    pub os_name: Option<String>,
    pub uptime_seconds: u64,
    pub cpu_count: usize,
    pub cpu_percent: f64,
    pub load: LoadAverage,
    pub memory: Memory,
    pub process_counts: ProcessCounts,
    pub refresh_interval_ms: u64,
    #[serde(skip)]
    pub total_cpu_ticks: u64,
    #[serde(skip)]
    pub idle_cpu_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    TracingStop,
    Zombie,
    Dead,
    Idle,
    Unknown,
}

impl ProcessState {
    pub const fn short(self) -> &'static str {
        match self {
            Self::Running => "R",
            Self::Sleeping => "S",
            Self::DiskSleep => "D",
            Self::Stopped => "T",
            Self::TracingStop => "t",
            Self::Zombie => "Z",
            Self::Dead => "X",
            Self::Idle => "I",
            Self::Unknown => "?",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Sleeping => "sleeping",
            Self::DiskSleep => "disk sleep",
            Self::Stopped => "stopped",
            Self::TracingStop => "tracing stop",
            Self::Zombie => "zombie",
            Self::Dead => "dead",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct IoCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_bytes_per_second: f64,
    pub write_bytes_per_second: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Process {
    pub pid: ProcessId,
    pub parent_pid: Option<ProcessId>,
    pub name: String,
    pub command_line: Option<String>,
    pub executable: Option<String>,
    pub user: User,
    pub state: ProcessState,
    pub cpu_percent: f64,
    pub memory_percent: f64,
    pub rss_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub threads: u32,
    pub io: IoCounters,
    pub runtime_seconds: u64,
    pub cgroup: Option<Cgroup>,
    pub service: Option<ServiceReference>,
    pub container: Option<ContainerReference>,
    pub file_descriptor_count: Option<u64>,
    pub child_pids: Vec<ProcessId>,
    pub unavailable_fields: Vec<String>,
    #[serde(skip)]
    pub cpu_time_ticks: u64,
    #[serde(skip)]
    pub start_time_ticks: u64,
}

impl Process {
    pub fn identity(&self) -> (ProcessId, u64) {
        (self.pid, self.start_time_ticks)
    }

    pub fn display_command(&self) -> &str {
        self.command_line.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    pub timestamp: Timestamp,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSeries {
    pub name: String,
    pub unit: String,
    pub points: Vec<MetricPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub target: String,
    pub built_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_count: Option<u64>,
}

/// Inventory row for a Docker or Podman container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub runtime: String,
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    pub state: String,
    pub created: String,
    pub ports: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSource {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub source: String,
    pub unit: Option<String>,
    pub priority: Option<String>,
    pub message: String,
    pub repeated: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
    pub inode_total: Option<u64>,
    pub inode_used: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filesystem {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedOpenFile {
    pub pid: Option<ProcessId>,
    pub command: String,
    pub path: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDevice {
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
    pub filesystem: Option<String>,
    pub mountpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub state: String,
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub destination: String,
    pub gateway: Option<String>,
    pub interface: Option<String>,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Socket {
    pub id: String,
    pub protocol: String,
    pub state: String,
    pub local: String,
    pub peer: String,
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<ProcessId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellularSim {
    pub path: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iccid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellularModem {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: String,
    pub access_technologies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_quality_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim: Option<CellularSim>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockContext {
    pub timezone: Option<String>,
    pub ntp_synchronized: Option<bool>,
    pub ntp_service: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsContext {
    pub source: String,
    pub nameservers: Vec<String>,
    pub search_domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateInfo {
    pub path: String,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub not_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupInfo {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareIdentity {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub board: Option<String>,
    pub board_revision: Option<String>,
    pub serial_number: Option<String>,
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raspberry_pi: Option<RaspberryPiStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaspberryPiStatus {
    pub throttled_raw: Option<u32>,
    #[serde(default)]
    pub active_conditions: Vec<String>,
    #[serde(default)]
    pub historical_conditions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemperatureSensor {
    pub name: String,
    pub source: String,
    pub temperature_c: f64,
    pub max_c: Option<f64>,
    pub critical_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareDevice {
    pub kind: String,
    pub name: String,
    pub path: String,
    pub manufacturer: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub serial_number: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: SchemaVersion,
    pub generated_at: Timestamp,
    pub host: Host,
    pub processes: Vec<Process>,
    pub services: Vec<Service>,
    #[serde(default)]
    pub containers: Vec<Container>,
    /// True when at least one container runtime (docker/podman) responded as live.
    /// Not serialized; used for cockpit summary when inventory is empty.
    #[serde(skip)]
    pub containers_runtime_live: bool,
    pub log_sources: Vec<LogSource>,
    pub logs: Vec<LogEntry>,
    pub mounts: Vec<Mount>,
    pub filesystems: Vec<Filesystem>,
    pub deleted_open_files: Vec<DeletedOpenFile>,
    #[serde(default)]
    pub block_devices: Vec<BlockDevice>,
    pub interfaces: Vec<Interface>,
    pub routes: Vec<Route>,
    pub sockets: Vec<Socket>,
    #[serde(default)]
    pub cellular_modems: Vec<CellularModem>,
    #[serde(default)]
    pub clock: ClockContext,
    #[serde(default)]
    pub dns: DnsContext,
    #[serde(default)]
    pub certificates: Vec<CertificateInfo>,
    #[serde(default)]
    pub accounts: Vec<AccountInfo>,
    #[serde(default)]
    pub groups: Vec<GroupInfo>,
    #[serde(default)]
    pub hardware: HardwareIdentity,
    #[serde(default)]
    pub temperatures: Vec<TemperatureSensor>,
    #[serde(default)]
    pub hardware_devices: Vec<HardwareDevice>,
    pub findings: Vec<Finding>,
    pub relationships: Vec<Relationship>,
    pub build: Option<BuildInfo>,
    pub collection_warnings: Vec<String>,
}

impl Snapshot {
    pub fn empty(hostname: impl Into<String>) -> Self {
        Self {
            schema_version: SchemaVersion::default(),
            generated_at: Timestamp::now(),
            host: Host {
                hostname: hostname.into(),
                kernel: String::new(),
                os_name: None,
                uptime_seconds: 0,
                cpu_count: 0,
                cpu_percent: 0.0,
                load: LoadAverage::default(),
                memory: Memory::default(),
                process_counts: ProcessCounts::default(),
                refresh_interval_ms: 1_000,
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
            clock: ClockContext::default(),
            dns: DnsContext::default(),
            certificates: Vec::new(),
            accounts: Vec::new(),
            groups: Vec::new(),
            hardware: HardwareIdentity::default(),
            temperatures: Vec::new(),
            hardware_devices: Vec::new(),
            findings: Vec::new(),
            relationships: Vec::new(),
            build: None,
            collection_warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_percent_handles_zero_total() {
        assert_eq!(Memory::default().used_percent(), 0.0);
    }

    #[test]
    fn schema_defaults_to_current_version() {
        assert_eq!(SchemaVersion::default().0, "2");
    }
}
