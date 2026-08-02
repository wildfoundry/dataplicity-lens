use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Cpu,
    Memory,
    Pid,
    Name,
    Runtime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

impl MemorySnapshot {
    pub fn used_percent(&self) -> f64 {
        percent(self.used_bytes, self.total_bytes)
    }

    pub fn swap_used_percent(&self) -> f64 {
        percent(self.swap_used_bytes, self.swap_total_bytes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub command: String,
    pub status: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub started_at_unix_secs: u64,
    pub runtime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub schema_version: u32,
    pub collected_at_unix_ms: u64,
    pub hostname: String,
    pub uptime_secs: u64,
    pub logical_cpu_count: usize,
    pub cpu_usage_percent: f32,
    pub load_average: LoadAverage,
    pub memory: MemorySnapshot,
    pub processes: Vec<ProcessSnapshot>,
}

impl SystemSnapshot {
    pub const SCHEMA_VERSION: u32 = 1;
}

fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::MemorySnapshot;

    #[test]
    fn memory_percent_handles_zero_total() {
        let memory = MemorySnapshot {
            total_bytes: 0,
            used_bytes: 42,
            available_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        };

        assert_eq!(memory.used_percent(), 0.0);
        assert_eq!(memory.swap_used_percent(), 0.0);
    }
}
