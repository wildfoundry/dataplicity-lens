#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use lens_model::{
    EntityId, Host, IoCounters, LoadAverage, Memory, Process, ProcessCounts, ProcessId,
    ProcessState, Relationship, RelationshipKind, SchemaVersion, Snapshot, Timestamp, User,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("required macOS command {program} failed: {detail}")]
    Command { program: String, detail: String },
    #[error("required macOS process data was malformed")]
    ProcessData,
}

#[derive(Debug)]
pub struct MacOsCollector {
    refresh_interval: Duration,
    last_sample: Option<Instant>,
    total_cpu_ticks: u64,
    idle_cpu_ticks: u64,
}

impl Default for MacOsCollector {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(1),
            last_sample: None,
            total_cpu_ticks: 0,
            idle_cpu_ticks: 0,
        }
    }
}

impl MacOsCollector {
    pub fn set_refresh_interval(&mut self, interval: Duration) {
        self.refresh_interval = interval;
    }

    pub fn collect(&mut self) -> Result<Snapshot, CollectError> {
        let now = Instant::now();
        let mut warnings = Vec::new();
        let hostname = optional_command("scutil", &["--get", "ComputerName"], &mut warnings)
            .or_else(|| optional_command("hostname", &[], &mut warnings))
            .unwrap_or_else(|| "unknown-host".into());
        let kernel = required_command("uname", &["-r"])?;
        let os_name = optional_command("sw_vers", &["-productName"], &mut warnings).map(|name| {
            let version = optional_command("sw_vers", &["-productVersion"], &mut warnings)
                .unwrap_or_default();
            format!("{name} {version}").trim().to_owned()
        });
        let cpu_count = sysctl_u64("hw.logicalcpu", &mut warnings).unwrap_or(1) as usize;
        let total_memory = sysctl_u64("hw.memsize", &mut warnings).unwrap_or_default();
        let memory = optional_command("vm_stat", &[], &mut warnings)
            .map(|text| parse_vm_stat(&text, total_memory))
            .unwrap_or(Memory {
                total_bytes: total_memory,
                ..Memory::default()
            });
        let uptime_seconds = optional_command("sysctl", &["-n", "kern.boottime"], &mut warnings)
            .and_then(|text| parse_boot_time(&text))
            .map(|boot| unix_seconds().saturating_sub(boot))
            .unwrap_or_default();
        let load = optional_command("sysctl", &["-n", "vm.loadavg"], &mut warnings)
            .map(|text| parse_load(&text))
            .unwrap_or_default();
        let idle_percent = optional_command("top", &["-l", "1", "-n", "0"], &mut warnings)
            .and_then(|text| parse_idle_percent(&text))
            .unwrap_or_default();
        let elapsed_ticks = self.last_sample.map_or(0, |previous| {
            now.duration_since(previous)
                .as_millis()
                .saturating_mul(cpu_count as u128)
                .try_into()
                .unwrap_or(u64::MAX)
        });
        self.total_cpu_ticks = self.total_cpu_ticks.saturating_add(elapsed_ticks);
        self.idle_cpu_ticks = self
            .idle_cpu_ticks
            .saturating_add((elapsed_ticks as f64 * idle_percent / 100.0) as u64);
        self.last_sample = Some(now);

        let ps = required_command(
            "ps",
            &[
                "-axo",
                "pid=,ppid=,uid=,user=,state=,pcpu=,pmem=,rss=,vsz=,time=,etime=,comm=",
            ],
        )?;
        let mut processes: Vec<_> = ps
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(parse_process)
            .collect();
        if processes.is_empty() {
            return Err(CollectError::ProcessData);
        }
        processes.sort_by_key(|process| process.pid);
        attach_children(&mut processes);
        let process_counts = count_states(&processes);
        let relationships = relationships(&hostname, &processes);

        Ok(Snapshot {
            schema_version: SchemaVersion::default(),
            generated_at: Timestamp::now(),
            host: Host {
                hostname,
                kernel,
                os_name,
                uptime_seconds,
                cpu_count,
                cpu_percent: 0.0,
                load,
                memory,
                process_counts,
                refresh_interval_ms: self
                    .refresh_interval
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                total_cpu_ticks: self.total_cpu_ticks,
                idle_cpu_ticks: self.idle_cpu_ticks,
            },
            processes,
            services: Vec::new(),
            log_sources: Vec::new(),
            logs: Vec::new(),
            mounts: Vec::new(),
            filesystems: Vec::new(),
            deleted_open_files: Vec::new(),
            block_devices: Vec::new(),
            interfaces: Vec::new(),
            routes: Vec::new(),
            sockets: Vec::new(),
            findings: Vec::new(),
            relationships,
            build: None,
            collection_warnings: warnings,
        })
    }
}

fn required_command(program: &str, args: &[&str]) -> Result<String, CollectError> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| CollectError::Command {
            program: program.to_owned(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(CollectError::Command {
            program: program.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn optional_command(program: &str, args: &[&str], warnings: &mut Vec<String>) -> Option<String> {
    match required_command(program, args) {
        Ok(value) => Some(value),
        Err(error) => {
            warnings.push(error.to_string());
            None
        }
    }
}

fn sysctl_u64(name: &str, warnings: &mut Vec<String>) -> Option<u64> {
    optional_command("sysctl", &["-n", name], warnings)?
        .parse()
        .ok()
}

fn parse_vm_stat(text: &str, total_bytes: u64) -> Memory {
    let page_size = text
        .lines()
        .next()
        .and_then(|line| line.split("page size of ").nth(1))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4096);
    let mut pages = HashMap::new();
    for line in text.lines().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            let value = value
                .trim()
                .trim_end_matches('.')
                .parse::<u64>()
                .unwrap_or_default();
            pages.insert(key.trim(), value);
        }
    }
    let available_pages = [
        "Pages free",
        "Pages inactive",
        "Pages speculative",
        "Pages purgeable",
    ]
    .iter()
    .map(|key| pages.get(key).copied().unwrap_or_default())
    .sum::<u64>();
    let available_bytes = available_pages.saturating_mul(page_size).min(total_bytes);
    Memory {
        total_bytes,
        available_bytes,
        used_bytes: total_bytes.saturating_sub(available_bytes),
        swap_total_bytes: 0,
        swap_used_bytes: 0,
    }
}

fn parse_boot_time(text: &str) -> Option<u64> {
    let start = text.find("sec =")? + 5;
    text[start..]
        .trim_start()
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_secs())
}

fn parse_load(text: &str) -> LoadAverage {
    let values: Vec<f64> = text
        .trim_matches(|character| character == '{' || character == '}')
        .split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect();
    LoadAverage {
        one: values.first().copied().unwrap_or_default(),
        five: values.get(1).copied().unwrap_or_default(),
        fifteen: values.get(2).copied().unwrap_or_default(),
    }
}

fn parse_idle_percent(text: &str) -> Option<f64> {
    let cpu = text.lines().find(|line| line.starts_with("CPU usage:"))?;
    let idle = cpu.split(',').find(|value| value.contains("idle"))?;
    idle.split_whitespace()
        .find_map(|value| value.trim_end_matches('%').parse().ok())
}

fn parse_process(line: &str) -> Option<Process> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 12 {
        return None;
    }
    let pid = ProcessId(fields[0].parse().ok()?);
    let parent = fields[1]
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .map(ProcessId);
    let uid = fields[2].parse().unwrap_or(u32::MAX);
    let state = match fields[4].chars().next().unwrap_or('?') {
        'R' => ProcessState::Running,
        'S' => ProcessState::Sleeping,
        'I' => ProcessState::Idle,
        'T' => ProcessState::Stopped,
        'Z' => ProcessState::Zombie,
        'U' | 'D' => ProcessState::DiskSleep,
        _ => ProcessState::Unknown,
    };
    let runtime_seconds = parse_duration(fields[10]);
    let command = fields[11].to_owned();
    let name = command.rsplit('/').next().unwrap_or(&command).to_owned();
    Some(Process {
        pid,
        parent_pid: parent,
        name,
        command_line: Some(command.clone()),
        executable: command.starts_with('/').then_some(command),
        user: User {
            uid,
            name: Some(fields[3].to_owned()),
        },
        state,
        cpu_percent: fields[5].parse().unwrap_or_default(),
        memory_percent: fields[6].parse().unwrap_or_default(),
        rss_bytes: fields[7]
            .parse::<u64>()
            .unwrap_or_default()
            .saturating_mul(1024),
        virtual_memory_bytes: fields[8]
            .parse::<u64>()
            .unwrap_or_default()
            .saturating_mul(1024),
        threads: 0,
        io: IoCounters::default(),
        runtime_seconds,
        cgroup: None,
        service: None,
        container: None,
        file_descriptor_count: None,
        child_pids: Vec::new(),
        unavailable_fields: vec![
            "I/O counters".into(),
            "thread count".into(),
            "file descriptor count".into(),
        ],
        cpu_time_ticks: parse_cpu_time(fields[9]),
        start_time_ticks: unix_seconds().saturating_sub(runtime_seconds),
    })
}

fn parse_duration(value: &str) -> u64 {
    let (days, time) = value.rsplit_once('-').map_or((0, value), |(days, time)| {
        (days.parse::<u64>().unwrap_or_default(), time)
    });
    let parts: Vec<u64> = time
        .split(':')
        .filter_map(|part| part.parse().ok())
        .collect();
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes.saturating_mul(60).saturating_add(*seconds),
        [hours, minutes, seconds] => hours
            .saturating_mul(3600)
            .saturating_add(minutes.saturating_mul(60))
            .saturating_add(*seconds),
        _ => 0,
    };
    days.saturating_mul(86_400).saturating_add(seconds)
}

fn parse_cpu_time(value: &str) -> u64 {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let centiseconds = fraction
        .chars()
        .take(2)
        .collect::<String>()
        .parse::<u64>()
        .unwrap_or_default()
        .saturating_mul(if fraction.len() == 1 { 10 } else { 1 });
    parse_duration(whole)
        .saturating_mul(100)
        .saturating_add(centiseconds)
}

fn attach_children(processes: &mut [Process]) {
    let by_pid: HashMap<ProcessId, usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.pid, index))
        .collect();
    let pairs: Vec<_> = processes
        .iter()
        .filter_map(|process| process.parent_pid.map(|parent| (parent, process.pid)))
        .collect();
    for (parent, child) in pairs {
        if let Some(index) = by_pid.get(&parent).copied().filter(|_| parent != child) {
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
            ProcessState::Running => counts.running += 1,
            ProcessState::Sleeping | ProcessState::DiskSleep | ProcessState::Idle => {
                counts.sleeping += 1
            }
            ProcessState::Stopped | ProcessState::TracingStop => counts.stopped += 1,
            ProcessState::Zombie => counts.zombie += 1,
            _ => counts.other += 1,
        }
    }
    counts
}

fn relationships(hostname: &str, processes: &[Process]) -> Vec<Relationship> {
    let host = EntityId::Host(hostname.to_owned());
    let mut values = Vec::new();
    for process in processes {
        let process_id = EntityId::Process {
            pid: process.pid,
            start_ticks: process.start_time_ticks,
        };
        values.push(Relationship {
            from: process_id.clone(),
            to: host.clone(),
            kind: RelationshipKind::FindingOnHost,
        });
        values.push(Relationship {
            from: process_id.clone(),
            to: EntityId::User(process.user.uid),
            kind: RelationshipKind::OwnedByUser,
        });
        if let Some(parent_pid) = process.parent_pid
            && let Some(parent) = processes
                .iter()
                .find(|candidate| candidate.pid == parent_pid)
        {
            values.push(Relationship {
                from: process_id,
                to: EntityId::Process {
                    pid: parent.pid,
                    start_ticks: parent.start_time_ticks,
                },
                kind: RelationshipKind::ParentProcess,
            });
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vm_and_process_data() {
        let memory = parse_vm_stat(
            "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 100.\nPages inactive: 200.\nPages speculative: 10.\nPages purgeable: 5.",
            4096 * 1000,
        );
        assert_eq!(memory.available_bytes, 4096 * 315);
        let process =
            parse_process("42 1 501 elliot S 1.2 0.5 1024 4096 0:02.34 01:05 /usr/bin/example")
                .expect("process");
        assert_eq!(process.pid, ProcessId(42));
        assert_eq!(process.cpu_time_ticks, 234);
        assert_eq!(process.runtime_seconds, 65);
    }

    #[test]
    fn parses_host_counters() {
        assert_eq!(parse_boot_time("{ sec = 100, usec = 0 }"), Some(100));
        assert_eq!(
            parse_idle_percent("CPU usage: 10.0% user, 5.0% sys, 85.0% idle"),
            Some(85.0)
        );
        assert_eq!(parse_duration("2-01:02:03"), 176_523);
    }
}
