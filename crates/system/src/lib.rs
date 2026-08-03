#![forbid(unsafe_code)]

use std::{
    env,
    io::{self, IsTerminal, Write},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const SCHEMA_VERSION: &str = "2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum View {
    Services,
    Logs,
    Disk,
    Net,
    Health,
}

impl View {
    pub const ALL: [Self; 5] = [Self::Services, Self::Logs, Self::Disk, Self::Net, Self::Health];

    pub const fn binary(self) -> &'static str {
        match self {
            Self::Services => "lens-services",
            Self::Logs => "lens-logs",
            Self::Disk => "lens-disk",
            Self::Net => "lens-net",
            Self::Health => "lens-health",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Logs => "Logs",
            Self::Disk => "Storage",
            Self::Net => "Network",
            Self::Health => "Health",
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about = "A coherent view of a Linux system")]
pub struct ViewArgs {
    /// Emit stable JSON rather than human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Use deterministic committed sample data.
    #[arg(long)]
    pub demo: bool,
    /// Case-insensitive filter applied to rows and findings.
    #[arg(long)]
    pub filter: Option<String>,
    /// Maximum rows to emit.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub schema_version: String,
    pub generated_at: String,
    pub host: String,
    pub services: Vec<Service>,
    pub logs: Vec<LogEntry>,
    pub mounts: Vec<Mount>,
    pub interfaces: Vec<Interface>,
    pub routes: Vec<Route>,
    pub sockets: Vec<Socket>,
    pub findings: Vec<SystemFinding>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub unit: Option<String>,
    pub priority: Option<String>,
    pub message: String,
    pub repeated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub state: String,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub destination: String,
    pub gateway: Option<String>,
    pub interface: Option<String>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Socket {
    pub protocol: String,
    pub state: String,
    pub local: String,
    pub peer: String,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemFinding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    pub evidence: Vec<String>,
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Attention,
    Critical,
}

pub fn run_view(view: View) -> Result<()> {
    let args = ViewArgs::parse();
    let snapshot = if args.demo { demo_snapshot() } else { collect() };
    let filtered = filter_snapshot(snapshot, args.filter.as_deref(), args.limit);
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &filtered).context("write JSON")?;
        println!();
    } else {
        render_plain(view, &filtered, &mut io::stdout().lock())?;
    }
    Ok(())
}

pub fn run_cockpit() -> Result<()> {
    let args = ViewArgs::parse();
    if args.json || args.demo || !io::stdout().is_terminal() {
        let snapshot = if args.demo { demo_snapshot() } else { collect() };
        if args.json {
            serde_json::to_writer_pretty(io::stdout().lock(), &snapshot)?;
            println!();
        } else {
            render_overview(&snapshot, &mut io::stdout().lock())?;
        }
        return Ok(());
    }

    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let result = cockpit_loop(&mut stdout);
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    result
}

fn cockpit_loop(stdout: &mut io::Stdout) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let snapshot = collect();
        execute!(stdout, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
        writeln!(stdout, "DATAPLICITY LENS  ·  {}", snapshot.host)?;
        writeln!(stdout, "Making Linux make sense.\n")?;
        let critical = snapshot.findings.iter().filter(|f| f.severity == Severity::Critical).count();
        let attention = snapshot.findings.iter().filter(|f| f.severity == Severity::Attention).count();
        writeln!(stdout, "{} services · {} mounts · {} interfaces · {} listeners", snapshot.services.len(), snapshot.mounts.len(), snapshot.interfaces.len(), snapshot.sockets.len())?;
        writeln!(stdout, "{} critical · {} attention\n", critical, attention)?;
        for (index, view) in View::ALL.iter().enumerate() {
            let marker = if index == selected { "▶" } else { " " };
            writeln!(stdout, "{marker} {}", view.title())?;
        }
        writeln!(stdout, "\n↑/↓ move   Enter open   r refresh   q quit")?;
        stdout.flush()?;

        match event::read()? {
            Event::Key(key) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(View::ALL.len() - 1),
                KeyCode::Enter => launch(View::ALL[selected])?,
                KeyCode::Char('r') => {}
                _ => {}
            },
            _ => {}
        }
    }
}

fn launch(view: View) -> Result<()> {
    terminal::disable_raw_mode()?;
    let status = Command::new(view.binary()).status();
    terminal::enable_raw_mode()?;
    match status {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let snapshot = collect();
            execute!(io::stdout(), cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
            render_plain(view, &snapshot, &mut io::stdout().lock())
        }
        Err(error) => Err(error).context("launch specialist view"),
    }
}

pub fn collect() -> SystemSnapshot {
    let mut warnings = Vec::new();
    let services = collect_services(&mut warnings);
    let logs = collect_logs(&mut warnings);
    let mounts = collect_mounts(&mut warnings);
    let interfaces = collect_interfaces(&mut warnings);
    let routes = collect_routes(&mut warnings);
    let sockets = collect_sockets(&mut warnings);
    let mut snapshot = SystemSnapshot {
        schema_version: SCHEMA_VERSION.to_owned(),
        generated_at: OffsetDateTime::now_utc().to_string(),
        host: hostname(),
        services,
        logs,
        mounts,
        interfaces,
        routes,
        sockets,
        findings: Vec::new(),
        warnings,
    };
    snapshot.findings = diagnose(&snapshot);
    snapshot
}

fn command(program: &str, args: &[&str], warnings: &mut Vec<String>) -> Option<String> {
    match Command::new(program).args(args).stdin(Stdio::null()).output() {
        Ok(output) if output.status.success() => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            warnings.push(format!("{program} unavailable: {detail}"));
            None
        }
        Err(error) => {
            warnings.push(format!("{program} unavailable: {error}"));
            None
        }
    }
}

fn collect_services(warnings: &mut Vec<String>) -> Vec<Service> {
    command("systemctl", &["list-units", "--type=service", "--all", "--no-legend", "--plain", "--no-pager"], warnings)
        .map(|text| text.lines().filter_map(parse_service).collect())
        .unwrap_or_default()
}

fn parse_service(line: &str) -> Option<Service> {
    let mut fields = line.split_whitespace();
    let name = fields.next()?.trim_start_matches('●').to_owned();
    let load = fields.next()?.to_owned();
    let active = fields.next()?.to_owned();
    let sub = fields.next()?.to_owned();
    let description = fields.collect::<Vec<_>>().join(" ");
    Some(Service { name, load, active, sub, description })
}

fn collect_logs(warnings: &mut Vec<String>) -> Vec<LogEntry> {
    let text = command("journalctl", &["--no-pager", "--output=short-iso", "-n", "200"], warnings).unwrap_or_default();
    let mut entries = Vec::<LogEntry>::new();
    for line in text.lines() {
        let (timestamp, message) = line.split_once(' ').unwrap_or(("", line));
        if let Some(previous) = entries.last_mut().filter(|entry| entry.message == message) {
            previous.repeated += 1;
        } else {
            entries.push(LogEntry { timestamp: timestamp.to_owned(), unit: None, priority: None, message: message.to_owned(), repeated: 1 });
        }
    }
    entries
}

fn collect_mounts(warnings: &mut Vec<String>) -> Vec<Mount> {
    command("df", &["-P", "-B1", "-T"], warnings)
        .map(|text| text.lines().skip(1).filter_map(parse_mount).collect())
        .unwrap_or_default()
}

fn parse_mount(line: &str) -> Option<Mount> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 7 { return None; }
    let total_bytes = fields[2].parse().ok()?;
    let used_bytes = fields[3].parse().ok()?;
    let available_bytes = fields[4].parse().ok()?;
    let used_percent = fields[5].trim_end_matches('%').parse().ok()?;
    Some(Mount { source: fields[0].to_owned(), filesystem: fields[1].to_owned(), total_bytes, used_bytes, available_bytes, used_percent, target: fields[6..].join(" ") })
}

fn collect_interfaces(warnings: &mut Vec<String>) -> Vec<Interface> {
    command("ip", &["-brief", "address", "show"], warnings)
        .map(|text| text.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(Interface { name: fields.next()?.to_owned(), state: fields.next()?.to_owned(), addresses: fields.map(str::to_owned).collect() })
        }).collect())
        .unwrap_or_default()
}

fn collect_routes(warnings: &mut Vec<String>) -> Vec<Route> {
    command("ip", &["route", "show"], warnings)
        .map(|text| text.lines().map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let destination = fields.first().copied().unwrap_or("unknown").to_owned();
            let gateway = fields.windows(2).find(|pair| pair[0] == "via").map(|pair| pair[1].to_owned());
            let interface = fields.windows(2).find(|pair| pair[0] == "dev").map(|pair| pair[1].to_owned());
            Route { destination, gateway, interface, raw: line.to_owned() }
        }).collect())
        .unwrap_or_default()
}

fn collect_sockets(warnings: &mut Vec<String>) -> Vec<Socket> {
    command("ss", &["-H", "-lntup"], warnings)
        .map(|text| text.lines().filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 5 { return None; }
            Some(Socket { protocol: fields[0].to_owned(), state: fields[1].to_owned(), local: fields[4].to_owned(), peer: fields.get(5).copied().unwrap_or("*").to_owned(), owner: fields.get(6).map(|value| (*value).to_owned()) })
        }).collect())
        .unwrap_or_default()
}

fn diagnose(snapshot: &SystemSnapshot) -> Vec<SystemFinding> {
    let mut findings = Vec::new();
    let failed: Vec<_> = snapshot.services.iter().filter(|service| service.active == "failed" || service.sub == "failed").collect();
    if !failed.is_empty() {
        findings.push(SystemFinding { id: "services.failed".into(), severity: Severity::Critical, title: "Failed services".into(), summary: format!("{} service units are failed.", failed.len()), evidence: failed.iter().take(10).map(|service| service.name.clone()).collect(), suggested_actions: vec!["Open lens-services and inspect the failed units.".into(), "Review related messages with lens-logs.".into()] });
    }
    for mount in snapshot.mounts.iter().filter(|mount| mount.used_percent >= 90.0) {
        findings.push(SystemFinding { id: format!("disk.{}", mount.target), severity: if mount.used_percent >= 97.0 { Severity::Critical } else { Severity::Attention }, title: "Filesystem pressure".into(), summary: format!("{} is {:.0}% full.", mount.target, mount.used_percent), evidence: vec![format!("{} bytes available", mount.available_bytes)], suggested_actions: vec!["Open lens-disk and identify the affected mount.".into(), "Check recent logs for rapid growth.".into()] });
    }
    if snapshot.routes.iter().all(|route| route.destination != "default") {
        findings.push(SystemFinding { id: "net.no-default-route".into(), severity: Severity::Attention, title: "No default route".into(), summary: "No default network route was detected.".into(), evidence: snapshot.routes.iter().take(5).map(|route| route.raw.clone()).collect(), suggested_actions: vec!["Open lens-net and inspect interfaces and routes.".into()] });
    }
    let severe_logs: Vec<_> = snapshot.logs.iter().filter(|entry| {
        let message = entry.message.to_ascii_lowercase();
        message.contains("error") || message.contains("failed") || message.contains("panic") || message.contains("out of memory")
    }).collect();
    if severe_logs.len() >= 5 {
        findings.push(SystemFinding { id: "logs.error-volume".into(), severity: Severity::Attention, title: "Elevated error logging".into(), summary: format!("{} recent log messages contain error indicators.", severe_logs.len()), evidence: severe_logs.iter().rev().take(5).map(|entry| entry.message.clone()).collect(), suggested_actions: vec!["Open lens-logs and filter the repeated messages.".into()] });
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

fn filter_snapshot(mut snapshot: SystemSnapshot, filter: Option<&str>, limit: usize) -> SystemSnapshot {
    let needle = filter.map(str::to_ascii_lowercase);
    let matches = |text: &str| needle.as_ref().is_none_or(|needle| text.to_ascii_lowercase().contains(needle));
    snapshot.services.retain(|item| matches(&format!("{} {} {} {}", item.name, item.active, item.sub, item.description)));
    snapshot.logs.retain(|item| matches(&item.message));
    snapshot.mounts.retain(|item| matches(&format!("{} {} {}", item.source, item.target, item.filesystem)));
    snapshot.interfaces.retain(|item| matches(&format!("{} {} {}", item.name, item.state, item.addresses.join(" "))));
    snapshot.routes.retain(|item| matches(&item.raw));
    snapshot.sockets.retain(|item| matches(&format!("{} {} {:?}", item.local, item.peer, item.owner)));
    snapshot.findings.retain(|item| matches(&format!("{} {} {}", item.id, item.title, item.summary)));
    snapshot.services.truncate(limit);
    snapshot.logs.truncate(limit);
    snapshot.mounts.truncate(limit);
    snapshot.interfaces.truncate(limit);
    snapshot.routes.truncate(limit);
    snapshot.sockets.truncate(limit);
    snapshot.findings.truncate(limit);
    snapshot
}

fn render_plain(view: View, snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    match view {
        View::Services => {
            writeln!(out, "SERVICE                          ACTIVE       SUB          DESCRIPTION")?;
            for item in &snapshot.services { writeln!(out, "{:<32} {:<12} {:<12} {}", item.name, item.active, item.sub, item.description)?; }
        }
        View::Logs => {
            for item in &snapshot.logs { writeln!(out, "{}  {}{}", item.timestamp, item.message, if item.repeated > 1 { format!("  ×{}", item.repeated) } else { String::new() })?; }
        }
        View::Disk => {
            writeln!(out, "MOUNT                          USED      AVAILABLE       USE%  FILESYSTEM")?;
            for item in &snapshot.mounts { writeln!(out, "{:<30} {:>10} {:>14} {:>6.1}%  {}", item.target, human_bytes(item.used_bytes), human_bytes(item.available_bytes), item.used_percent, item.filesystem)?; }
        }
        View::Net => {
            writeln!(out, "INTERFACES")?;
            for item in &snapshot.interfaces { writeln!(out, "{:<16} {:<10} {}", item.name, item.state, item.addresses.join(" "))?; }
            writeln!(out, "\nROUTES")?;
            for item in &snapshot.routes { writeln!(out, "{}", item.raw)?; }
            writeln!(out, "\nLISTENERS")?;
            for item in &snapshot.sockets { writeln!(out, "{:<5} {:<8} {:<28} {}", item.protocol, item.state, item.local, item.owner.as_deref().unwrap_or(""))?; }
        }
        View::Health => render_findings(snapshot, out)?,
    }
    render_warnings(snapshot, out)?;
    Ok(())
}

fn render_overview(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "Dataplicity Lens · {}", snapshot.host)?;
    writeln!(out, "{} services · {} mounts · {} interfaces · {} listeners", snapshot.services.len(), snapshot.mounts.len(), snapshot.interfaces.len(), snapshot.sockets.len())?;
    writeln!(out)?;
    render_findings(snapshot, out)
}

fn render_findings(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    if snapshot.findings.is_empty() {
        writeln!(out, "Everything looks healthy based on the available checks.")?;
    } else {
        for finding in &snapshot.findings {
            writeln!(out, "{:?}: {}", finding.severity, finding.title)?;
            writeln!(out, "  {}", finding.summary)?;
            for evidence in &finding.evidence { writeln!(out, "  · {evidence}")?; }
        }
    }
    Ok(())
}

fn render_warnings(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    if !snapshot.warnings.is_empty() {
        writeln!(out, "\nUnavailable data")?;
        for warning in &snapshot.warnings { writeln!(out, "  · {warning}")?; }
    }
    Ok(())
}

fn hostname() -> String {
    env::var("HOSTNAME").ok().filter(|value| !value.is_empty()).or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|value| value.trim().to_owned())).unwrap_or_else(|| "unknown-host".into())
}

fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut number = value as f64;
    let mut unit = 0usize;
    while number >= 1024.0 && unit < UNITS.len() - 1 { number /= 1024.0; unit += 1; }
    format!("{number:.1}{}", UNITS[unit])
}

pub fn demo_snapshot() -> SystemSnapshot {
    let mut snapshot = SystemSnapshot {
        schema_version: SCHEMA_VERSION.into(),
        generated_at: "2026-08-03T00:00:00Z".into(),
        host: "production-gateway-04".into(),
        services: vec![
            Service { name: "mosquitto.service".into(), load: "loaded".into(), active: "failed".into(), sub: "failed".into(), description: "MQTT broker".into() },
            Service { name: "postgresql.service".into(), load: "loaded".into(), active: "active".into(), sub: "running".into(), description: "PostgreSQL database".into() },
        ],
        logs: vec![
            LogEntry { timestamp: "2026-08-03T00:00:01Z".into(), unit: Some("mosquitto.service".into()), priority: Some("error".into()), message: "write failed: No space left on device".into(), repeated: 12 },
            LogEntry { timestamp: "2026-08-03T00:00:02Z".into(), unit: Some("systemd".into()), priority: Some("warning".into()), message: "mosquitto.service entered failed state".into(), repeated: 3 },
        ],
        mounts: vec![Mount { source: "/dev/mmcblk0p2".into(), target: "/".into(), filesystem: "ext4".into(), total_bytes: 16_000_000_000, used_bytes: 15_520_000_000, available_bytes: 480_000_000, used_percent: 97.0 }],
        interfaces: vec![Interface { name: "eth0".into(), state: "UP".into(), addresses: vec!["192.0.2.40/24".into()] }],
        routes: vec![Route { destination: "default".into(), gateway: Some("192.0.2.1".into()), interface: Some("eth0".into()), raw: "default via 192.0.2.1 dev eth0".into() }],
        sockets: vec![Socket { protocol: "tcp".into(), state: "LISTEN".into(), local: "0.0.0.0:5432".into(), peer: "0.0.0.0:*".into(), owner: Some("postgres".into()) }],
        findings: Vec::new(),
        warnings: Vec::new(),
    };
    snapshot.findings = diagnose(&snapshot);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_units() {
        let service = parse_service("nginx.service loaded active running A web server").expect("service");
        assert_eq!(service.name, "nginx.service");
        assert_eq!(service.description, "A web server");
    }

    #[test]
    fn demo_exposes_cross_domain_findings() {
        let snapshot = demo_snapshot();
        assert!(snapshot.findings.iter().any(|item| item.id == "services.failed"));
        assert!(snapshot.findings.iter().any(|item| item.id.starts_with("disk.")));
    }
}
