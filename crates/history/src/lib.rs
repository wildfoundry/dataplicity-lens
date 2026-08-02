#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};

use lens_model::{MetricPoint, MetricSeries, ProcessId, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: ProcessId,
    pub start_time_ticks: u64,
}

#[derive(Debug, Clone, Copy)]
struct ProcessCounters {
    cpu_time_ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
    rss_bytes: u64,
}

#[derive(Debug, Clone)]
struct HistorySample {
    timestamp: lens_model::Timestamp,
    host_cpu: f64,
    memory_percent: f64,
    process_count: usize,
    total_cpu_ticks: u64,
    idle_cpu_ticks: u64,
    processes: HashMap<ProcessKey, ProcessCounters>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessChanges {
    pub appeared: usize,
    pub disappeared: usize,
    pub count_delta: i64,
}

#[derive(Debug)]
pub struct HistoryStore {
    capacity: usize,
    samples: VecDeque<HistorySample>,
    last_changes: ProcessChanges,
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(2),
            samples: VecDeque::with_capacity(capacity.max(2)),
            last_changes: ProcessChanges::default(),
        }
    }

    pub fn apply(&mut self, snapshot: &mut Snapshot, elapsed_seconds: f64) {
        let previous = self.samples.back();
        if let Some(previous) = previous {
            let total_delta = snapshot
                .host
                .total_cpu_ticks
                .saturating_sub(previous.total_cpu_ticks);
            let idle_delta = snapshot
                .host
                .idle_cpu_ticks
                .saturating_sub(previous.idle_cpu_ticks);
            snapshot.host.cpu_percent = if total_delta == 0 {
                0.0
            } else {
                ((total_delta.saturating_sub(idle_delta)) as f64 / total_delta as f64) * 100.0
            };

            for process in &mut snapshot.processes {
                let key = ProcessKey {
                    pid: process.pid,
                    start_time_ticks: process.start_time_ticks,
                };
                if let Some(old) = previous.processes.get(&key) {
                    let process_delta = process.cpu_time_ticks.saturating_sub(old.cpu_time_ticks);
                    process.cpu_percent = if total_delta == 0 {
                        0.0
                    } else {
                        (process_delta as f64 / total_delta as f64)
                            * snapshot.host.cpu_count.max(1) as f64
                            * 100.0
                    };
                    let seconds = elapsed_seconds.max(0.001);
                    process.io.read_bytes_per_second =
                        process.io.read_bytes.saturating_sub(old.read_bytes) as f64 / seconds;
                    process.io.write_bytes_per_second =
                        process.io.write_bytes.saturating_sub(old.write_bytes) as f64 / seconds;
                }
            }

            let old_keys: HashSet<ProcessKey> = previous.processes.keys().copied().collect();
            let new_keys: HashSet<ProcessKey> = snapshot
                .processes
                .iter()
                .map(|process| ProcessKey {
                    pid: process.pid,
                    start_time_ticks: process.start_time_ticks,
                })
                .collect();
            self.last_changes = ProcessChanges {
                appeared: new_keys.difference(&old_keys).count(),
                disappeared: old_keys.difference(&new_keys).count(),
                count_delta: snapshot.processes.len() as i64 - previous.process_count as i64,
            };
        }

        let processes = snapshot
            .processes
            .iter()
            .map(|process| {
                (
                    ProcessKey {
                        pid: process.pid,
                        start_time_ticks: process.start_time_ticks,
                    },
                    ProcessCounters {
                        cpu_time_ticks: process.cpu_time_ticks,
                        read_bytes: process.io.read_bytes,
                        write_bytes: process.io.write_bytes,
                        rss_bytes: process.rss_bytes,
                    },
                )
            })
            .collect();
        self.samples.push_back(HistorySample {
            timestamp: snapshot.generated_at.clone(),
            host_cpu: snapshot.host.cpu_percent,
            memory_percent: snapshot.host.memory.used_percent(),
            process_count: snapshot.processes.len(),
            total_cpu_ticks: snapshot.host.total_cpu_ticks,
            idle_cpu_ticks: snapshot.host.idle_cpu_ticks,
            processes,
        });
        while self.samples.len() > self.capacity {
            self.samples.pop_front();
        }
    }

    pub fn host_cpu_series(&self) -> MetricSeries {
        MetricSeries {
            name: "host_cpu".to_owned(),
            unit: "percent".to_owned(),
            points: self
                .samples
                .iter()
                .map(|sample| MetricPoint {
                    timestamp: sample.timestamp.clone(),
                    value: sample.host_cpu,
                })
                .collect(),
        }
    }

    pub fn memory_series(&self) -> MetricSeries {
        MetricSeries {
            name: "memory_used".to_owned(),
            unit: "percent".to_owned(),
            points: self
                .samples
                .iter()
                .map(|sample| MetricPoint {
                    timestamp: sample.timestamp.clone(),
                    value: sample.memory_percent,
                })
                .collect(),
        }
    }

    pub fn process_cpu_series(&self, key: ProcessKey) -> MetricSeries {
        let mut previous: Option<&ProcessCounters> = None;
        let mut points = Vec::new();
        for sample in &self.samples {
            if let Some(current) = sample.processes.get(&key) {
                let value = previous.map_or(0.0, |old| {
                    current.cpu_time_ticks.saturating_sub(old.cpu_time_ticks) as f64
                });
                points.push(MetricPoint {
                    timestamp: sample.timestamp.clone(),
                    value,
                });
                previous = Some(current);
            }
        }
        MetricSeries {
            name: format!("process_{}_cpu_ticks_delta", key.pid.0),
            unit: "ticks".to_owned(),
            points,
        }
    }

    pub fn process_rss_series(&self, key: ProcessKey) -> MetricSeries {
        MetricSeries {
            name: format!("process_{}_rss", key.pid.0),
            unit: "bytes".to_owned(),
            points: self
                .samples
                .iter()
                .filter_map(|sample| {
                    sample.processes.get(&key).map(|process| MetricPoint {
                        timestamp: sample.timestamp.clone(),
                        value: process.rss_bytes as f64,
                    })
                })
                .collect(),
        }
    }

    pub fn process_rss_growth(&self, key: ProcessKey) -> Option<(u64, f64)> {
        let values: Vec<u64> = self
            .samples
            .iter()
            .filter_map(|sample| sample.processes.get(&key).map(|process| process.rss_bytes))
            .collect();
        let first = *values.first()?;
        let last = *values.last()?;
        if values.len() < 4 || last <= first {
            return None;
        }
        let delta = last - first;
        let percent = if first == 0 {
            100.0
        } else {
            delta as f64 / first as f64 * 100.0
        };
        Some((delta, percent))
    }

    pub fn recent_host_cpu(&self, count: usize) -> Vec<f64> {
        self.samples
            .iter()
            .rev()
            .take(count)
            .map(|sample| sample.host_cpu)
            .collect()
    }

    pub const fn changes(&self) -> ProcessChanges {
        self.last_changes
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use lens_model::{IoCounters, Process, ProcessState, User};

    use super::*;

    fn snapshot(ticks: u64, process_ticks: u64, rss: u64) -> Snapshot {
        let mut value = Snapshot::empty("fixture");
        value.host.cpu_count = 2;
        value.host.total_cpu_ticks = ticks;
        value.host.idle_cpu_ticks = ticks / 2;
        value.processes.push(Process {
            pid: ProcessId(10),
            parent_pid: None,
            name: "worker".to_owned(),
            command_line: None,
            executable: None,
            user: User { uid: 0, name: None },
            state: ProcessState::Running,
            cpu_percent: 0.0,
            memory_percent: 0.0,
            rss_bytes: rss,
            virtual_memory_bytes: 0,
            threads: 1,
            io: IoCounters::default(),
            runtime_seconds: 1,
            cgroup: None,
            service: None,
            container: None,
            file_descriptor_count: None,
            child_pids: Vec::new(),
            unavailable_fields: Vec::new(),
            cpu_time_ticks: process_ticks,
            start_time_ticks: 5,
        });
        value
    }

    #[test]
    fn history_is_bounded() {
        let mut history = HistoryStore::new(3);
        for index in 0..10 {
            let mut value = snapshot(index * 100, index * 10, index * 1_000);
            history.apply(&mut value, 1.0);
        }
        assert_eq!(history.len(), 3);
    }
}
