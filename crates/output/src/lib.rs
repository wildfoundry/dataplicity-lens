#![forbid(unsafe_code)]

use std::io::{self, Write};

use lens_model::{Finding, Process, Snapshot};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Plain,
    Json,
    JsonLines,
}

#[derive(Debug, Clone, Copy)]
pub struct PlainOptions {
    pub width: usize,
    pub limit: Option<usize>,
}

impl Default for PlainOptions {
    fn default() -> Self {
        Self {
            width: 120,
            limit: None,
        }
    }
}

pub fn write_snapshot(
    writer: &mut impl Write,
    snapshot: &Snapshot,
    format: OutputFormat,
    options: PlainOptions,
) -> io::Result<()> {
    match format {
        OutputFormat::Plain => write_plain(writer, snapshot, options),
        OutputFormat::Json => write_json(writer, snapshot),
        OutputFormat::JsonLines => write_json_lines(writer, snapshot),
    }
}

pub fn write_plain(
    writer: &mut impl Write,
    snapshot: &Snapshot,
    options: PlainOptions,
) -> io::Result<()> {
    let memory = snapshot.host.memory;
    safe_writeln(
        writer,
        &format!(
            "Dataplicity Lens - {}  kernel {}  uptime {}",
            snapshot.host.hostname,
            snapshot.host.kernel,
            format_duration(snapshot.host.uptime_seconds)
        ),
    )?;
    safe_writeln(
        writer,
        &format!(
            "CPU {:>5.1}%  load {:.2} {:.2} {:.2}  memory {:>5.1}% ({}/{})  swap {:>5.1}%",
            snapshot.host.cpu_percent,
            snapshot.host.load.one,
            snapshot.host.load.five,
            snapshot.host.load.fifteen,
            memory.used_percent(),
            format_bytes(memory.used_bytes),
            format_bytes(memory.total_bytes),
            memory.swap_used_percent(),
        ),
    )?;
    safe_writeln(
        writer,
        &format!(
            "Processes {}  running {}  sleeping {}  zombies {}  findings {}",
            snapshot.host.process_counts.total,
            snapshot.host.process_counts.running,
            snapshot.host.process_counts.sleeping,
            snapshot.host.process_counts.zombie,
            snapshot.findings.len(),
        ),
    )?;
    safe_writeln(writer, "")?;

    let wide = options.width >= 110;
    if wide {
        safe_writeln(
            writer,
            "    PID PROCESS              USER           CPU%   MEM%       RSS       READ      WRITE ST THR  RUNTIME SERVICE/CGROUP",
        )?;
    } else {
        safe_writeln(
            writer,
            "    PID PROCESS              USER           CPU%   MEM%       RSS ST  RUNTIME",
        )?;
    }

    let limit = options.limit.unwrap_or(snapshot.processes.len());
    for process in snapshot.processes.iter().take(limit) {
        let user = truncate(&process.user.display_name(), 14);
        let name = truncate(&process.name, 20);
        if wide {
            let service = process
                .service
                .as_ref()
                .map(|item| item.name.as_str())
                .or_else(|| process.cgroup.as_ref().map(|item| item.path.as_str()))
                .unwrap_or("-");
            safe_writeln(
                writer,
                &format!(
                    "{:>7} {:<20} {:<14} {:>6.1} {:>6.1} {:>9} {:>9} {:>9} {:>2} {:>3} {:>8} {}",
                    process.pid.0,
                    name,
                    user,
                    process.cpu_percent,
                    process.memory_percent,
                    format_bytes(process.rss_bytes),
                    format_rate(process.io.read_bytes_per_second),
                    format_rate(process.io.write_bytes_per_second),
                    process.state.short(),
                    process.threads,
                    format_duration(process.runtime_seconds),
                    truncate(service, options.width.saturating_sub(108).max(8)),
                ),
            )?;
        } else {
            safe_writeln(
                writer,
                &format!(
                    "{:>7} {:<20} {:<14} {:>6.1} {:>6.1} {:>9} {:>2} {:>8}",
                    process.pid.0,
                    name,
                    user,
                    process.cpu_percent,
                    process.memory_percent,
                    format_bytes(process.rss_bytes),
                    process.state.short(),
                    format_duration(process.runtime_seconds),
                ),
            )?;
        }
    }

    if !snapshot.findings.is_empty() {
        safe_writeln(writer, "")?;
        safe_writeln(writer, "Findings")?;
        for finding in &snapshot.findings {
            safe_writeln(
                writer,
                &format!(
                    "- [{}] {}: {}",
                    finding.severity.label(),
                    finding.title,
                    finding.summary
                ),
            )?;
        }
    }
    Ok(())
}

pub fn write_json(writer: &mut impl Write, snapshot: &Snapshot) -> io::Result<()> {
    match serde_json::to_writer_pretty(&mut *writer, snapshot) {
        Ok(()) => safe_writeln(writer, ""),
        Err(error) if error.is_io() => Err(io::Error::new(
            error.io_error_kind().unwrap_or(io::ErrorKind::Other),
            error,
        )),
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

pub fn write_json_value(writer: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    match serde_json::to_writer_pretty(&mut *writer, value) {
        Ok(()) => safe_writeln(writer, ""),
        Err(error) if error.is_io() => Err(io::Error::new(
            error.io_error_kind().unwrap_or(io::ErrorKind::Other),
            error,
        )),
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

/// Emit JSON Lines for the given record types only (plus always `host` when requested).
pub fn write_json_lines_filtered(
    writer: &mut impl Write,
    snapshot: &Snapshot,
    record_types: &[&str],
) -> io::Result<()> {
    let emit_all = record_types.is_empty();
    let want = |name: &str| emit_all || record_types.iter().any(|item| *item == name);
    if want("host") {
        write_json_line(writer, snapshot, "host", &snapshot.host)?;
    }
    if want("process") {
        for process in &snapshot.processes {
            write_json_line(writer, snapshot, "process", process)?;
        }
    }
    if want("service") {
        for service in &snapshot.services {
            write_json_line(writer, snapshot, "service", service)?;
        }
    }
    if want("log_source") {
        for source in &snapshot.log_sources {
            write_json_line(writer, snapshot, "log_source", source)?;
        }
    }
    if want("log") {
        for entry in &snapshot.logs {
            write_json_line(writer, snapshot, "log", entry)?;
        }
    }
    if want("mount") {
        for mount in &snapshot.mounts {
            write_json_line(writer, snapshot, "mount", mount)?;
        }
    }
    if want("filesystem") {
        for filesystem in &snapshot.filesystems {
            write_json_line(writer, snapshot, "filesystem", filesystem)?;
        }
    }
    if want("deleted_open_file") {
        for file in &snapshot.deleted_open_files {
            write_json_line(writer, snapshot, "deleted_open_file", file)?;
        }
    }
    if want("block_device") {
        for device in &snapshot.block_devices {
            write_json_line(writer, snapshot, "block_device", device)?;
        }
    }
    if want("interface") {
        for interface in &snapshot.interfaces {
            write_json_line(writer, snapshot, "interface", interface)?;
        }
    }
    if want("route") {
        for route in &snapshot.routes {
            write_json_line(writer, snapshot, "route", route)?;
        }
    }
    if want("socket") {
        for socket in &snapshot.sockets {
            write_json_line(writer, snapshot, "socket", socket)?;
        }
    }
    if want("finding") {
        for finding in &snapshot.findings {
            write_json_line(writer, snapshot, "finding", finding)?;
        }
    }
    if want("relationship") {
        for relationship in &snapshot.relationships {
            write_json_line(writer, snapshot, "relationship", relationship)?;
        }
    }
    if want("hardware_device") {
        for device in &snapshot.hardware_devices {
            write_json_line(writer, snapshot, "hardware_device", device)?;
        }
    }
    if want("collection_warning") {
        for warning in &snapshot.collection_warnings {
            write_json_line(writer, snapshot, "collection_warning", warning)?;
        }
    }
    Ok(())
}

pub fn jsonl_record_types_for_fields(fields: &[String]) -> Vec<&'static str> {
    let mut types = Vec::new();
    for field in fields {
        let record = match field.as_str() {
            "processes" => "process",
            "services" => "service",
            "log_sources" => "log_source",
            "logs" => "log",
            "mounts" => "mount",
            "filesystems" => "filesystem",
            "deleted_open_files" => "deleted_open_file",
            "block_devices" => "block_device",
            "interfaces" => "interface",
            "routes" => "route",
            "sockets" => "socket",
            "findings" => "finding",
            "relationships" => "relationship",
            "hardware_devices" => "hardware_device",
            "collection_warnings" => "collection_warning",
            _ => continue,
        };
        if !types.contains(&record) {
            types.push(record);
        }
    }
    types
}

#[derive(Serialize)]
struct JsonLine<'a, T: Serialize> {
    schema_version: &'a str,
    generated_at: &'a str,
    record_type: &'a str,
    value: &'a T,
}

pub fn write_json_lines(writer: &mut impl Write, snapshot: &Snapshot) -> io::Result<()> {
    write_json_line(writer, snapshot, "host", &snapshot.host)?;
    for process in &snapshot.processes {
        write_json_line(writer, snapshot, "process", process)?;
    }
    for service in &snapshot.services {
        write_json_line(writer, snapshot, "service", service)?;
    }
    for source in &snapshot.log_sources {
        write_json_line(writer, snapshot, "log_source", source)?;
    }
    for entry in &snapshot.logs {
        write_json_line(writer, snapshot, "log", entry)?;
    }
    for mount in &snapshot.mounts {
        write_json_line(writer, snapshot, "mount", mount)?;
    }
    for filesystem in &snapshot.filesystems {
        write_json_line(writer, snapshot, "filesystem", filesystem)?;
    }
    for file in &snapshot.deleted_open_files {
        write_json_line(writer, snapshot, "deleted_open_file", file)?;
    }
    for device in &snapshot.block_devices {
        write_json_line(writer, snapshot, "block_device", device)?;
    }
    for interface in &snapshot.interfaces {
        write_json_line(writer, snapshot, "interface", interface)?;
    }
    for route in &snapshot.routes {
        write_json_line(writer, snapshot, "route", route)?;
    }
    for socket in &snapshot.sockets {
        write_json_line(writer, snapshot, "socket", socket)?;
    }
    for finding in &snapshot.findings {
        write_json_line(writer, snapshot, "finding", finding)?;
    }
    for relationship in &snapshot.relationships {
        write_json_line(writer, snapshot, "relationship", relationship)?;
    }
    Ok(())
}

fn write_json_line<T: Serialize>(
    writer: &mut impl Write,
    snapshot: &Snapshot,
    record_type: &str,
    value: &T,
) -> io::Result<()> {
    let record = JsonLine {
        schema_version: &snapshot.schema_version.0,
        generated_at: &snapshot.generated_at.0,
        record_type,
        value,
    };
    serde_json::to_writer(&mut *writer, &record).map_err(|error| {
        let kind = error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData);
        io::Error::new(kind, error)
    })?;
    safe_writeln(writer, "")
}

pub fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut amount = value as f64;
    let mut unit = 0usize;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value}B")
    } else if amount >= 100.0 {
        format!("{amount:.0}{}", UNITS[unit])
    } else {
        format!("{amount:.1}{}", UNITS[unit])
    }
}

pub fn format_rate(value: f64) -> String {
    if value <= 0.0 {
        "-".to_owned()
    } else {
        format!("{}/s", format_bytes(value as u64))
    }
}

pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if days > 0 {
        format!("{days}d{hours:02}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

pub fn truncate(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if unicode_width::UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    if width == 1 {
        return "...".chars().take(width).collect();
    }
    let mut result = String::new();
    let target = width.saturating_sub(1);
    for character in value.chars() {
        let candidate = format!("{result}{character}");
        if unicode_width::UnicodeWidthStr::width(candidate.as_str()) > target {
            break;
        }
        result.push(character);
    }
    result.push('…');
    result
}

pub fn most_severe_finding(findings: &[Finding]) -> Option<&Finding> {
    findings.iter().max_by_key(|finding| finding.severity)
}

pub fn process_label(process: &Process) -> String {
    format!("{} ({})", process.name, process.pid.0)
}

fn safe_writeln(writer: &mut impl Write, value: &str) -> io::Result<()> {
    match writeln!(writer, "{value}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use lens_model::{
        Host, LoadAverage, Memory, ProcessCounts, SchemaVersion, Snapshot, Timestamp,
    };

    use super::*;

    fn fixture() -> Snapshot {
        Snapshot {
            schema_version: SchemaVersion("2".to_owned()),
            generated_at: Timestamp("2026-08-03T00:00:00Z".to_owned()),
            host: Host {
                hostname: "demo-host".to_owned(),
                kernel: "6.8.0".to_owned(),
                os_name: Some("Demo Linux".to_owned()),
                uptime_seconds: 3_661,
                cpu_count: 4,
                cpu_percent: 18.2,
                load: LoadAverage {
                    one: 0.4,
                    five: 0.3,
                    fifteen: 0.2,
                },
                memory: Memory {
                    total_bytes: 1024,
                    available_bytes: 512,
                    used_bytes: 512,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                process_counts: ProcessCounts::default(),
                refresh_interval_ms: 1_000,
                total_cpu_ticks: 0,
                idle_cpu_ticks: 0,
            },
            processes: Vec::new(),
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

    #[test]
    fn plain_output_is_stable() {
        let mut bytes = Vec::new();
        write_plain(&mut bytes, &fixture(), PlainOptions::default())
            .expect("plain output should render");
        let value = String::from_utf8(bytes).expect("output should be UTF-8");
        insta::assert_snapshot!(value, @r###"
        Dataplicity Lens - demo-host  kernel 6.8.0  uptime 1h01m
        CPU  18.2%  load 0.40 0.30 0.20  memory  50.0% (512B/1.0KB)  swap   0.0%
        Processes 0  running 0  sleeping 0  zombies 0  findings 0

            PID PROCESS              USER           CPU%   MEM%       RSS       READ      WRITE ST THR  RUNTIME SERVICE/CGROUP
        "###);
    }

    #[test]
    fn formats_human_units() {
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_duration(3_661), "1h01m");
    }
}
