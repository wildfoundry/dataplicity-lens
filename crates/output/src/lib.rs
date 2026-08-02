use std::io::{self, Write};

use lens_model::{ProcessSnapshot, SystemSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
    Ndjson,
}

pub fn write_snapshot<W: Write>(
    mut writer: W,
    snapshot: &SystemSnapshot,
    processes: &[ProcessSnapshot],
    format: OutputFormat,
) -> io::Result<()> {
    match format {
        OutputFormat::Table => write_table(&mut writer, snapshot, processes),
        OutputFormat::Json => {
            let projected = projected_snapshot(snapshot, processes);
            serde_json::to_writer_pretty(&mut writer, &projected).map_err(io::Error::other)?;
            writeln!(writer)
        }
        OutputFormat::Ndjson => {
            let projected = projected_snapshot(snapshot, processes);
            serde_json::to_writer(&mut writer, &projected).map_err(io::Error::other)?;
            writeln!(writer)
        }
    }
}

fn projected_snapshot(
    snapshot: &SystemSnapshot,
    processes: &[ProcessSnapshot],
) -> SystemSnapshot {
    let mut projected = snapshot.clone();
    projected.processes = processes.to_vec();
    projected
}

fn write_table<W: Write>(
    writer: &mut W,
    snapshot: &SystemSnapshot,
    processes: &[ProcessSnapshot],
) -> io::Result<()> {
    writeln!(
        writer,
        "lens-top  host={}  uptime={}  cpu={:.1}%  load={:.2}/{:.2}/{:.2}  memory={:.1}%",
        snapshot.hostname,
        format_duration(snapshot.uptime_secs),
        snapshot.cpu_usage_percent,
        snapshot.load_average.one,
        snapshot.load_average.five,
        snapshot.load_average.fifteen,
        snapshot.memory.used_percent(),
    )?;
    writeln!(writer, "{:<7} {:>7} {:>10} {:<12} COMMAND", "PID", "CPU%", "MEM", "STATE")?;

    for process in processes {
        let command = if process.command.is_empty() {
            &process.name
        } else {
            &process.command
        };
        writeln!(
            writer,
            "{:<7} {:>7.1} {:>10} {:<12} {}",
            process.pid,
            process.cpu_percent,
            format_bytes(process.memory_bytes),
            truncate(&process.status, 12),
            truncate(command, 80),
        )?;
    }

    Ok(())
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }

    let mut result: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, format_duration};

    #[test]
    fn formats_binary_units() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn formats_uptime() {
        assert_eq!(format_duration(90), "1m");
        assert_eq!(format_duration(3_660), "1h 01m");
    }
}
