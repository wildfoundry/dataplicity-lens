use std::cmp::Ordering;

use lens_model::{ProcessSnapshot, SortKey, SystemSnapshot};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("platform collector failed: {0}")]
    Platform(String),
}

pub trait SnapshotSource {
    fn refresh(&mut self) -> Result<SystemSnapshot, SnapshotError>;
}

#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub sort_key: SortKey,
    pub descending: bool,
    pub filter: Option<String>,
    pub limit: usize,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            sort_key: SortKey::Cpu,
            descending: true,
            filter: None,
            limit: 100,
        }
    }
}

pub fn select_processes(snapshot: &SystemSnapshot, options: &ViewOptions) -> Vec<ProcessSnapshot> {
    let filter = options
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);

    let mut processes: Vec<_> = snapshot
        .processes
        .iter()
        .filter(|process| match &filter {
            Some(needle) => {
                process.name.to_lowercase().contains(needle)
                    || process.command.to_lowercase().contains(needle)
                    || process.pid.to_string().contains(needle)
            }
            None => true,
        })
        .cloned()
        .collect();

    processes.sort_by(|left, right| compare_processes(left, right, options.sort_key));
    if options.descending {
        processes.reverse();
    }
    processes.truncate(options.limit);
    processes
}

fn compare_processes(
    left: &ProcessSnapshot,
    right: &ProcessSnapshot,
    sort_key: SortKey,
) -> Ordering {
    match sort_key {
        SortKey::Cpu => left
            .cpu_percent
            .partial_cmp(&right.cpu_percent)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.pid.cmp(&right.pid)),
        SortKey::Memory => left
            .memory_bytes
            .cmp(&right.memory_bytes)
            .then_with(|| left.pid.cmp(&right.pid)),
        SortKey::Pid => left.pid.cmp(&right.pid),
        SortKey::Name => left
            .name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.pid.cmp(&right.pid)),
        SortKey::Runtime => left
            .runtime_secs
            .cmp(&right.runtime_secs)
            .then_with(|| left.pid.cmp(&right.pid)),
    }
}

#[cfg(test)]
mod tests {
    use lens_model::{LoadAverage, MemorySnapshot, ProcessSnapshot, SortKey, SystemSnapshot};

    use super::{select_processes, ViewOptions};

    fn process(pid: u32, name: &str, cpu: f32, memory_bytes: u64) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent_pid: None,
            name: name.to_owned(),
            command: name.to_owned(),
            status: "Run".to_owned(),
            cpu_percent: cpu,
            memory_bytes,
            virtual_memory_bytes: memory_bytes,
            started_at_unix_secs: 0,
            runtime_secs: 0,
        }
    }

    fn snapshot() -> SystemSnapshot {
        SystemSnapshot {
            schema_version: 1,
            collected_at_unix_ms: 0,
            hostname: "test".to_owned(),
            uptime_secs: 0,
            logical_cpu_count: 4,
            cpu_usage_percent: 0.0,
            load_average: LoadAverage {
                one: 0.0,
                five: 0.0,
                fifteen: 0.0,
            },
            memory: MemorySnapshot {
                total_bytes: 1,
                used_bytes: 0,
                available_bytes: 1,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            processes: vec![
                process(10, "postgres", 7.0, 1_000),
                process(20, "nginx", 2.0, 5_000),
                process(30, "postgres-exporter", 1.0, 500),
            ],
        }
    }

    #[test]
    fn filters_sorts_and_limits() {
        let options = ViewOptions {
            sort_key: SortKey::Cpu,
            descending: true,
            filter: Some("postgres".to_owned()),
            limit: 1,
        };

        let selected = select_processes(&snapshot(), &options);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].pid, 10);
    }
}
