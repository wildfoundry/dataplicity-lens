use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lens_core::{GroupMode, ProcessFilter, SortDirection, SortKey, select_processes};
use lens_model::{IoCounters, Process, ProcessId, ProcessState, User};

fn processes(count: usize) -> Vec<Process> {
    (0..count)
        .map(|index| Process {
            pid: ProcessId(index as u32 + 1),
            parent_pid: (index > 0).then_some(ProcessId(index as u32)),
            name: format!("worker-{index}"),
            command_line: Some(format!("worker-{index} --queue {}", index % 16)),
            executable: None,
            user: User {
                uid: (index % 100) as u32,
                name: Some(format!("user-{}", index % 100)),
            },
            state: ProcessState::Sleeping,
            cpu_percent: (index % 400) as f64 / 10.0,
            memory_percent: (index % 100) as f64 / 10.0,
            rss_bytes: index as u64 * 4096,
            virtual_memory_bytes: index as u64 * 8192,
            threads: (index % 32 + 1) as u32,
            io: IoCounters {
                read_bytes_per_second: index as f64 * 100.0,
                write_bytes_per_second: index as f64 * 10.0,
                ..IoCounters::default()
            },
            runtime_seconds: index as u64,
            cgroup: None,
            service: None,
            container: None,
            file_descriptor_count: Some(10),
            child_pids: Vec::new(),
            unavailable_fields: Vec::new(),
            cpu_time_ticks: index as u64,
            start_time_ticks: index as u64,
        })
        .collect()
}

fn benchmark_sort_and_filter(criterion: &mut Criterion) {
    let values = processes(10_000);
    criterion.bench_function("filter_sort_10000_processes", |bencher| {
        bencher.iter(|| {
            select_processes(
                black_box(&values),
                black_box(&ProcessFilter {
                    min_cpu: Some(5.0),
                    ..ProcessFilter::default()
                }),
                SortKey::Cpu,
                SortDirection::Descending,
                GroupMode::None,
                Some(200),
            )
        });
    });
}

criterion_group!(benches, benchmark_sort_and_filter);
criterion_main!(benches);
