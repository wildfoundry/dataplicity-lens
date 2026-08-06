use lens_model::{
    Cgroup, ContainerReference, Host, IoCounters, LoadAverage, Memory, Process, ProcessCounts,
    ProcessId, ProcessState, SchemaVersion, ServiceReference, Snapshot, Timestamp, User,
};

#[derive(Debug, Default)]
pub struct DemoSource {
    sequence: u64,
}

impl DemoSource {
    pub fn collect(&mut self, interval_ms: u64) -> Snapshot {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        let processes = vec![
            process(
                8421,
                1,
                "image-worker",
                "service",
                ProcessState::Running,
                380 + sequence * 42,
                12_400 + sequence * 400,
                812 * 1024 * 1024 + sequence * 12 * 1024 * 1024,
                Some("image-worker.service"),
            ),
            process(
                1027,
                1,
                "postgres",
                "postgres",
                ProcessState::Sleeping,
                160 + sequence * 8,
                18_100 + sequence * 12,
                1_180 * 1024 * 1024,
                Some("postgresql.service"),
            ),
            process(
                2214,
                1,
                "mqtt-bridge",
                "mqtt",
                ProcessState::Sleeping,
                80 + sequence * 3,
                1_800,
                118 * 1024 * 1024,
                Some("mqtt-bridge.service"),
            ),
            process(
                9462,
                8421,
                "image-helper",
                "service",
                ProcessState::Zombie,
                0,
                0,
                0,
                Some("image-worker.service"),
            ),
            container_process(sequence),
        ];
        let counts = ProcessCounts {
            total: processes.len(),
            running: 1,
            sleeping: 3,
            stopped: 0,
            zombie: 1,
            other: 0,
        };
        Snapshot {
            schema_version: SchemaVersion::default(),
            generated_at: Timestamp(format!("2026-08-03T00:00:{:02}Z", sequence.min(59))),
            host: Host {
                hostname: "production-gateway-04".to_owned(),
                kernel: "6.8.0-lens-demo".to_owned(),
                os_name: Some("Ubuntu 24.04 LTS".to_owned()),
                uptime_seconds: 8 * 86_400 + sequence,
                cpu_count: 8,
                cpu_percent: 0.0,
                load: LoadAverage {
                    one: 0.41,
                    five: 0.38,
                    fifteen: 0.31,
                },
                memory: Memory {
                    total_bytes: 8 * 1024 * 1024 * 1024,
                    available_bytes: 4_600 * 1024 * 1024,
                    used_bytes: 3_592 * 1024 * 1024,
                    swap_total_bytes: 2 * 1024 * 1024 * 1024,
                    swap_used_bytes: 128 * 1024 * 1024,
                },
                process_counts: counts,
                refresh_interval_ms: interval_ms,
                total_cpu_ticks: 100_000 + sequence * 800,
                idle_cpu_ticks: 80_000 + sequence * 650,
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
}

#[allow(clippy::too_many_arguments)]
fn process(
    pid: u32,
    parent: u32,
    name: &str,
    user: &str,
    state: ProcessState,
    cpu_ticks: u64,
    memory_basis_points: u64,
    rss_bytes: u64,
    service: Option<&str>,
) -> Process {
    Process {
        pid: ProcessId(pid),
        parent_pid: Some(ProcessId(parent)),
        name: name.to_owned(),
        command_line: Some(format!("/usr/local/bin/{name} --config /etc/{name}.toml")),
        executable: Some(format!("/usr/local/bin/{name}")),
        user: User {
            uid: 1000 + pid % 100,
            name: Some(user.to_owned()),
        },
        state,
        cpu_percent: 0.0,
        memory_percent: memory_basis_points as f64 / 1_000.0,
        rss_bytes,
        virtual_memory_bytes: rss_bytes.saturating_mul(2),
        threads: if state == ProcessState::Zombie { 1 } else { 8 },
        io: IoCounters {
            read_bytes: cpu_ticks.saturating_mul(80_000),
            write_bytes: cpu_ticks.saturating_mul(12_000),
            read_bytes_per_second: 0.0,
            write_bytes_per_second: 0.0,
        },
        runtime_seconds: 7_200 + u64::from(pid % 1_000),
        cgroup: service.map(|name| Cgroup {
            path: format!("/system.slice/{name}"),
        }),
        service: service.map(|name| ServiceReference {
            name: name.to_owned(),
            inferred: true,
        }),
        container: None,
        file_descriptor_count: Some(32 + u64::from(pid % 20)),
        child_pids: if pid == 8421 {
            vec![ProcessId(9462)]
        } else {
            Vec::new()
        },
        unavailable_fields: Vec::new(),
        cpu_time_ticks: cpu_ticks,
        start_time_ticks: 1_000 + u64::from(pid),
    }
}

fn container_process(sequence: u64) -> Process {
    let mut value = process(
        3011,
        1,
        "telemetry-api",
        "telemetry",
        ProcessState::Sleeping,
        120 + sequence * 5,
        2_100,
        172 * 1024 * 1024,
        None,
    );
    value.cgroup = Some(Cgroup {
        path: "/kubepods.slice/cri-containerd-1234567890abcdef1234567890abcdef.scope".to_owned(),
    });
    value.container = Some(ContainerReference {
        runtime: Some("containerd".to_owned()),
        id: "1234567890abcdef1234567890abcdef".to_owned(),
        inferred: true,
    });
    value
}
