#![forbid(unsafe_code)]

use std::{
    env,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, ValueEnum};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
pub use lens_model::{
    BlockDevice, DeletedOpenFile, EntityId, Filesystem, Interface, LogEntry, LogSource, Mount,
    Relationship, RelationshipKind, Route, Service, Snapshot as SystemSnapshot, Socket,
};
use lens_model::{
    Cgroup, IoCounters, Process, ProcessId, ProcessState, SchemaVersion, ServiceReference,
    Timestamp, User,
};
#[cfg(target_os = "linux")]
use lens_platform_linux::LinuxCollector;
#[cfg(target_os = "macos")]
use lens_platform_macos::MacOsCollector;

pub const SCHEMA_VERSION: &str = "2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum View {
    Processes,
    Services,
    Logs,
    Disk,
    Net,
    Health,
}

impl View {
    pub const ALL: [Self; 6] = [
        Self::Processes,
        Self::Services,
        Self::Logs,
        Self::Disk,
        Self::Net,
        Self::Health,
    ];

    pub const fn binary(self) -> &'static str {
        match self {
            Self::Processes => "lens-top",
            Self::Services => "lens-services",
            Self::Logs => "lens-logs",
            Self::Disk => "lens-disk",
            Self::Net => "lens-net",
            Self::Health => "lens-health",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Services => "Services",
            Self::Logs => "Logs",
            Self::Disk => "Storage",
            Self::Net => "Network",
            Self::Health => "Health",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Parser)]
#[command(version, about = "A coherent view of a Linux or macOS system")]
pub struct ViewArgs {
    /// Emit stable JSON rather than human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Emit stable plain text explicitly (the default outside an interactive terminal).
    #[arg(long, conflicts_with = "json")]
    pub plain: bool,
    /// Use deterministic committed sample data.
    #[arg(long)]
    pub demo: bool,
    /// Case-insensitive filter applied to rows and findings.
    #[arg(long)]
    pub filter: Option<String>,
    /// Restrict log records and services to a unit name.
    #[arg(long)]
    pub service: Option<String>,
    /// Restrict log records to text associated with a process name or PID.
    #[arg(long)]
    pub process: Option<String>,
    /// Restrict log records to a journal priority label.
    #[arg(long)]
    pub severity: Option<String>,
    /// Restrict journal collection to entries newer than this journalctl time expression.
    #[arg(long)]
    pub since: Option<String>,
    /// Read an additional plain-text log file (repeatable).
    #[arg(long, value_name = "PATH")]
    pub log_file: Vec<PathBuf>,
    /// Generate a manual page and exit.
    #[arg(long, value_name = "PATH")]
    pub generate_man: Option<PathBuf>,
    /// Generate shell completion and exit.
    #[arg(long, value_enum)]
    pub generate_completion: Option<CompletionShell>,
    /// Output path used with --generate-completion.
    #[arg(long, value_name = "PATH", requires = "generate_completion")]
    pub generate_output: Option<PathBuf>,
    /// Maximum rows to emit.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

pub type SystemFinding = lens_model::Finding;
pub type Severity = lens_model::Severity;

pub fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            || cause
                .downcast_ref::<serde_json::Error>()
                .and_then(serde_json::Error::io_error_kind)
                .is_some_and(|kind| kind == io::ErrorKind::BrokenPipe)
    })
}

pub fn run_view(view: View) -> Result<()> {
    let args = ViewArgs::parse();
    if generate_assets(view.binary(), &args)? {
        return Ok(());
    }
    let snapshot = if args.demo {
        demo_snapshot()
    } else {
        collect_with_options(args.since.as_deref(), &args.log_file)
    };
    let filtered = filter_snapshot(
        snapshot,
        args.filter.as_deref(),
        args.service.as_deref(),
        args.process.as_deref(),
        args.severity.as_deref(),
        args.limit,
    );
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &filtered).context("write JSON")?;
        println!();
    } else if args.plain || args.demo || !io::stdout().is_terminal() {
        render_plain(view, &filtered, &mut io::stdout().lock())?;
    } else {
        let mut terminal = CockpitTerminal::enter()?;
        specialist_loop(view, &args, &mut terminal.stdout)?;
    }
    Ok(())
}

pub fn run_cockpit() -> Result<()> {
    let args = ViewArgs::parse();
    if generate_assets("lens", &args)? {
        return Ok(());
    }
    if args.json || args.plain || args.demo || !io::stdout().is_terminal() {
        let snapshot = if args.demo {
            demo_snapshot()
        } else {
            collect_with_options(args.since.as_deref(), &args.log_file)
        };
        let snapshot = filter_snapshot(
            snapshot,
            args.filter.as_deref(),
            args.service.as_deref(),
            args.process.as_deref(),
            args.severity.as_deref(),
            args.limit,
        );
        if args.json {
            serde_json::to_writer_pretty(io::stdout().lock(), &snapshot)?;
            println!();
        } else {
            render_overview(&snapshot, &mut io::stdout().lock())?;
        }
        return Ok(());
    }

    let mut terminal = CockpitTerminal::enter()?;
    cockpit_loop(&mut terminal.stdout)
}

fn generate_assets(name: &'static str, args: &ViewArgs) -> Result<bool> {
    if let Some(path) = &args.generate_man {
        let command = ViewArgs::command().name(name);
        let mut output =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        clap_mangen::Man::new(command).render(&mut output)?;
        return Ok(true);
    }
    if let Some(shell) = args.generate_completion {
        let path = args
            .generate_output
            .as_ref()
            .context("--generate-output is required")?;
        let mut command = ViewArgs::command().name(name);
        let mut output =
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
        match shell {
            CompletionShell::Bash => clap_complete::generate(
                clap_complete::shells::Bash,
                &mut command,
                name,
                &mut output,
            ),
            CompletionShell::Zsh => {
                clap_complete::generate(clap_complete::shells::Zsh, &mut command, name, &mut output)
            }
            CompletionShell::Fish => clap_complete::generate(
                clap_complete::shells::Fish,
                &mut command,
                name,
                &mut output,
            ),
        }
        return Ok(true);
    }
    Ok(false)
}

struct CockpitTerminal {
    stdout: io::Stdout,
}

impl CockpitTerminal {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { stdout })
    }
}

impl Drop for CockpitTerminal {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn cockpit_loop(stdout: &mut io::Stdout) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let snapshot = collect();
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All)
        )?;
        writeln!(stdout, "DATAPLICITY LENS  ·  {}", snapshot.host.hostname)?;
        writeln!(stdout, "Making Linux make sense.\n")?;
        let critical = snapshot
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Critical)
            .count();
        let attention = snapshot
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Attention)
            .count();
        writeln!(
            stdout,
            "{} services · {} mounts · {} interfaces · {} listeners",
            snapshot.services.len(),
            snapshot.mounts.len(),
            snapshot.interfaces.len(),
            snapshot.sockets.len()
        )?;
        writeln!(stdout, "{critical} critical · {attention} attention\n")?;
        for (index, view) in View::ALL.iter().enumerate() {
            let marker = if index == selected { "▶" } else { " " };
            writeln!(stdout, "{marker} {}", view.title())?;
        }
        writeln!(
            stdout,
            "\n↑/↓ move   Enter open   / search   ? help   r refresh   q quit"
        )?;
        stdout.flush()?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = move_selection(selected, 1, View::ALL.len())
                }
                KeyCode::Enter => launch(View::ALL[selected])?,
                KeyCode::Char('/') => {
                    if let Some(query) = prompt_search(stdout)? {
                        launch_search(View::ALL[selected], &query)?;
                    }
                }
                KeyCode::Char('?') => show_cockpit_help(stdout)?,
                KeyCode::Char('r') => {}
                _ => {}
            }
        }
    }
}

fn move_selection(selected: usize, delta: isize, length: usize) -> usize {
    selected
        .saturating_add_signed(delta)
        .min(length.saturating_sub(1))
}

fn specialist_loop(view: View, args: &ViewArgs, stdout: &mut io::Stdout) -> Result<()> {
    loop {
        let snapshot = filter_snapshot(
            collect_with_options(args.since.as_deref(), &args.log_file),
            args.filter.as_deref(),
            args.service.as_deref(),
            args.process.as_deref(),
            args.severity.as_deref(),
            args.limit,
        );
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All)
        )?;
        writeln!(stdout, "{} · {}\n", view.title(), snapshot.host.hostname)?;
        render_plain(view, &snapshot, stdout)?;
        writeln!(stdout, "\nr refresh   q quit")?;
        stdout.flush()?;
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char('r') => {}
                _ => {}
            }
        }
    }
}

fn prompt_search(stdout: &mut io::Stdout) -> Result<Option<String>> {
    let mut query = String::new();
    loop {
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All)
        )?;
        writeln!(stdout, "Search selected view")?;
        writeln!(stdout, "> {query}")?;
        writeln!(stdout, "\nEnter search   Esc cancel")?;
        stdout.flush()?;
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter if !query.is_empty() => return Ok(Some(query)),
                KeyCode::Esc => return Ok(None),
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(character)
                }
                _ => {}
            }
        }
    }
}

fn show_cockpit_help(stdout: &mut io::Stdout) -> Result<()> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    writeln!(stdout, "Lens cockpit help\n")?;
    writeln!(stdout, "↑/↓ or j/k   select a view")?;
    writeln!(stdout, "Enter         open selected view")?;
    writeln!(stdout, "/             search selected view")?;
    writeln!(stdout, "r             refresh")?;
    writeln!(stdout, "q or Ctrl+C   quit safely")?;
    writeln!(stdout, "\nPress any key to return")?;
    stdout.flush()?;
    let _ = event::read()?;
    Ok(())
}

fn launch_search(view: View, query: &str) -> Result<()> {
    terminal::disable_raw_mode()?;
    let argument = if view == View::Processes {
        "--filter-name"
    } else {
        "--filter"
    };
    let status = Command::new(view.binary()).args([argument, query]).status();
    terminal::enable_raw_mode()?;
    match status {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let snapshot = filter_snapshot(collect(), Some(query), None, None, None, 100);
            render_plain(view, &snapshot, &mut io::stdout().lock())
        }
        Err(error) => Err(error).context("launch filtered specialist view"),
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
            execute!(
                io::stdout(),
                cursor::MoveTo(0, 0),
                terminal::Clear(ClearType::All)
            )?;
            render_plain(view, &snapshot, &mut io::stdout().lock())
        }
        Err(error) => Err(error).context("launch specialist view"),
    }
}

pub fn collect() -> SystemSnapshot {
    collect_with_options(None, &[])
}

pub fn collect_with_options(since: Option<&str>, log_files: &[PathBuf]) -> SystemSnapshot {
    #[cfg(target_os = "linux")]
    let collected = LinuxCollector::default().collect();
    #[cfg(target_os = "macos")]
    let collected = MacOsCollector::default().collect();
    let mut snapshot = collected.unwrap_or_else(|error| {
        let mut snapshot = SystemSnapshot::empty(hostname());
        snapshot
            .collection_warnings
            .push(format!("process collector unavailable: {error}"));
        snapshot
    });
    snapshot.schema_version = SchemaVersion(SCHEMA_VERSION.to_owned());
    let warnings = &mut snapshot.collection_warnings;
    let services = collect_services(warnings);
    let mut logs = collect_logs(warnings, since);
    let file_sources = collect_file_logs(log_files, &mut logs, warnings);
    let mut mounts = collect_mounts(warnings);
    apply_inode_usage(&mut mounts, warnings);
    let interfaces = collect_interfaces(warnings);
    let routes = collect_routes(warnings);
    let sockets = collect_sockets(warnings);
    snapshot.services = services;
    snapshot.log_sources = vec![platform_log_source()];
    snapshot.log_sources.extend(file_sources);
    snapshot.logs = logs;
    snapshot.filesystems = filesystems(&mounts);
    snapshot.deleted_open_files = collect_deleted_open_files(warnings);
    snapshot.block_devices = collect_block_devices(warnings);
    snapshot.mounts = mounts;
    snapshot.interfaces = interfaces;
    snapshot.routes = routes;
    snapshot.sockets = sockets;
    snapshot.findings = diagnose(&snapshot);
    snapshot
        .relationships
        .extend(domain_relationships(&snapshot));
    snapshot
}

fn platform_log_source() -> LogSource {
    #[cfg(target_os = "linux")]
    return LogSource {
        id: "systemd-journal".into(),
        kind: "journal".into(),
    };
    #[cfg(target_os = "macos")]
    return LogSource {
        id: "macos-unified-log".into(),
        kind: "unified-log".into(),
    };
}

fn command(program: &str, args: &[&str], warnings: &mut Vec<String>) -> Option<String> {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
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

#[cfg(target_os = "linux")]
fn collect_services(warnings: &mut Vec<String>) -> Vec<Service> {
    let mut services: Vec<Service> = command(
        "systemctl",
        &[
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--plain",
            "--no-pager",
        ],
        warnings,
    )
    .map(|text| text.lines().filter_map(parse_service).collect())
    .unwrap_or_default();
    let restart_counts = command(
        "systemctl",
        &[
            "show",
            "--all",
            "--type=service",
            "--property=Id,NRestarts",
            "--no-pager",
        ],
        warnings,
    )
    .map(|text| parse_service_restart_counts(&text))
    .unwrap_or_default();
    for service in &mut services {
        service.restart_count = restart_counts
            .iter()
            .find(|(name, _)| name == &service.name)
            .map(|(_, count)| *count);
    }
    services
}

#[cfg(target_os = "macos")]
fn collect_services(warnings: &mut Vec<String>) -> Vec<Service> {
    command("launchctl", &["list"], warnings)
        .map(|text| {
            text.lines()
                .skip(1)
                .filter_map(parse_launchd_service)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn parse_launchd_service(line: &str) -> Option<Service> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?;
    let status = fields.next()?;
    let name = fields.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    let running = pid != "-";
    Some(Service {
        name: name.clone(),
        load: "loaded".into(),
        active: if running { "active" } else { "inactive" }.into(),
        sub: if running { "running" } else { "exited" }.into(),
        description: format!("launchd job {name} (last exit status {status})"),
        restart_count: None,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_service(line: &str) -> Option<Service> {
    let mut fields = line.split_whitespace();
    let first = fields.next()?;
    let name = if first == "●" {
        fields.next()?.to_owned()
    } else {
        first.trim_start_matches('●').to_owned()
    };
    let load = fields.next()?.to_owned();
    let active = fields.next()?.to_owned();
    let sub = fields.next()?.to_owned();
    let description = fields.collect::<Vec<_>>().join(" ");
    Some(Service {
        name,
        load,
        active,
        sub,
        description,
        restart_count: None,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_service_restart_counts(text: &str) -> Vec<(String, u64)> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut name = None;
            let mut count = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("Id=") {
                    name = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("NRestarts=") {
                    count = value.parse().ok();
                }
            }
            Some((name?, count?))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_logs(warnings: &mut Vec<String>, since: Option<&str>) -> Vec<LogEntry> {
    let mut args = vec!["--no-pager", "--output=short-iso", "-n", "200"];
    if let Some(since) = since {
        args.extend(["--since", since]);
    }
    let text = command("journalctl", &args, warnings).unwrap_or_default();
    let mut entries = Vec::<LogEntry>::new();
    for line in text.lines() {
        let entry = parse_journal_entry(line);
        let message = entry.message.clone();
        if let Some(previous) = entries.last_mut().filter(|entry| entry.message == message) {
            previous.repeated += 1;
        } else {
            entries.push(entry);
        }
    }
    entries
}

#[cfg(target_os = "macos")]
fn collect_logs(warnings: &mut Vec<String>, since: Option<&str>) -> Vec<LogEntry> {
    let period = since.unwrap_or("1h");
    let text = command(
        "/usr/bin/log",
        &[
            "show", "--style", "syslog", "--last", period, "--info", "--debug",
        ],
        warnings,
    )
    .unwrap_or_default();
    let mut entries = Vec::<LogEntry>::new();
    let lines: Vec<_> = text.lines().skip(1).collect();
    for line in lines.iter().rev().take(200).rev() {
        let entry = parse_macos_log_entry(line);
        if let Some(previous) = entries
            .last_mut()
            .filter(|item| item.message == entry.message)
        {
            previous.repeated += 1;
        } else {
            entries.push(entry);
        }
    }
    entries
}

#[cfg(target_os = "macos")]
fn parse_macos_log_entry(line: &str) -> LogEntry {
    let mut fields = line.split_whitespace();
    let timestamp = match (fields.next(), fields.next(), fields.next()) {
        (Some(date), Some(time), Some(zone)) => format!("{date}T{time}{zone}"),
        _ => String::new(),
    };
    LogEntry {
        timestamp,
        source: "macos-unified-log".into(),
        unit: None,
        priority: log_priority(line),
        message: line.to_owned(),
        repeated: 1,
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_journal_entry(line: &str) -> LogEntry {
    let (timestamp, message) = line.split_once(' ').unwrap_or(("", line));
    let unit = message
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.strip_suffix(':'))
        .map(|value| value.split('[').next().unwrap_or(value).to_owned());
    let lowered = message.to_ascii_lowercase();
    let priority = if lowered.contains("error") || lowered.contains("failed") {
        Some("error".to_owned())
    } else if lowered.contains("warn") {
        Some("warning".to_owned())
    } else {
        None
    };
    LogEntry {
        timestamp: timestamp.to_owned(),
        source: "systemd-journal".to_owned(),
        unit,
        priority,
        message: message.to_owned(),
        repeated: 1,
    }
}

fn collect_file_logs(
    paths: &[PathBuf],
    entries: &mut Vec<LogEntry>,
    warnings: &mut Vec<String>,
) -> Vec<LogSource> {
    let mut sources = Vec::new();
    for path in paths {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let id = path.display().to_string();
                sources.push(LogSource {
                    id: id.clone(),
                    kind: "file".into(),
                });
                let lines: Vec<_> = text.lines().collect();
                let start = lines.len().saturating_sub(200);
                entries.extend(lines[start..].iter().map(|line| LogEntry {
                    timestamp: String::new(),
                    source: id.clone(),
                    unit: None,
                    priority: log_priority(line),
                    message: (*line).to_owned(),
                    repeated: 1,
                }));
            }
            Err(error) => {
                warnings.push(format!("log file {} unavailable: {error}", path.display()))
            }
        }
    }
    sources
}

fn log_priority(message: &str) -> Option<String> {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("error") || lowered.contains("failed") {
        Some("error".into())
    } else if lowered.contains("warn") {
        Some("warning".into())
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn collect_mounts(warnings: &mut Vec<String>) -> Vec<Mount> {
    command("df", &["-P", "-B1", "-T"], warnings)
        .map(|text| text.lines().skip(1).filter_map(parse_mount).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn collect_mounts(warnings: &mut Vec<String>) -> Vec<Mount> {
    command("df", &["-Pk"], warnings)
        .map(|text| text.lines().skip(1).filter_map(parse_macos_mount).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn parse_macos_mount(line: &str) -> Option<Mount> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 6 {
        return None;
    }
    let blocks = fields[1].parse::<u64>().ok()?;
    let used = fields[2].parse::<u64>().ok()?;
    let available = fields[3].parse::<u64>().ok()?;
    Some(Mount {
        source: fields[0].to_owned(),
        target: fields[5..].join(" "),
        filesystem: macos_filesystem_kind(fields[0]),
        total_bytes: blocks.saturating_mul(1024),
        used_bytes: used.saturating_mul(1024),
        available_bytes: available.saturating_mul(1024),
        used_percent: fields[4].trim_end_matches('%').parse().unwrap_or_default(),
        inode_total: None,
        inode_used: None,
    })
}

#[cfg(target_os = "macos")]
fn macos_filesystem_kind(source: &str) -> String {
    if source.starts_with("/dev/disk") {
        "apfs"
    } else if source == "devfs" {
        "devfs"
    } else {
        "network"
    }
    .into()
}

#[cfg(any(target_os = "linux", test))]
fn parse_mount(line: &str) -> Option<Mount> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 7 {
        return None;
    }
    let total_bytes = fields[2].parse().ok()?;
    let used_bytes = fields[3].parse().ok()?;
    let available_bytes = fields[4].parse().ok()?;
    let used_percent = fields[5].trim_end_matches('%').parse().ok()?;
    Some(Mount {
        source: fields[0].to_owned(),
        filesystem: fields[1].to_owned(),
        total_bytes,
        used_bytes,
        available_bytes,
        used_percent,
        inode_total: None,
        inode_used: None,
        target: fields[6..].join(" "),
    })
}

#[cfg(target_os = "linux")]
fn apply_inode_usage(mounts: &mut [Mount], warnings: &mut Vec<String>) {
    let Some(text) = command("df", &["-Pi"], warnings) else {
        return;
    };
    for line in text.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let target = fields[5..].join(" ");
        let Some(mount) = mounts.iter_mut().find(|mount| mount.target == target) else {
            continue;
        };
        let used = fields[2].parse::<u64>().ok();
        let free = fields[3].parse::<u64>().ok();
        if let (Some(used), Some(free)) = (used, free) {
            mount.inode_total = Some(used + free);
            mount.inode_used = Some(used);
        }
    }
}

#[cfg(target_os = "macos")]
fn apply_inode_usage(mounts: &mut [Mount], warnings: &mut Vec<String>) {
    let Some(text) = command("df", &["-Pi"], warnings) else {
        return;
    };
    for line in text.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let target = fields[8..].join(" ");
        if let Some(mount) = mounts.iter_mut().find(|mount| mount.target == target) {
            mount.inode_used = fields[5].parse().ok();
            mount.inode_total = match (fields[5].parse::<u64>(), fields[6].parse::<u64>()) {
                (Ok(used), Ok(free)) => Some(used.saturating_add(free)),
                _ => None,
            };
        }
    }
}

fn collect_deleted_open_files(warnings: &mut Vec<String>) -> Vec<DeletedOpenFile> {
    let Some(text) = command("lsof", &["-nP", "+L1"], warnings) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 9 {
                return None;
            }
            Some(DeletedOpenFile {
                pid: fields[1].parse::<u32>().ok().map(ProcessId),
                command: fields[0].to_owned(),
                size_bytes: fields[6].parse().ok(),
                path: fields[8..].join(" "),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_block_devices(warnings: &mut Vec<String>) -> Vec<BlockDevice> {
    command(
        "lsblk",
        &["-P", "-b", "-n", "-o", "NAME,TYPE,SIZE,FSTYPE,MOUNTPOINTS"],
        warnings,
    )
    .map(|text| text.lines().filter_map(parse_block_device).collect())
    .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn collect_block_devices(warnings: &mut Vec<String>) -> Vec<BlockDevice> {
    command("diskutil", &["list"], warnings)
        .map(|text| parse_diskutil(&text))
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn parse_diskutil(text: &str) -> Vec<BlockDevice> {
    let mut devices = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.trim().strip_prefix("/dev/").and_then(|value| {
            value
                .strip_suffix(" (internal, physical):")
                .or_else(|| value.strip_suffix(" (synthesized):"))
        }) {
            devices.push(BlockDevice {
                name: name.into(),
                kind: "disk".into(),
                size_bytes: 0,
                filesystem: None,
                mountpoints: Vec::new(),
            });
        }
    }
    devices
}

#[cfg(any(target_os = "linux", test))]
fn parse_block_device(line: &str) -> Option<BlockDevice> {
    let value = |key: &str| {
        let marker = format!("{key}=\"");
        let start = line.find(&marker)? + marker.len();
        let rest = &line[start..];
        Some(rest.split('"').next()?.replace("\\x20", " "))
    };
    Some(BlockDevice {
        name: value("NAME")?,
        kind: value("TYPE")?,
        size_bytes: value("SIZE")?.parse().ok()?,
        filesystem: value("FSTYPE").filter(|item| !item.is_empty()),
        mountpoints: value("MOUNTPOINTS")?
            .lines()
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

#[cfg(target_os = "linux")]
fn collect_interfaces(warnings: &mut Vec<String>) -> Vec<Interface> {
    command("ip", &["-brief", "address", "show"], warnings)
        .map(|text| text.lines().filter_map(parse_interface).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn collect_interfaces(warnings: &mut Vec<String>) -> Vec<Interface> {
    command("ifconfig", &["-a"], warnings)
        .map(|text| parse_ifconfig(&text))
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn parse_ifconfig(text: &str) -> Vec<Interface> {
    let mut interfaces = Vec::<Interface>::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            if let Some((name, details)) = line.split_once(':') {
                interfaces.push(Interface {
                    name: name.into(),
                    state: if details.contains("<UP,") || details.contains(",UP,") {
                        "UP"
                    } else {
                        "DOWN"
                    }
                    .into(),
                    addresses: Vec::new(),
                });
            }
        } else if let Some(interface) = interfaces.last_mut() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if matches!(fields.first(), Some(&"inet") | Some(&"inet6"))
                && let Some(address) = fields.get(1)
            {
                interface.addresses.push((*address).into());
            }
        }
    }
    interfaces
}

#[cfg(any(target_os = "linux", test))]
fn parse_interface(line: &str) -> Option<Interface> {
    let mut fields = line.split_whitespace();
    Some(Interface {
        name: fields.next()?.to_owned(),
        state: fields.next()?.to_owned(),
        addresses: fields.map(str::to_owned).collect(),
    })
}

#[cfg(target_os = "linux")]
fn collect_routes(warnings: &mut Vec<String>) -> Vec<Route> {
    command("ip", &["route", "show"], warnings)
        .map(|text| {
            text.lines()
                .enumerate()
                .map(|(index, line)| parse_route(index, line))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn collect_routes(warnings: &mut Vec<String>) -> Vec<Route> {
    command("netstat", &["-rn", "-f", "inet"], warnings)
        .map(|text| {
            text.lines()
                .skip_while(|line| !line.starts_with("Destination"))
                .skip(1)
                .enumerate()
                .filter_map(|(index, line)| {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    (fields.len() >= 4).then(|| Route {
                        id: format!("route-{index}"),
                        destination: fields[0].into(),
                        gateway: Some(fields[1].into()),
                        interface: Some(fields[3].into()),
                        raw: line.into(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(any(target_os = "linux", test))]
fn parse_route(index: usize, line: &str) -> Route {
    let fields: Vec<_> = line.split_whitespace().collect();
    let destination = fields.first().copied().unwrap_or("unknown").to_owned();
    let gateway = fields
        .windows(2)
        .find(|pair| pair[0] == "via")
        .map(|pair| pair[1].to_owned());
    let interface = fields
        .windows(2)
        .find(|pair| pair[0] == "dev")
        .map(|pair| pair[1].to_owned());
    Route {
        id: format!("route-{index}"),
        destination,
        gateway,
        interface,
        raw: line.to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn collect_sockets(warnings: &mut Vec<String>) -> Vec<Socket> {
    command("ss", &["-H", "-lntup"], warnings)
        .map(|text| {
            text.lines()
                .filter_map(|line| {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    if fields.len() < 5 {
                        return None;
                    }
                    let owner = fields.get(6).map(|value| (*value).to_owned());
                    let process_id = owner.as_deref().and_then(parse_socket_pid);
                    Some(Socket {
                        id: format!("{}:{}", fields[0], fields[4]),
                        protocol: fields[0].to_owned(),
                        state: fields[1].to_owned(),
                        local: fields[4].to_owned(),
                        peer: fields.get(5).copied().unwrap_or("*").to_owned(),
                        owner,
                        process_id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn collect_sockets(warnings: &mut Vec<String>) -> Vec<Socket> {
    command("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN"], warnings)
        .map(|text| {
            text.lines()
                .skip(1)
                .filter_map(parse_macos_socket)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn parse_macos_socket(line: &str) -> Option<Socket> {
    let fields: Vec<_> = line.split_whitespace().collect();
    let pid = fields.get(1)?.parse::<u32>().ok().map(ProcessId);
    let protocol_index = fields.iter().position(|field| *field == "TCP")?;
    let local = fields.get(protocol_index + 1)?.to_string();
    Some(Socket {
        id: format!("tcp:{local}:{}", pid.map_or(0, |value| value.0)),
        protocol: "tcp".into(),
        state: "LISTEN".into(),
        local,
        peer: "*".into(),
        owner: Some(fields[0].into()),
        process_id: pid,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_socket_pid(owner: &str) -> Option<ProcessId> {
    let start = owner.find("pid=")? + 4;
    let digits: String = owner[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u32>().ok().map(ProcessId)
}

fn filesystems(mounts: &[Mount]) -> Vec<Filesystem> {
    let mut values: Vec<_> = mounts
        .iter()
        .map(|mount| Filesystem {
            id: mount.source.clone(),
            kind: mount.filesystem.clone(),
        })
        .collect();
    values.sort_by(|left, right| left.id.cmp(&right.id));
    values.dedup_by(|left, right| left.id == right.id);
    values
}

fn domain_relationships(snapshot: &SystemSnapshot) -> Vec<Relationship> {
    let host = EntityId::Host(snapshot.host.hostname.clone());
    let mut values = Vec::new();
    for service in &snapshot.services {
        values.push(Relationship {
            from: EntityId::Service(service.name.clone()),
            to: host.clone(),
            kind: RelationshipKind::HostedOn,
        });
    }
    for process in &snapshot.processes {
        let process_id = EntityId::Process {
            pid: process.pid,
            start_ticks: process.start_time_ticks,
        };
        if let Some(service) = &process.service {
            values.push(Relationship {
                from: process_id,
                to: EntityId::Service(service.name.clone()),
                kind: RelationshipKind::MemberOfService,
            });
        }
    }
    for entry in &snapshot.logs {
        if let Some(unit) = &entry.unit {
            values.push(Relationship {
                from: EntityId::LogSource(entry.source.clone()),
                to: EntityId::Service(unit.clone()),
                kind: RelationshipKind::EmittedByService,
            });
        }
    }
    for mount in &snapshot.mounts {
        values.push(Relationship {
            from: EntityId::Mount(mount.target.clone()),
            to: EntityId::Filesystem(mount.source.clone()),
            kind: RelationshipKind::MountedFilesystem,
        });
    }
    for file in &snapshot.deleted_open_files {
        let Some(pid) = file.pid else { continue };
        let Some(process) = snapshot.processes.iter().find(|process| process.pid == pid) else {
            continue;
        };
        let Some(mount) = snapshot
            .mounts
            .iter()
            .filter(|mount| file.path.starts_with(&mount.target))
            .max_by_key(|mount| mount.target.len())
        else {
            continue;
        };
        values.push(Relationship {
            from: EntityId::Process {
                pid,
                start_ticks: process.start_time_ticks,
            },
            to: EntityId::Mount(mount.target.clone()),
            kind: RelationshipKind::ProcessUsesMount,
        });
    }
    for route in &snapshot.routes {
        if let Some(interface) = &route.interface {
            values.push(Relationship {
                from: EntityId::Route(route.id.clone()),
                to: EntityId::Interface(interface.clone()),
                kind: RelationshipKind::RouteUsesInterface,
            });
        }
    }
    for socket in &snapshot.sockets {
        if let Some(pid) = socket.process_id
            && let Some(process) = snapshot.processes.iter().find(|process| process.pid == pid)
        {
            values.push(Relationship {
                from: EntityId::Socket(socket.id.clone()),
                to: EntityId::Process {
                    pid,
                    start_ticks: process.start_time_ticks,
                },
                kind: RelationshipKind::SocketOwnedByProcess,
            });
        }
        let local_host = endpoint_host(&socket.local);
        if let Some(interface) = snapshot.interfaces.iter().find(|interface| {
            interface.addresses.iter().any(|address| {
                address
                    .split('/')
                    .next()
                    .is_some_and(|address| address == local_host)
            })
        }) {
            values.push(Relationship {
                from: EntityId::Socket(socket.id.clone()),
                to: EntityId::Interface(interface.name.clone()),
                kind: RelationshipKind::SocketBoundToInterface,
            });
        }
    }
    for device in &snapshot.block_devices {
        for mountpoint in &device.mountpoints {
            values.push(Relationship {
                from: EntityId::BlockDevice(device.name.clone()),
                to: EntityId::Mount(mountpoint.clone()),
                kind: RelationshipKind::BlockDeviceMountedAt,
            });
        }
    }
    for finding in &snapshot.findings {
        for entity in &finding.related_entities {
            values.push(Relationship {
                from: EntityId::Finding(finding.id.clone()),
                to: entity.clone(),
                kind: RelationshipKind::FindingConcerns,
            });
        }
    }
    values
}

fn endpoint_host(endpoint: &str) -> &str {
    if endpoint.starts_with('[') {
        endpoint
            .strip_prefix('[')
            .and_then(|value| value.split(']').next())
            .unwrap_or(endpoint)
    } else {
        endpoint.rsplit_once(':').map_or(endpoint, |(host, _)| host)
    }
}

fn diagnose(snapshot: &SystemSnapshot) -> Vec<SystemFinding> {
    let mut findings = Vec::new();
    let failed: Vec<_> = snapshot
        .services
        .iter()
        .filter(|service| service.active == "failed" || service.sub == "failed")
        .collect();
    if !failed.is_empty() {
        findings.push(SystemFinding {
            id: "services.failed".into(),
            severity: Severity::Critical,
            title: "Failed services".into(),
            summary: format!("{} service units are failed.", failed.len()),
            evidence: failed
                .iter()
                .take(10)
                .map(|service| lens_model::Evidence {
                    label: "service".into(),
                    value: service.name.clone(),
                    unit: None,
                })
                .collect(),
            related_entities: failed
                .iter()
                .map(|service| EntityId::Service(service.name.clone()))
                .collect(),
            suggested_actions: vec![
                "Open lens-services and inspect the failed units.".into(),
                "Review related messages with lens-logs.".into(),
            ],
        });
    }
    let restarting: Vec<_> = snapshot
        .services
        .iter()
        .filter(|service| service.restart_count.is_some_and(|count| count >= 3))
        .collect();
    if !restarting.is_empty() {
        findings.push(SystemFinding {
            id: "services.restart-loop".into(),
            severity: Severity::Attention,
            title: "Repeated service restarts".into(),
            summary: format!(
                "{} services have restarted at least three times.",
                restarting.len()
            ),
            evidence: restarting
                .iter()
                .take(10)
                .map(|service| lens_model::Evidence {
                    label: service.name.clone(),
                    value: service.restart_count.unwrap_or_default().to_string(),
                    unit: Some("restarts".into()),
                })
                .collect(),
            related_entities: restarting
                .iter()
                .map(|service| EntityId::Service(service.name.clone()))
                .collect(),
            suggested_actions: vec![
                "Review the service and its recent logs before restarting it.".into(),
            ],
        });
    }
    for mount in snapshot
        .mounts
        .iter()
        .filter(|mount| mount.used_percent >= 90.0)
    {
        findings.push(SystemFinding {
            id: format!("disk.{}", mount.target),
            severity: if mount.used_percent >= 97.0 {
                Severity::Critical
            } else {
                Severity::Attention
            },
            title: "Filesystem pressure".into(),
            summary: format!("{} is {:.0}% full.", mount.target, mount.used_percent),
            evidence: vec![lens_model::Evidence {
                label: "available".into(),
                value: mount.available_bytes.to_string(),
                unit: Some("bytes".into()),
            }],
            related_entities: vec![
                EntityId::Mount(mount.target.clone()),
                EntityId::Filesystem(mount.source.clone()),
            ],
            suggested_actions: vec![
                "Open lens-disk and identify the affected mount.".into(),
                "Check recent logs for rapid growth.".into(),
            ],
        });
    }
    if snapshot
        .routes
        .iter()
        .all(|route| route.destination != "default")
    {
        findings.push(SystemFinding {
            id: "net.no-default-route".into(),
            severity: Severity::Attention,
            title: "No default route".into(),
            summary: "No default network route was detected.".into(),
            evidence: snapshot
                .routes
                .iter()
                .take(5)
                .map(|route| lens_model::Evidence {
                    label: "route".into(),
                    value: route.raw.clone(),
                    unit: None,
                })
                .collect(),
            related_entities: snapshot
                .routes
                .iter()
                .map(|route| EntityId::Route(route.id.clone()))
                .collect(),
            suggested_actions: vec!["Open lens-net and inspect interfaces and routes.".into()],
        });
    }
    for interface in snapshot
        .interfaces
        .iter()
        .filter(|interface| interface.name != "lo" && interface.state.eq_ignore_ascii_case("down"))
    {
        findings.push(SystemFinding {
            id: format!("net.interface-down.{}", interface.name),
            severity: Severity::Attention,
            title: "Network interface is down".into(),
            summary: format!("{} is reported down.", interface.name),
            evidence: vec![lens_model::Evidence {
                label: "state".into(),
                value: interface.state.clone(),
                unit: None,
            }],
            related_entities: vec![EntityId::Interface(interface.name.clone())],
            suggested_actions: vec![
                "Inspect interface configuration and link state with lens-net.".into(),
            ],
        });
    }
    let exposed: Vec<_> = snapshot
        .sockets
        .iter()
        .filter(|socket| {
            matches!(endpoint_host(&socket.local), "0.0.0.0" | "::" | "*")
                && !socket.local.ends_with(":22")
        })
        .collect();
    if !exposed.is_empty() {
        findings.push(SystemFinding {
            id: "net.unexpected-listeners".into(),
            severity: Severity::Attention,
            title: "Services listening on all interfaces".into(),
            summary: format!(
                "{} listeners are exposed on wildcard addresses.",
                exposed.len()
            ),
            evidence: exposed
                .iter()
                .take(10)
                .map(|socket| lens_model::Evidence {
                    label: socket.protocol.clone(),
                    value: socket.local.clone(),
                    unit: None,
                })
                .collect(),
            related_entities: exposed
                .iter()
                .map(|socket| EntityId::Socket(socket.id.clone()))
                .collect(),
            suggested_actions: vec![
                "Confirm each wildcard listener is intended and protected by host policy.".into(),
            ],
        });
    }
    let severe_logs: Vec<_> = snapshot
        .logs
        .iter()
        .filter(|entry| {
            let message = entry.message.to_ascii_lowercase();
            message.contains("error")
                || message.contains("failed")
                || message.contains("panic")
                || message.contains("out of memory")
        })
        .collect();
    let severe_occurrences: u64 = severe_logs.iter().map(|entry| entry.repeated).sum();
    if severe_occurrences >= 5 {
        findings.push(SystemFinding {
            id: "logs.error-volume".into(),
            severity: Severity::Attention,
            title: "Elevated error logging".into(),
            summary: format!(
                "{severe_occurrences} recent log occurrences contain error indicators."
            ),
            evidence: severe_logs
                .iter()
                .rev()
                .take(5)
                .map(|entry| lens_model::Evidence {
                    label: "message".into(),
                    value: entry.message.clone(),
                    unit: None,
                })
                .collect(),
            related_entities: vec![EntityId::LogSource("systemd-journal".into())],
            suggested_actions: vec!["Open lens-logs and filter the repeated messages.".into()],
        });
    }
    let crash_logs: Vec<_> = snapshot
        .logs
        .iter()
        .filter(|entry| {
            let message = entry.message.to_ascii_lowercase();
            message.contains("panic")
                || message.contains("segfault")
                || message.contains("core dumped")
                || message.contains("out of memory")
        })
        .collect();
    if !crash_logs.is_empty() {
        findings.push(SystemFinding {
            id: "logs.crash-context".into(),
            severity: Severity::Critical,
            title: "Recent crash indicators".into(),
            summary: format!(
                "{} recent messages contain crash indicators.",
                crash_logs.len()
            ),
            evidence: crash_logs
                .iter()
                .take(5)
                .map(|entry| lens_model::Evidence {
                    label: entry.unit.clone().unwrap_or_else(|| entry.source.clone()),
                    value: entry.message.clone(),
                    unit: None,
                })
                .collect(),
            related_entities: vec![EntityId::LogSource("systemd-journal".into())],
            suggested_actions: vec!["Inspect preceding service logs with lens-logs.".into()],
        });
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

fn filter_snapshot(
    mut snapshot: SystemSnapshot,
    filter: Option<&str>,
    service: Option<&str>,
    process: Option<&str>,
    severity: Option<&str>,
    limit: usize,
) -> SystemSnapshot {
    let needle = filter.map(str::to_ascii_lowercase);
    let matches = |text: &str| {
        needle
            .as_ref()
            .is_none_or(|needle| text.to_ascii_lowercase().contains(needle))
    };
    snapshot.services.retain(|item| {
        matches(&format!(
            "{} {} {} {}",
            item.name, item.active, item.sub, item.description
        ))
    });
    if let Some(service) = service {
        let service = service.to_ascii_lowercase();
        snapshot
            .services
            .retain(|item| item.name.to_ascii_lowercase().contains(&service));
        snapshot.logs.retain(|item| {
            item.unit
                .as_deref()
                .is_some_and(|unit| unit.to_ascii_lowercase().contains(&service))
        });
    }
    if let Some(severity) = severity {
        let severity = severity.to_ascii_lowercase();
        snapshot.logs.retain(|item| {
            item.priority
                .as_deref()
                .is_some_and(|priority| priority.eq_ignore_ascii_case(&severity))
        });
    }
    if let Some(process) = process {
        let process = process.to_ascii_lowercase();
        snapshot
            .logs
            .retain(|item| item.message.to_ascii_lowercase().contains(&process));
    }
    snapshot.logs.retain(|item| matches(&item.message));
    snapshot.mounts.retain(|item| {
        matches(&format!(
            "{} {} {}",
            item.source, item.target, item.filesystem
        ))
    });
    snapshot.block_devices.retain(|item| {
        matches(&format!(
            "{} {} {} {}",
            item.name,
            item.kind,
            item.filesystem.as_deref().unwrap_or(""),
            item.mountpoints.join(" ")
        ))
    });
    snapshot
        .deleted_open_files
        .retain(|item| matches(&format!("{} {}", item.command, item.path)));
    snapshot.interfaces.retain(|item| {
        matches(&format!(
            "{} {} {}",
            item.name,
            item.state,
            item.addresses.join(" ")
        ))
    });
    snapshot.routes.retain(|item| matches(&item.raw));
    snapshot
        .sockets
        .retain(|item| matches(&format!("{} {} {:?}", item.local, item.peer, item.owner)));
    snapshot
        .findings
        .retain(|item| matches(&format!("{} {} {}", item.id, item.title, item.summary)));
    snapshot.services.truncate(limit);
    snapshot.logs.truncate(limit);
    snapshot.mounts.truncate(limit);
    snapshot.block_devices.truncate(limit);
    snapshot.deleted_open_files.truncate(limit);
    snapshot.interfaces.truncate(limit);
    snapshot.routes.truncate(limit);
    snapshot.sockets.truncate(limit);
    snapshot.findings.truncate(limit);
    snapshot
}

fn render_plain(view: View, snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    match view {
        View::Processes => {
            writeln!(out, "    PID PROCESS                  CPU%   MEM% SERVICE")?;
            for process in &snapshot.processes {
                writeln!(
                    out,
                    "{:>7} {:<24} {:>6.1} {:>6.1} {}",
                    process.pid,
                    process.name,
                    process.cpu_percent,
                    process.memory_percent,
                    process
                        .service
                        .as_ref()
                        .map_or("", |service| service.name.as_str())
                )?;
            }
        }
        View::Services => {
            writeln!(
                out,
                "SERVICE                          ACTIVE       SUB          PIDS RESTARTS DESCRIPTION"
            )?;
            for item in &snapshot.services {
                let process_count = snapshot
                    .processes
                    .iter()
                    .filter(|process| {
                        process
                            .service
                            .as_ref()
                            .is_some_and(|service| service.name == item.name)
                    })
                    .count();
                writeln!(
                    out,
                    "{:<32} {:<12} {:<12} {:>4} {:>8} {}",
                    item.name,
                    item.active,
                    item.sub,
                    process_count,
                    item.restart_count
                        .map_or_else(|| "-".into(), |count| count.to_string()),
                    item.description
                )?;
            }
        }
        View::Logs => {
            for item in &snapshot.logs {
                writeln!(
                    out,
                    "{}  {}{}",
                    item.timestamp,
                    item.message,
                    if item.repeated > 1 {
                        format!("  ×{}", item.repeated)
                    } else {
                        String::new()
                    }
                )?;
            }
        }
        View::Disk => {
            if !snapshot.block_devices.is_empty() {
                writeln!(out, "BLOCK DEVICES")?;
                for item in &snapshot.block_devices {
                    writeln!(
                        out,
                        "{:<16} {:<8} {:>10} {:<10} {}",
                        item.name,
                        item.kind,
                        human_bytes(item.size_bytes),
                        item.filesystem.as_deref().unwrap_or("-"),
                        item.mountpoints.join(", ")
                    )?;
                }
                writeln!(out)?;
            }
            writeln!(
                out,
                "MOUNT                          USED      AVAILABLE       USE%  FILESYSTEM"
            )?;
            for item in &snapshot.mounts {
                writeln!(
                    out,
                    "{:<30} {:>10} {:>14} {:>6.1}%  {}",
                    item.target,
                    human_bytes(item.used_bytes),
                    human_bytes(item.available_bytes),
                    item.used_percent,
                    item.filesystem
                )?;
            }
            if !snapshot.deleted_open_files.is_empty() {
                writeln!(out, "\nDELETED BUT OPEN")?;
                for file in &snapshot.deleted_open_files {
                    writeln!(
                        out,
                        "{:>7} {:<18} {:>10} {}",
                        file.pid
                            .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
                        file.command,
                        file.size_bytes.map_or_else(|| "-".to_owned(), human_bytes),
                        file.path
                    )?;
                }
            }
        }
        View::Net => {
            writeln!(out, "INTERFACES")?;
            for item in &snapshot.interfaces {
                writeln!(
                    out,
                    "{:<16} {:<10} {}",
                    item.name,
                    item.state,
                    item.addresses.join(" ")
                )?;
            }
            writeln!(out, "\nROUTES")?;
            for item in &snapshot.routes {
                writeln!(out, "{}", item.raw)?;
            }
            writeln!(out, "\nLISTENERS")?;
            for item in &snapshot.sockets {
                writeln!(
                    out,
                    "{:<5} {:<8} {:<28} {}",
                    item.protocol,
                    item.state,
                    item.local,
                    item.owner.as_deref().unwrap_or("")
                )?;
            }
        }
        View::Health => render_findings(snapshot, out)?,
    }
    render_warnings(snapshot, out)?;
    Ok(())
}

fn render_overview(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "Dataplicity Lens · {}", snapshot.host.hostname)?;
    writeln!(
        out,
        "{} services · {} mounts · {} interfaces · {} listeners",
        snapshot.services.len(),
        snapshot.mounts.len(),
        snapshot.interfaces.len(),
        snapshot.sockets.len()
    )?;
    writeln!(out)?;
    render_findings(snapshot, out)
}

fn render_findings(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    if snapshot.findings.is_empty() {
        writeln!(
            out,
            "Everything looks healthy based on the available checks."
        )?;
    } else {
        for finding in &snapshot.findings {
            writeln!(out, "{:?}: {}", finding.severity, finding.title)?;
            writeln!(out, "  {}", finding.summary)?;
            for evidence in &finding.evidence {
                writeln!(
                    out,
                    "  · {}: {}{}",
                    evidence.label,
                    evidence.value,
                    evidence
                        .unit
                        .as_deref()
                        .map(|unit| format!(" {unit}"))
                        .unwrap_or_default()
                )?;
            }
        }
    }
    Ok(())
}

fn render_warnings(snapshot: &SystemSnapshot, out: &mut dyn Write) -> Result<()> {
    if !snapshot.collection_warnings.is_empty() {
        writeln!(out, "\nUnavailable data")?;
        for warning in &snapshot.collection_warnings {
            writeln!(out, "  · {warning}")?;
        }
    }
    Ok(())
}

fn hostname() -> String {
    env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown-host".into())
}

fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut number = value as f64;
    let mut unit = 0usize;
    while number >= 1024.0 && unit < UNITS.len() - 1 {
        number /= 1024.0;
        unit += 1;
    }
    format!("{number:.1}{}", UNITS[unit])
}

pub fn demo_snapshot() -> SystemSnapshot {
    let mut snapshot = SystemSnapshot::empty("production-gateway-04");
    snapshot.schema_version = SchemaVersion(SCHEMA_VERSION.into());
    snapshot.generated_at = Timestamp("2026-08-03T00:00:00Z".into());
    snapshot.host.kernel = "6.8.0-lens-demo".into();
    snapshot.host.cpu_count = 4;
    snapshot.host.uptime_seconds = 86_400;
    snapshot.host.process_counts = lens_model::ProcessCounts {
        total: 1,
        sleeping: 1,
        ..lens_model::ProcessCounts::default()
    };
    snapshot.processes = vec![Process {
        pid: ProcessId(4242),
        parent_pid: Some(ProcessId(1)),
        name: "mosquitto".into(),
        command_line: Some("/usr/sbin/mosquitto -c /etc/mosquitto.conf".into()),
        executable: Some("/usr/sbin/mosquitto".into()),
        user: User {
            uid: 1883,
            name: Some("mosquitto".into()),
        },
        state: ProcessState::Sleeping,
        cpu_percent: 0.2,
        memory_percent: 1.4,
        rss_bytes: 22_000_000,
        virtual_memory_bytes: 48_000_000,
        threads: 2,
        io: IoCounters::default(),
        runtime_seconds: 600,
        cgroup: Some(Cgroup {
            path: "/system.slice/mosquitto.service".into(),
        }),
        service: Some(ServiceReference {
            name: "mosquitto.service".into(),
            inferred: true,
        }),
        container: None,
        file_descriptor_count: Some(18),
        child_pids: Vec::new(),
        unavailable_fields: Vec::new(),
        cpu_time_ticks: 100,
        start_time_ticks: 12_345,
    }];
    snapshot.services = vec![
        Service {
            name: "mosquitto.service".into(),
            load: "loaded".into(),
            active: "failed".into(),
            sub: "failed".into(),
            description: "MQTT broker".into(),
            restart_count: Some(7),
        },
        Service {
            name: "postgresql.service".into(),
            load: "loaded".into(),
            active: "active".into(),
            sub: "running".into(),
            description: "PostgreSQL database".into(),
            restart_count: Some(0),
        },
    ];
    snapshot.log_sources = vec![LogSource {
        id: "systemd-journal".into(),
        kind: "journal".into(),
    }];
    snapshot.logs = vec![
        LogEntry {
            timestamp: "2026-08-03T00:00:01Z".into(),
            source: "systemd-journal".into(),
            unit: Some("mosquitto.service".into()),
            priority: Some("error".into()),
            message: "write failed: No space left on device".into(),
            repeated: 12,
        },
        LogEntry {
            timestamp: "2026-08-03T00:00:02Z".into(),
            source: "systemd-journal".into(),
            unit: Some("systemd".into()),
            priority: Some("warning".into()),
            message: "mosquitto.service entered failed state".into(),
            repeated: 3,
        },
    ];
    snapshot.mounts = vec![Mount {
        source: "/dev/mmcblk0p2".into(),
        target: "/".into(),
        filesystem: "ext4".into(),
        total_bytes: 16_000_000_000,
        used_bytes: 15_520_000_000,
        available_bytes: 480_000_000,
        used_percent: 97.0,
        inode_total: Some(1_000_000),
        inode_used: Some(930_000),
    }];
    snapshot.filesystems = filesystems(&snapshot.mounts);
    snapshot.deleted_open_files = vec![DeletedOpenFile {
        pid: Some(ProcessId(4242)),
        command: "mosquitto".into(),
        path: "/var/log/mosquitto/old.log (deleted)".into(),
        size_bytes: Some(12_000_000),
    }];
    snapshot.block_devices = vec![BlockDevice {
        name: "mmcblk0".into(),
        kind: "disk".into(),
        size_bytes: 16_000_000_000,
        filesystem: None,
        mountpoints: vec!["/".into()],
    }];
    snapshot.interfaces = vec![Interface {
        name: "eth0".into(),
        state: "UP".into(),
        addresses: vec!["192.0.2.40/24".into()],
    }];
    snapshot.routes = vec![Route {
        id: "default".into(),
        destination: "default".into(),
        gateway: Some("192.0.2.1".into()),
        interface: Some("eth0".into()),
        raw: "default via 192.0.2.1 dev eth0".into(),
    }];
    snapshot.sockets = vec![Socket {
        id: "tcp:0.0.0.0:1883".into(),
        protocol: "tcp".into(),
        state: "LISTEN".into(),
        local: "0.0.0.0:1883".into(),
        peer: "0.0.0.0:*".into(),
        owner: Some("mosquitto".into()),
        process_id: Some(ProcessId(4242)),
    }];
    snapshot.findings = diagnose(&snapshot);
    snapshot.relationships = domain_relationships(&snapshot);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_units() {
        let service =
            parse_service("nginx.service loaded active running A web server").expect("service");
        assert_eq!(service.name, "nginx.service");
        assert_eq!(service.description, "A web server");
    }

    #[test]
    fn committed_service_fixtures_cover_failures_and_restarts() {
        let units = include_str!("../../../tests/fixtures/systemctl/services.txt");
        let services: Vec<_> = units.lines().filter_map(parse_service).collect();
        assert_eq!(services.len(), 2);
        assert_eq!(services[1].active, "failed");
        let restarts = parse_service_restart_counts(include_str!(
            "../../../tests/fixtures/systemctl/restarts.txt"
        ));
        assert_eq!(restarts[1], ("failed-agent.service".into(), 6));
    }

    #[test]
    fn committed_disk_and_socket_fixtures_parse() {
        let mount = parse_mount(
            include_str!("../../../tests/fixtures/disk/df.txt")
                .lines()
                .nth(1)
                .expect("df row"),
        )
        .expect("mount");
        assert_eq!(mount.target, "/");
        let devices: Vec<_> = include_str!("../../../tests/fixtures/disk/lsblk.txt")
            .lines()
            .filter_map(parse_block_device)
            .collect();
        assert_eq!(devices[1].mountpoints, ["/"]);
        let socket_line = include_str!("../../../tests/fixtures/net/ss.txt");
        assert_eq!(parse_socket_pid(socket_line), Some(ProcessId(42)));
        let interfaces: Vec<_> = include_str!("../../../tests/fixtures/net/ip-address.txt")
            .lines()
            .filter_map(parse_interface)
            .collect();
        assert_eq!(interfaces[1].addresses[0], "192.0.2.40/24");
        let route = parse_route(
            0,
            include_str!("../../../tests/fixtures/net/ip-route.txt")
                .lines()
                .next()
                .expect("route"),
        );
        assert_eq!(route.interface.as_deref(), Some("eth0"));
    }

    #[test]
    fn committed_journal_fixture_folds_repeated_messages() {
        let fixture = include_str!("../../../tests/fixtures/journal/journal.txt");
        let mut entries = Vec::<LogEntry>::new();
        for line in fixture.lines() {
            let entry = parse_journal_entry(line);
            if let Some(previous) = entries
                .last_mut()
                .filter(|previous| previous.message == entry.message)
            {
                previous.repeated += 1;
            } else {
                entries.push(entry);
            }
        }
        assert_eq!(entries.last().expect("last entry").repeated, 2);
        assert_eq!(
            entries.last().expect("last entry").unit.as_deref(),
            Some("failed-agent")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_native_macos_domains() {
        let service = parse_launchd_service("123\t0\tcom.example.worker").expect("launchd job");
        assert_eq!(service.active, "active");
        assert_eq!(service.name, "com.example.worker");

        let mount = parse_macos_mount("/dev/disk3s1s1 1000 250 750 25% /").expect("mount");
        assert_eq!(mount.filesystem, "apfs");
        assert_eq!(mount.total_bytes, 1_024_000);

        let interfaces = parse_ifconfig(
            "lo0: flags=8049<UP,LOOPBACK,RUNNING> mtu 16384\n\tinet 127.0.0.1 netmask 0xff000000\nen0: flags=0<> mtu 1500\n",
        );
        assert_eq!(interfaces[0].state, "UP");
        assert_eq!(interfaces[0].addresses, ["127.0.0.1"]);

        let socket =
            parse_macos_socket("worker 42 user 10u IPv4 0x0 0t0 TCP 127.0.0.1:8080 (LISTEN)")
                .expect("socket");
        assert_eq!(socket.process_id, Some(ProcessId(42)));
        assert_eq!(socket.local, "127.0.0.1:8080");
    }

    #[test]
    fn demo_exposes_cross_domain_findings() {
        let snapshot = demo_snapshot();
        assert!(
            snapshot
                .findings
                .iter()
                .any(|item| item.id == "services.failed")
        );
        assert!(
            snapshot
                .findings
                .iter()
                .any(|item| item.id.starts_with("disk."))
        );
        assert!(snapshot.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::MemberOfService
                && matches!(relationship.from, EntityId::Process { .. })
                && relationship.to == EntityId::Service("mosquitto.service".into())
        }));
        assert!(snapshot.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::RouteUsesInterface
                && relationship.to == EntityId::Interface("eth0".into())
        }));
    }

    #[test]
    fn missing_command_becomes_a_warning() {
        let mut warnings = Vec::new();
        assert!(command("lens-command-that-does-not-exist", &[], &mut warnings).is_none());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn cockpit_navigation_and_overview_are_bounded() {
        assert_eq!(move_selection(0, -1, View::ALL.len()), 0);
        assert_eq!(move_selection(0, 1, View::ALL.len()), 1);
        assert_eq!(
            move_selection(View::ALL.len() - 1, 1, View::ALL.len()),
            View::ALL.len() - 1
        );
        let mut output = Vec::new();
        render_overview(&demo_snapshot(), &mut output).expect("overview");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("production-gateway-04"));
        assert!(output.contains("Failed services"));
    }

    #[test]
    fn service_and_severity_filters_are_deterministic() {
        let filtered = filter_snapshot(
            demo_snapshot(),
            None,
            Some("mosquitto"),
            None,
            Some("error"),
            10,
        );
        assert_eq!(filtered.services.len(), 1);
        assert_eq!(filtered.logs.len(), 1);
        assert_eq!(filtered.logs[0].priority.as_deref(), Some("error"));
    }

    #[test]
    fn demo_contract_has_every_specialist_domain() {
        let snapshot = demo_snapshot();
        assert!(!snapshot.services.is_empty());
        assert!(!snapshot.logs.is_empty());
        assert!(!snapshot.mounts.is_empty());
        assert!(!snapshot.interfaces.is_empty());
        assert!(!snapshot.routes.is_empty());
        assert!(!snapshot.sockets.is_empty());
        assert!(!snapshot.findings.is_empty());
    }
}
