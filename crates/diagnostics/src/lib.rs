#![forbid(unsafe_code)]

use lens_history::{HistoryStore, ProcessKey};
use lens_model::{EntityId, Evidence, Finding, ProcessState, Severity, Snapshot};

pub fn evaluate(snapshot: &Snapshot, history: &HistoryStore) -> Vec<Finding> {
    let mut findings = Vec::new();
    host_findings(snapshot, history, &mut findings);
    process_findings(snapshot, history, &mut findings);
    permission_findings(snapshot, &mut findings);
    findings.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| left.id.cmp(&right.id))
    });
    findings
}

fn host_findings(snapshot: &Snapshot, history: &HistoryStore, output: &mut Vec<Finding>) {
    let cpu_count = snapshot.host.cpu_count.max(1) as f64;
    if snapshot.host.load.one > cpu_count * 1.5 {
        output.push(Finding {
            id: "host.load.high".to_owned(),
            severity: if snapshot.host.load.one > cpu_count * 3.0 {
                Severity::Critical
            } else {
                Severity::Attention
            },
            title: "System load is high".to_owned(),
            summary: "The one-minute load average is high relative to the available CPUs. This may indicate CPU saturation or tasks blocked on I/O.".to_owned(),
            evidence: vec![
                Evidence {
                    label: "one-minute load".to_owned(),
                    value: format!("{:.2}", snapshot.host.load.one),
                    unit: None,
                },
                Evidence {
                    label: "logical CPUs".to_owned(),
                    value: snapshot.host.cpu_count.to_string(),
                    unit: None,
                },
            ],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec![
                "Inspect the busiest processes and tasks in uninterruptible sleep.".to_owned(),
                "Compare CPU pressure with disk and network activity before taking action.".to_owned(),
            ],
        });
    }

    let swap_percent = snapshot.host.memory.swap_used_percent();
    if swap_percent >= 75.0 {
        output.push(Finding {
            id: "host.swap.pressure".to_owned(),
            severity: if swap_percent >= 90.0 {
                Severity::Critical
            } else {
                Severity::Attention
            },
            title: "Swap usage is high".to_owned(),
            summary: "Heavy swap use may indicate memory pressure, although inactive pages can remain in swap after pressure has passed.".to_owned(),
            evidence: vec![Evidence {
                label: "swap used".to_owned(),
                value: format!("{swap_percent:.1}"),
                unit: Some("percent".to_owned()),
            }],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec![
                "Inspect resident-memory consumers and confirm whether swap-in activity is ongoing."
                    .to_owned(),
            ],
        });
    }

    let recent_cpu = history.recent_host_cpu(4);
    if recent_cpu.len() >= 4 && recent_cpu.iter().all(|value| *value >= 90.0) {
        output.push(Finding {
            id: "host.cpu.sustained".to_owned(),
            severity: Severity::Attention,
            title: "CPU usage has remained high".to_owned(),
            summary: "Host CPU usage stayed above 90% across recent Lens samples.".to_owned(),
            evidence: vec![Evidence {
                label: "recent samples".to_owned(),
                value: recent_cpu
                    .iter()
                    .rev()
                    .map(|value| format!("{value:.0}%"))
                    .collect::<Vec<_>>()
                    .join(", "),
                unit: None,
            }],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec!["Inspect CPU-heavy processes and their service context.".to_owned()],
        });
    }

    let changes = history.changes();
    if changes.count_delta >= 50
        && changes.appeared >= 50
        && changes.count_delta as usize >= snapshot.processes.len().saturating_div(4)
    {
        output.push(Finding {
            id: "host.process-count.spike".to_owned(),
            severity: Severity::Attention,
            title: "Process count increased rapidly".to_owned(),
            summary: "A large number of processes appeared between samples. This may indicate a fork burst, restart loop or short-lived worker surge.".to_owned(),
            evidence: vec![Evidence {
                label: "new processes".to_owned(),
                value: changes.appeared.to_string(),
                unit: None,
            }],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec!["Group by service or user to identify the source of the increase.".to_owned()],
        });
    }
}

fn process_findings(snapshot: &Snapshot, history: &HistoryStore, output: &mut Vec<Finding>) {
    for process in &snapshot.processes {
        let entity = EntityId::Process {
            pid: process.pid,
            start_ticks: process.start_time_ticks,
        };
        if process.state == ProcessState::Zombie {
            output.push(Finding {
                id: format!("process.{}.zombie", process.pid.0),
                severity: Severity::Attention,
                title: "Zombie process detected".to_owned(),
                summary: format!(
                    "PID {} ({}) has exited but has not yet been reaped by its parent.",
                    process.pid.0, process.name
                ),
                evidence: vec![Evidence {
                    label: "parent PID".to_owned(),
                    value: process
                        .parent_pid
                        .map_or_else(|| "unknown".to_owned(), |pid| pid.0.to_string()),
                    unit: None,
                }],
                related_entities: vec![entity.clone()],
                suggested_actions: vec![
                    "Inspect the parent process and its service before considering a restart."
                        .to_owned(),
                ],
            });
        }

        if process.cpu_percent >= 90.0 {
            output.push(Finding {
                id: format!("process.{}.cpu.high", process.pid.0),
                severity: Severity::Attention,
                title: "Process CPU usage is high".to_owned(),
                summary: format!(
                    "{} is consuming {:.1}% CPU in the current sample.",
                    process.name, process.cpu_percent
                ),
                evidence: vec![Evidence {
                    label: "CPU".to_owned(),
                    value: format!("{:.1}", process.cpu_percent),
                    unit: Some("percent".to_owned()),
                }],
                related_entities: vec![entity.clone()],
                suggested_actions: vec![
                    "Inspect the process command, parent and service context; confirm whether the workload is expected."
                        .to_owned(),
                ],
            });
        }

        if process.memory_percent >= 30.0 {
            output.push(Finding {
                id: format!("process.{}.memory.high", process.pid.0),
                severity: if process.memory_percent >= 60.0 {
                    Severity::Critical
                } else {
                    Severity::Attention
                },
                title: "Process memory consumption is very high".to_owned(),
                summary: format!(
                    "{} holds {:.1}% of host memory in resident pages.",
                    process.name, process.memory_percent
                ),
                evidence: vec![Evidence {
                    label: "resident memory".to_owned(),
                    value: process.rss_bytes.to_string(),
                    unit: Some("bytes".to_owned()),
                }],
                related_entities: vec![entity.clone()],
                suggested_actions: vec![
                    "Compare the current resident-memory trend with the process's expected working set."
                        .to_owned(),
                ],
            });
        }

        let key = ProcessKey {
            pid: process.pid,
            start_time_ticks: process.start_time_ticks,
        };
        if let Some((bytes, percent)) = history.process_rss_growth(key)
            && bytes >= 100 * 1024 * 1024
            && percent >= 25.0
        {
            output.push(Finding {
                id: format!("process.{}.memory.growing", process.pid.0),
                severity: Severity::Attention,
                title: "Resident memory is growing quickly".to_owned(),
                summary: "Resident memory increased substantially during the current Lens session. This may indicate a leak, cache growth or a workload phase change.".to_owned(),
                evidence: vec![
                    Evidence {
                        label: "growth".to_owned(),
                        value: bytes.to_string(),
                        unit: Some("bytes".to_owned()),
                    },
                    Evidence {
                        label: "growth relative to first sample".to_owned(),
                        value: format!("{percent:.1}"),
                        unit: Some("percent".to_owned()),
                    },
                ],
                related_entities: vec![entity],
                suggested_actions: vec![
                    "Keep observing the trend and correlate it with workload or request volume."
                        .to_owned(),
                ],
            });
        }
    }
}

fn permission_findings(snapshot: &Snapshot, output: &mut Vec<Finding>) {
    let affected = snapshot
        .processes
        .iter()
        .filter(|process| {
            process
                .unavailable_fields
                .iter()
                .any(|field| field.contains("permission denied"))
        })
        .count();
    if affected > 0 {
        output.push(Finding {
            id: "collection.permissions.limited".to_owned(),
            severity: Severity::Information,
            title: "Some process details are permission-limited".to_owned(),
            summary: "Lens is showing all available data, but the current user cannot read every process field.".to_owned(),
            evidence: vec![Evidence {
                label: "affected processes".to_owned(),
                value: affected.to_string(),
                unit: None,
            }],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec![
                "Run with the minimum additional privileges required only when those fields are necessary."
                    .to_owned(),
            ],
        });
    }
}

#[cfg(test)]
mod tests {
    use lens_history::HistoryStore;
    use lens_model::{IoCounters, Process, ProcessId, User};

    use super::*;

    #[test]
    fn identifies_zombies_without_overstating_cause() {
        let mut snapshot = Snapshot::empty("fixture");
        snapshot.processes.push(Process {
            pid: ProcessId(99),
            parent_pid: Some(ProcessId(1)),
            name: "finished".to_owned(),
            command_line: None,
            executable: None,
            user: User { uid: 0, name: None },
            state: ProcessState::Zombie,
            cpu_percent: 0.0,
            memory_percent: 0.0,
            rss_bytes: 0,
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
            cpu_time_ticks: 0,
            start_time_ticks: 1,
        });
        let findings = evaluate(&snapshot, &HistoryStore::new(10));
        assert!(findings.iter().any(|finding| finding.id == "process.99.zombie"));
    }
}
