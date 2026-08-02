use lens_model::SystemSnapshot;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub title: String,
    pub detail: String,
}

pub fn analyse(snapshot: &SystemSnapshot) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let cpu_count = snapshot.logical_cpu_count.max(1) as f64;

    if snapshot.load_average.one > cpu_count * 1.5 {
        findings.push(Diagnostic {
            severity: Severity::Critical,
            code: "load.saturated",
            title: "Run queue is saturated".to_owned(),
            detail: format!(
                "1 minute load is {:.2} across {} logical CPUs",
                snapshot.load_average.one, snapshot.logical_cpu_count
            ),
        });
    } else if snapshot.load_average.one > cpu_count {
        findings.push(Diagnostic {
            severity: Severity::Warning,
            code: "load.elevated",
            title: "Load is above CPU capacity".to_owned(),
            detail: format!(
                "1 minute load is {:.2} across {} logical CPUs",
                snapshot.load_average.one, snapshot.logical_cpu_count
            ),
        });
    }

    let memory_percent = snapshot.memory.used_percent();
    if memory_percent >= 95.0 {
        findings.push(Diagnostic {
            severity: Severity::Critical,
            code: "memory.critical",
            title: "Memory is critically constrained".to_owned(),
            detail: format!("{memory_percent:.1}% of physical memory is in use"),
        });
    } else if memory_percent >= 85.0 {
        findings.push(Diagnostic {
            severity: Severity::Warning,
            code: "memory.pressure",
            title: "Memory pressure is elevated".to_owned(),
            detail: format!("{memory_percent:.1}% of physical memory is in use"),
        });
    }

    let swap_percent = snapshot.memory.swap_used_percent();
    if snapshot.memory.swap_used_bytes > 0 && swap_percent >= 50.0 {
        findings.push(Diagnostic {
            severity: Severity::Warning,
            code: "swap.active",
            title: "Swap use is significant".to_owned(),
            detail: format!("{swap_percent:.1}% of configured swap is in use"),
        });
    }

    let zombies = snapshot
        .processes
        .iter()
        .filter(|process| process.status.eq_ignore_ascii_case("zombie"))
        .count();
    if zombies > 0 {
        findings.push(Diagnostic {
            severity: Severity::Warning,
            code: "process.zombie",
            title: "Zombie processes detected".to_owned(),
            detail: format!("{zombies} process(es) are waiting to be reaped"),
        });
    }

    if findings.is_empty() {
        findings.push(Diagnostic {
            severity: Severity::Info,
            code: "system.nominal",
            title: "No obvious pressure detected".to_owned(),
            detail: "CPU load, memory, swap, and process state are within basic thresholds"
                .to_owned(),
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use lens_model::{LoadAverage, MemorySnapshot, SystemSnapshot};

    use super::{analyse, Severity};

    #[test]
    fn reports_critical_memory_pressure() {
        let snapshot = SystemSnapshot {
            schema_version: 1,
            collected_at_unix_ms: 0,
            hostname: "test".to_owned(),
            uptime_secs: 0,
            logical_cpu_count: 2,
            cpu_usage_percent: 10.0,
            load_average: LoadAverage {
                one: 0.2,
                five: 0.2,
                fifteen: 0.2,
            },
            memory: MemorySnapshot {
                total_bytes: 100,
                used_bytes: 96,
                available_bytes: 4,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            processes: Vec::new(),
        };

        assert!(analyse(&snapshot)
            .iter()
            .any(|finding| finding.severity == Severity::Critical));
    }
}
