use std::time::{SystemTime, UNIX_EPOCH};

use lens_core::{SnapshotError, SnapshotSource};
use lens_model::{LoadAverage, MemorySnapshot, ProcessSnapshot, SystemSnapshot};
use sysinfo::{CpuExt, PidExt, ProcessExt, System, SystemExt};

pub struct LinuxSource {
    system: System,
}

impl LinuxSource {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        Self { system }
    }
}

impl Default for LinuxSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotSource for LinuxSource {
    fn refresh(&mut self) -> Result<SystemSnapshot, SnapshotError> {
        self.system.refresh_cpu();
        self.system.refresh_memory();
        self.system.refresh_processes();

        let load = System::load_average();
        let processes = self
            .system
            .processes()
            .values()
            .map(|process| ProcessSnapshot {
                pid: process.pid().as_u32(),
                parent_pid: process.parent().map(|pid| pid.as_u32()),
                name: process.name().to_owned(),
                command: process.cmd().join(" "),
                status: format!("{:?}", process.status()),
                cpu_percent: process.cpu_usage(),
                memory_bytes: process.memory(),
                virtual_memory_bytes: process.virtual_memory(),
                started_at_unix_secs: process.start_time(),
                runtime_secs: process.run_time(),
            })
            .collect();

        Ok(SystemSnapshot {
            schema_version: SystemSnapshot::SCHEMA_VERSION,
            collected_at_unix_ms: now_unix_ms()?,
            hostname: System::host_name().unwrap_or_else(|| "linux".to_owned()),
            uptime_secs: System::uptime(),
            logical_cpu_count: self.system.cpus().len(),
            cpu_usage_percent: self.system.global_cpu_info().cpu_usage(),
            load_average: LoadAverage {
                one: load.one,
                five: load.five,
                fifteen: load.fifteen,
            },
            memory: MemorySnapshot {
                total_bytes: self.system.total_memory(),
                used_bytes: self.system.used_memory(),
                available_bytes: self.system.available_memory(),
                swap_total_bytes: self.system.total_swap(),
                swap_used_bytes: self.system.used_swap(),
            },
            processes,
        })
    }
}

fn now_unix_ms() -> Result<u64, SnapshotError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SnapshotError::Platform(error.to_string()))?;
    Ok(duration.as_millis() as u64)
}
