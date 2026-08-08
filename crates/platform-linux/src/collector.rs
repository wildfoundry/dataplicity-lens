use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use lens_model::{
    Cgroup, ContainerReference, EntityId, Host, IoCounters, Process, ProcessCounts, ProcessId,
    Relationship, RelationshipKind, SchemaVersion, ServiceReference, Snapshot, Timestamp, User,
};
use thiserror::Error;
use tracing::debug;

use crate::proc_parse::{
    ParseError, parse_loadavg, parse_meminfo, parse_pid_stat, parse_proc_stat,
};

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("required Linux interface {path} could not be read: {source}")]
    RequiredRead { path: PathBuf, source: io::Error },
    #[error("required Linux interface {path} was malformed: {source}")]
    RequiredParse { path: PathBuf, source: ParseError },
}

#[derive(Debug)]
pub struct LinuxCollector {
    proc_root: PathBuf,
    etc_root: PathBuf,
    refresh_interval: Duration,
    uid_names: HashMap<u32, Option<String>>,
    clock_ticks: u64,
    page_size: u64,
}

impl Default for LinuxCollector {
    fn default() -> Self {
        Self::new("/proc", "/etc")
    }
}

impl LinuxCollector {
    pub fn new(proc_root: impl Into<PathBuf>, etc_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            etc_root: etc_root.into(),
            refresh_interval: Duration::from_secs(1),
            uid_names: HashMap::new(),
            clock_ticks: clock_ticks(),
            page_size: page_size(),
        }
    }

    pub fn set_refresh_interval(&mut self, interval: Duration) {
        self.refresh_interval = interval;
    }

    pub fn collect(&mut self) -> Result<Snapshot, CollectError> {
        let stat_path = self.proc_root.join("stat");
        let stat_text = read_required(&stat_path)?;
        let cpu = parse_proc_stat(&stat_text).map_err(|source| CollectError::RequiredParse {
            path: stat_path,
            source,
        })?;

        let mem_path = self.proc_root.join("meminfo");
        let memory = parse_meminfo(&read_required(&mem_path)?).map_err(|source| {
            CollectError::RequiredParse {
                path: mem_path,
                source,
            }
        })?;

        let mut warnings = Vec::new();
        let hostname = read_optional_trimmed(self.proc_root.join("sys/kernel/hostname"))
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| "unknown-host".to_owned());
        let kernel = read_optional_trimmed(self.proc_root.join("sys/kernel/osrelease"))
            .unwrap_or_else(|| "unknown".to_owned());
        let os_name = read_os_name(&self.etc_root.join("os-release"));
        let uptime_seconds = read_optional_trimmed(self.proc_root.join("uptime"))
            .and_then(|value| {
                value
                    .split_whitespace()
                    .next()
                    .and_then(|item| item.parse::<f64>().ok())
            })
            .map_or(0, |value| value.max(0.0) as u64);
        let load = match read_optional_trimmed(self.proc_root.join("loadavg")) {
            Some(value) => parse_loadavg(&value).unwrap_or_default(),
            None => Default::default(),
        };

        let mut processes = Vec::new();
        let entries =
            fs::read_dir(&self.proc_root).map_err(|source| CollectError::RequiredRead {
                path: self.proc_root.clone(),
                source,
            })?;
        let mut permission_limited = 0usize;
        let mut malformed = 0usize;
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            match self.collect_process(pid, memory.total_bytes, uptime_seconds) {
                Ok(Some(process)) => processes.push(process),
                Ok(None) => {}
                Err(ProcessCollectError::Permission) => permission_limited += 1,
                Err(ProcessCollectError::Malformed) => malformed += 1,
            }
        }

        processes.sort_by_key(|process| process.pid);
        attach_children(&mut processes);
        let process_counts = count_states(&processes);
        if permission_limited > 0 {
            warnings.push(format!(
                "details for {permission_limited} processes were unreadable due to permissions"
            ));
        }
        if malformed > 0 {
            warnings.push(format!(
                "{malformed} process entries changed or were malformed during collection"
            ));
        }

        let relationships = relationships(&hostname, &processes);
        Ok(Snapshot {
            schema_version: SchemaVersion::default(),
            generated_at: Timestamp::now(),
            host: Host {
                hostname,
                kernel,
                os_name,
                uptime_seconds,
                cpu_count: cpu.cpu_count,
                cpu_percent: 0.0,
                load,
                memory,
                process_counts,
                refresh_interval_ms: self
                    .refresh_interval
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                total_cpu_ticks: cpu.total_ticks,
                idle_cpu_ticks: cpu.idle_ticks,
            },
            processes,
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
            relationships,
            build: None,
            collection_warnings: warnings,
        })
    }

    fn collect_process(
        &mut self,
        pid: u32,
        memory_total: u64,
        uptime_seconds: u64,
    ) -> Result<Option<Process>, ProcessCollectError> {
        let root = self.proc_root.join(pid.to_string());
        let stat_text = match fs::read_to_string(root.join("stat")) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(ProcessCollectError::Permission);
            }
            Err(_) => return Err(ProcessCollectError::Malformed),
        };
        let stat = parse_pid_stat(&stat_text).map_err(|_| ProcessCollectError::Malformed)?;
        let mut unavailable = Vec::new();
        let status = match fs::read_to_string(root.join("status")) {
            Ok(value) => parse_status(&value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                if error.kind() == io::ErrorKind::PermissionDenied {
                    unavailable.push("status (permission denied)".to_owned());
                } else {
                    unavailable.push("status".to_owned());
                }
                StatusData::default()
            }
        };
        let uid = status.uid.unwrap_or(u32::MAX);
        let user_name = if uid == u32::MAX {
            None
        } else {
            self.resolve_user(uid)
        };
        let command_line = read_cmdline(&root.join("cmdline"), &mut unavailable);
        let executable = match fs::read_link(root.join("exe")) {
            Ok(path) => Some(path.to_string_lossy().into_owned()),
            Err(error) => {
                if error.kind() == io::ErrorKind::PermissionDenied {
                    unavailable.push("executable (permission denied)".to_owned());
                }
                None
            }
        };
        let io = read_io(&root.join("io"), &mut unavailable);
        let cgroup = read_cgroup(&root.join("cgroup"), &mut unavailable);
        let service = cgroup.as_ref().and_then(infer_service);
        let container = cgroup.as_ref().and_then(infer_container);
        let file_descriptor_count = match fs::read_dir(root.join("fd")) {
            Ok(entries) => Some(entries.filter_map(Result::ok).count() as u64),
            Err(error) => {
                if error.kind() == io::ErrorKind::PermissionDenied {
                    unavailable.push("file descriptors (permission denied)".to_owned());
                }
                None
            }
        };
        let rss_pages = stat.rss_pages.max(0) as u64;
        let rss_bytes = status
            .rss_bytes
            .unwrap_or(rss_pages.saturating_mul(self.page_size));
        let virtual_memory_bytes = status
            .virtual_memory_bytes
            .unwrap_or(stat.virtual_memory_bytes);
        let memory_percent = if memory_total == 0 {
            0.0
        } else {
            rss_bytes as f64 / memory_total as f64 * 100.0
        };
        let started_seconds = stat.start_time_ticks / self.clock_ticks;
        let runtime_seconds = uptime_seconds.saturating_sub(started_seconds);
        Ok(Some(Process {
            pid: ProcessId(stat.pid),
            parent_pid: (stat.parent_pid > 0).then_some(ProcessId(stat.parent_pid)),
            name: status.name.unwrap_or(stat.name),
            command_line,
            executable,
            user: User {
                uid,
                name: user_name,
            },
            state: stat.state,
            cpu_percent: 0.0,
            memory_percent,
            rss_bytes,
            virtual_memory_bytes,
            threads: status.threads.unwrap_or(stat.threads),
            io,
            runtime_seconds,
            cgroup,
            service,
            container,
            file_descriptor_count,
            child_pids: Vec::new(),
            unavailable_fields: unavailable,
            cpu_time_ticks: stat.user_ticks.saturating_add(stat.system_ticks),
            start_time_ticks: stat.start_time_ticks,
        }))
    }

    fn resolve_user(&mut self, uid: u32) -> Option<String> {
        if let Some(name) = self.uid_names.get(&uid) {
            return name.clone();
        }
        let name = fs::read_to_string(self.etc_root.join("passwd"))
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    let fields: Vec<&str> = line.split(':').collect();
                    (fields.len() >= 3 && fields[2].parse::<u32>().ok() == Some(uid))
                        .then(|| fields[0].to_owned())
                })
            });
        self.uid_names.insert(uid, name.clone());
        name
    }
}

#[cfg(target_os = "linux")]
fn clock_ticks() -> u64 {
    procfs::ticks_per_second().max(1)
}

#[cfg(not(target_os = "linux"))]
const fn clock_ticks() -> u64 {
    100
}

#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    procfs::page_size().max(1)
}

#[cfg(not(target_os = "linux"))]
const fn page_size() -> u64 {
    4_096
}

#[derive(Debug, Clone, Copy)]
enum ProcessCollectError {
    Permission,
    Malformed,
}

#[derive(Debug, Default)]
struct StatusData {
    name: Option<String>,
    uid: Option<u32>,
    threads: Option<u32>,
    rss_bytes: Option<u64>,
    virtual_memory_bytes: Option<u64>,
}

fn parse_status(input: &str) -> StatusData {
    let mut result = StatusData::default();
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Name" => result.name = Some(value.to_owned()),
            "Uid" => {
                result.uid = value
                    .split_whitespace()
                    .next()
                    .and_then(|item| item.parse().ok())
            }
            "Threads" => result.threads = value.parse().ok(),
            "VmRSS" => result.rss_bytes = parse_kib(value),
            "VmSize" => result.virtual_memory_bytes = parse_kib(value),
            _ => {}
        }
    }
    result
}

fn parse_kib(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()
        .and_then(|item| item.parse::<u64>().ok())
        .map(|value| value.saturating_mul(1024))
}

fn read_cmdline(path: &Path, unavailable: &mut Vec<String>) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => {
            let parts: Vec<String> = bytes
                .split(|byte| *byte == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied {
                unavailable.push("command line (permission denied)".to_owned());
            } else {
                unavailable.push("command line".to_owned());
            }
            None
        }
    }
}

fn read_io(path: &Path, unavailable: &mut Vec<String>) -> IoCounters {
    let contents = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied {
                unavailable.push("I/O counters (permission denied)".to_owned());
            }
            return IoCounters::default();
        }
    };
    let mut result = IoCounters::default();
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let parsed = value.trim().parse().unwrap_or_default();
            match key {
                "read_bytes" => result.read_bytes = parsed,
                "write_bytes" => result.write_bytes = parsed,
                _ => {}
            }
        }
    }
    result
}

fn read_cgroup(path: &Path, unavailable: &mut Vec<String>) -> Option<Cgroup> {
    let contents = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied {
                unavailable.push("cgroup (permission denied)".to_owned());
            }
            return None;
        }
    };
    contents
        .lines()
        .filter_map(|line| line.rsplit_once(':').map(|(_, path)| path.trim()))
        .max_by_key(|path| path.len())
        .filter(|path| !path.is_empty())
        .map(|path| Cgroup {
            path: path.to_owned(),
        })
}

fn infer_service(cgroup: &Cgroup) -> Option<ServiceReference> {
    cgroup
        .path
        .split('/')
        .find(|part| part.ends_with(".service"))
        .map(|name| ServiceReference {
            name: name.to_owned(),
            inferred: true,
        })
}

fn infer_container(cgroup: &Cgroup) -> Option<ContainerReference> {
    let runtime = if cgroup.path.contains("docker") {
        Some("docker".to_owned())
    } else if cgroup.path.contains("libpod") {
        Some("podman".to_owned())
    } else if cgroup.path.contains("containerd") || cgroup.path.contains("cri-containerd") {
        Some("containerd".to_owned())
    } else {
        None
    };
    let id = cgroup.path.split('/').rev().find_map(|part| {
        let trimmed = part
            .trim_end_matches(".scope")
            .trim_start_matches("docker-")
            .trim_start_matches("libpod-")
            .trim_start_matches("cri-containerd-");
        (trimmed.len() >= 12
            && trimmed
                .chars()
                .all(|character| character.is_ascii_hexdigit()))
        .then(|| trimmed.to_owned())
    })?;
    Some(ContainerReference {
        runtime,
        id,
        inferred: true,
    })
}

fn read_required(path: &Path) -> Result<String, CollectError> {
    fs::read_to_string(path).map_err(|source| CollectError::RequiredRead {
        path: path.to_owned(),
        source,
    })
}

fn read_optional_trimmed(path: PathBuf) -> Option<String> {
    match fs::read_to_string(&path) {
        Ok(value) => Some(value.trim().to_owned()),
        Err(error) => {
            debug!(path = %path.display(), %error, "optional Linux interface unavailable");
            None
        }
    }
}

fn read_os_name(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|contents| {
        contents.lines().find_map(|line| {
            line.strip_prefix("PRETTY_NAME=")
                .map(|value| value.trim_matches('"').to_owned())
        })
    })
}

fn attach_children(processes: &mut [Process]) {
    let by_pid: HashMap<ProcessId, usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.pid, index))
        .collect();
    let pairs: Vec<(ProcessId, ProcessId)> = processes
        .iter()
        .filter_map(|process| process.parent_pid.map(|parent| (parent, process.pid)))
        .collect();
    for (parent, child) in pairs {
        if parent == child {
            continue;
        }
        if let Some(index) = by_pid.get(&parent).copied() {
            processes[index].child_pids.push(child);
        }
    }
}

fn count_states(processes: &[Process]) -> ProcessCounts {
    let mut counts = ProcessCounts {
        total: processes.len(),
        ..ProcessCounts::default()
    };
    for process in processes {
        match process.state {
            lens_model::ProcessState::Running => counts.running += 1,
            lens_model::ProcessState::Sleeping | lens_model::ProcessState::DiskSleep => {
                counts.sleeping += 1;
            }
            lens_model::ProcessState::Stopped | lens_model::ProcessState::TracingStop => {
                counts.stopped += 1;
            }
            lens_model::ProcessState::Zombie => counts.zombie += 1,
            _ => counts.other += 1,
        }
    }
    counts
}

fn relationships(hostname: &str, processes: &[Process]) -> Vec<Relationship> {
    let mut values = Vec::new();
    for process in processes {
        let process_id = EntityId::Process {
            pid: process.pid,
            start_ticks: process.start_time_ticks,
        };
        values.push(Relationship {
            from: process_id.clone(),
            to: EntityId::Host(hostname.to_owned()),
            kind: RelationshipKind::FindingOnHost,
        });
        values.push(Relationship {
            from: process_id.clone(),
            to: EntityId::User(process.user.uid),
            kind: RelationshipKind::OwnedByUser,
        });
        if let Some(parent) = process.parent_pid
            && let Some(parent_process) = processes.iter().find(|candidate| candidate.pid == parent)
        {
            values.push(Relationship {
                from: process_id.clone(),
                to: EntityId::Process {
                    pid: parent,
                    start_ticks: parent_process.start_time_ticks,
                },
                kind: RelationshipKind::ParentProcess,
            });
        }
        if let Some(cgroup) = &process.cgroup {
            values.push(Relationship {
                from: process_id.clone(),
                to: EntityId::Cgroup(cgroup.path.clone()),
                kind: RelationshipKind::MemberOfCgroup,
            });
        }
        if let Some(service) = &process.service {
            values.push(Relationship {
                from: process_id.clone(),
                to: EntityId::Service(service.name.clone()),
                kind: RelationshipKind::MemberOfService,
            });
        }
        if let Some(container) = &process.container {
            values.push(Relationship {
                from: process_id,
                to: EntityId::Container(container.id.clone()),
                kind: RelationshipKind::MemberOfContainer,
            });
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parser_handles_partial_data() {
        let value = parse_status("Name:\tworker\nUid:\t1000 1000 1000 1000\nVmRSS:\t42 kB\n");
        assert_eq!(value.uid, Some(1000));
        assert_eq!(value.rss_bytes, Some(42 * 1024));
        assert_eq!(value.threads, None);
    }

    #[test]
    fn infers_systemd_service() {
        let service = infer_service(&Cgroup {
            path: "/system.slice/sshd.service".to_owned(),
        })
        .expect("service should be inferred");
        assert_eq!(service.name, "sshd.service");
    }

    #[test]
    fn committed_host_fixture_collects_deterministically() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut collector = LinuxCollector::new(root.join("proc"), root.join("system"));
        let snapshot = collector.collect().expect("fixture should collect");
        assert_eq!(snapshot.schema_version, SchemaVersion::default());
        assert_eq!(snapshot.host.memory.total_bytes, 8_388_608_000);
        assert_eq!(snapshot.host.cpu_count, 2);
        assert!(snapshot.processes.is_empty());
    }
}
