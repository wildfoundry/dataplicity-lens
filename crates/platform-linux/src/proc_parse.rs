use std::collections::HashMap;

use lens_model::{LoadAverage, Memory, ProcessState};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParseError {
    #[error("missing field: {0}")]
    Missing(&'static str),
    #[error("invalid field {field}: {value}")]
    Invalid { field: &'static str, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuStat {
    pub total_ticks: u64,
    pub idle_ticks: u64,
    pub cpu_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidStat {
    pub pid: u32,
    pub name: String,
    pub state: ProcessState,
    pub parent_pid: u32,
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub threads: u32,
    pub start_time_ticks: u64,
    pub virtual_memory_bytes: u64,
    pub rss_pages: i64,
}

pub fn parse_proc_stat(input: &str) -> Result<CpuStat, ParseError> {
    let mut lines = input.lines();
    let aggregate = lines.next().ok_or(ParseError::Missing("cpu aggregate"))?;
    let mut fields = aggregate.split_whitespace();
    if fields.next() != Some("cpu") {
        return Err(ParseError::Missing("cpu aggregate"));
    }
    let values: Vec<u64> = fields
        .map(|value| parse_u64("cpu tick", value))
        .collect::<Result<_, _>>()?;
    if values.len() < 4 {
        return Err(ParseError::Missing("cpu tick fields"));
    }
    let idle = values[3].saturating_add(values.get(4).copied().unwrap_or_default());
    let cpu_count = input
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu")
                .and_then(|suffix| suffix.split_whitespace().next())
                .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
        })
        .count();
    Ok(CpuStat {
        total_ticks: values.iter().copied().sum(),
        idle_ticks: idle,
        cpu_count: cpu_count.max(1),
    })
}

pub fn parse_meminfo(input: &str) -> Result<Memory, ParseError> {
    let values: HashMap<&str, u64> = input
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let raw = rest.split_whitespace().next()?;
            raw.parse::<u64>().ok().map(|value| (key, value * 1024))
        })
        .collect();
    let total = *values.get("MemTotal").ok_or(ParseError::Missing("MemTotal"))?;
    let available = values
        .get("MemAvailable")
        .copied()
        .or_else(|| values.get("MemFree").copied())
        .unwrap_or_default();
    let swap_total = values.get("SwapTotal").copied().unwrap_or_default();
    let swap_free = values.get("SwapFree").copied().unwrap_or_default();
    Ok(Memory {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total.saturating_sub(available),
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_total.saturating_sub(swap_free),
    })
}

pub fn parse_loadavg(input: &str) -> Result<LoadAverage, ParseError> {
    let values: Vec<&str> = input.split_whitespace().collect();
    if values.len() < 3 {
        return Err(ParseError::Missing("load averages"));
    }
    Ok(LoadAverage {
        one: parse_f64("load one", values[0])?,
        five: parse_f64("load five", values[1])?,
        fifteen: parse_f64("load fifteen", values[2])?,
    })
}

pub fn parse_pid_stat(input: &str) -> Result<PidStat, ParseError> {
    let open = input.find('(').ok_or(ParseError::Missing("process name open"))?;
    let close = input.rfind(')').ok_or(ParseError::Missing("process name close"))?;
    if close <= open {
        return Err(ParseError::Missing("process name"));
    }
    let pid = parse_u32("pid", input[..open].trim())?;
    let name = input[open + 1..close].to_owned();
    let fields: Vec<&str> = input[close + 1..].split_whitespace().collect();
    if fields.len() < 22 {
        return Err(ParseError::Missing("pid stat fields"));
    }
    let state = fields[0].chars().next().map_or(ProcessState::Unknown, state_from_char);
    Ok(PidStat {
        pid,
        name,
        state,
        parent_pid: parse_u32("parent pid", fields[1])?,
        user_ticks: parse_u64("user ticks", fields[11])?,
        system_ticks: parse_u64("system ticks", fields[12])?,
        threads: parse_u32("threads", fields[17])?,
        start_time_ticks: parse_u64("start time", fields[19])?,
        virtual_memory_bytes: parse_u64("virtual memory", fields[20])?,
        rss_pages: parse_i64("rss pages", fields[21])?,
    })
}

pub fn state_from_char(value: char) -> ProcessState {
    match value {
        'R' => ProcessState::Running,
        'S' => ProcessState::Sleeping,
        'D' => ProcessState::DiskSleep,
        'T' => ProcessState::Stopped,
        't' => ProcessState::TracingStop,
        'Z' => ProcessState::Zombie,
        'X' | 'x' => ProcessState::Dead,
        'I' => ProcessState::Idle,
        _ => ProcessState::Unknown,
    }
}

fn parse_u64(field: &'static str, value: &str) -> Result<u64, ParseError> {
    value.parse().map_err(|_| ParseError::Invalid {
        field,
        value: value.to_owned(),
    })
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, ParseError> {
    value.parse().map_err(|_| ParseError::Invalid {
        field,
        value: value.to_owned(),
    })
}

fn parse_i64(field: &'static str, value: &str) -> Result<i64, ParseError> {
    value.parse().map_err(|_| ParseError::Invalid {
        field,
        value: value.to_owned(),
    })
}

fn parse_f64(field: &'static str, value: &str) -> Result<f64, ParseError> {
    value.parse().map_err(|_| ParseError::Invalid {
        field,
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn parses_names_with_spaces_and_parentheses() {
        let input = "42 (strange ) worker) S 1 0 0 0 0 0 0 0 0 10 5 0 0 20 0 3 0 900 4096 12";
        let parsed = parse_pid_stat(input).expect("fixture should parse");
        assert_eq!(parsed.name, "strange ) worker");
        assert_eq!(parsed.parent_pid, 1);
        assert_eq!(parsed.user_ticks, 10);
        assert_eq!(parsed.rss_pages, 12);
    }

    #[test]
    fn parses_memory_without_swap() {
        let memory = parse_meminfo("MemTotal: 100 kB\nMemAvailable: 40 kB\n")
            .expect("fixture should parse");
        assert_eq!(memory.used_bytes, 60 * 1024);
        assert_eq!(memory.swap_total_bytes, 0);
    }

    proptest! {
        #[test]
        fn malformed_pid_stat_never_panics(input in ".{0,2048}") {
            let _ = parse_pid_stat(&input);
        }
    }
}
