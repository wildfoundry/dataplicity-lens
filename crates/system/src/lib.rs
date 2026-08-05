#![forbid(unsafe_code)]

use std::{
    cmp::Ordering,
    env,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
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
    CellularModem, CellularSim, Cgroup, IoCounters, Process, ProcessId, ProcessState,
    SchemaVersion, ServiceReference, Timestamp, User,
};
#[cfg(target_os = "linux")]
use lens_platform_linux::LinuxCollector;
#[cfg(target_os = "macos")]
use lens_platform_macos::MacOsCollector;
use serde::Serialize;
use time::{OffsetDateTime, macros::format_description};

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

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

#[derive(Debug, Clone, Parser)]
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
    /// Maximum rows to collect and emit; use 0 for every available row.
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
    /// Change a service state (lens-services on Linux only).
    #[arg(long, value_enum, requires = "target")]
    pub action: Option<ServiceAction>,
    /// Exact service unit targeted by --action.
    #[arg(long, requires = "action")]
    pub target: Option<String>,
    /// Confirm a requested state change for non-interactive use.
    #[arg(long, requires = "action")]
    pub yes: bool,
    /// Print the planned state change without executing it.
    #[arg(long, requires = "action")]
    pub dry_run: bool,
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
    if args.action.is_some() {
        return run_service_action(view, &args);
    }
    if !args.json && !args.plain && !args.demo && io::stdout().is_terminal() {
        let mut terminal = CockpitTerminal::enter()?;
        specialist_loop(view, &args, &mut terminal.stdout)?;
        return Ok(());
    }
    let snapshot = if args.demo {
        demo_snapshot()
    } else {
        collect_view(view, args.since.as_deref(), &args.log_file, args.limit)
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
    }
    Ok(())
}

pub fn run_cockpit() -> Result<()> {
    let args = ViewArgs::parse();
    if generate_assets("lens", &args)? {
        return Ok(());
    }
    if args.action.is_some() {
        bail!("service actions are available through lens-services");
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
    cockpit_loop(
        &mut terminal.stdout,
        args.since.clone(),
        args.log_file.clone(),
    )
}

#[derive(Debug, Serialize)]
struct ActionOutcome<'a> {
    action: ServiceAction,
    target: &'a str,
    status: &'static str,
    verified_state: Option<String>,
}

fn run_service_action(view: View, args: &ViewArgs) -> Result<()> {
    if view != View::Services {
        bail!("--action is only supported by lens-services");
    }
    let action = args.action.context("missing --action")?;
    let target = args.target.as_deref().context("missing --target")?;
    if target.trim().is_empty()
        || target.starts_with('-')
        || target.chars().any(char::is_whitespace)
    {
        bail!("--target must be one exact service unit name");
    }
    if !args.dry_run && !args.yes {
        bail!("state changes require --yes; use --dry-run to inspect the plan safely");
    }
    if args.dry_run {
        return write_action_outcome(
            args,
            &ActionOutcome {
                action,
                target,
                status: "planned",
                verified_state: None,
            },
        );
    }
    #[cfg(target_os = "macos")]
    bail!(
        "service actions are not yet supported safely for launchd; no change was made to {target}"
    );
    #[cfg(target_os = "linux")]
    {
        let verb = match action {
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
            ServiceAction::Enable => "enable",
            ServiceAction::Disable => "disable",
        };
        let mut warnings = Vec::new();
        if command_with_timeout(
            "systemctl",
            &[verb, "--", target],
            &mut warnings,
            Duration::from_secs(15),
        )
        .is_none()
        {
            bail!("{}", warnings.join("; "));
        }
        let mut verify_warnings = Vec::new();
        let verified_state = collect_services(&mut verify_warnings)
            .into_iter()
            .find(|service| service.name == target)
            .map(|service| format!("{} / {}", service.active, service.sub));
        write_action_outcome(
            args,
            &ActionOutcome {
                action,
                target,
                status: "completed",
                verified_state,
            },
        )
    }
}

fn write_action_outcome(args: &ViewArgs, outcome: &ActionOutcome<'_>) -> Result<()> {
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), outcome)?;
        println!();
    } else {
        println!(
            "{:?} {}: {}",
            outcome.action, outcome.target, outcome.status
        );
        if let Some(state) = &outcome.verified_state {
            println!("Verified state: {state}");
        }
    }
    Ok(())
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
    stdout: TerminalWriter<io::Stdout>,
}

impl CockpitTerminal {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self {
            stdout: TerminalWriter::new(stdout),
        })
    }
}

impl Drop for CockpitTerminal {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// Raw terminal mode disables output post-processing, including the conversion
/// of `\n` to `\r\n`. Preserve normal line starts explicitly so interactive
/// screens render correctly in terminals that honour that setting (notably
/// Terminal.app on macOS).
struct TerminalWriter<W> {
    inner: W,
    trailing_carriage_return: bool,
}

impl<W> TerminalWriter<W> {
    const fn new(inner: W) -> Self {
        Self {
            inner,
            trailing_carriage_return: false,
        }
    }
}

impl<W: Write> Write for TerminalWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut converted = Vec::with_capacity(buffer.len() + 8);
        let mut previous_was_carriage_return = self.trailing_carriage_return;
        for &byte in buffer {
            if byte == b'\n' && !previous_was_carriage_return {
                converted.push(b'\r');
            }
            converted.push(byte);
            previous_was_carriage_return = byte == b'\r';
        }
        self.inner.write_all(&converted)?;
        self.trailing_carriage_return = previous_was_carriage_return;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn cockpit_loop(
    stdout: &mut impl Write,
    since: Option<String>,
    log_files: Vec<PathBuf>,
) -> Result<()> {
    let mut selected = 0usize;
    let mut snapshot = SystemSnapshot::empty(hostname());
    let mut receiver = spawn_cockpit_collection(since.clone(), log_files.clone());
    let mut loading = true;
    let mut search_query: Option<String> = None;
    let mut diagnostic = DiagnosticShell::new();
    let mut next_clock = Instant::now() + Duration::from_secs(1);
    let mut redraw = true;
    loop {
        if Instant::now() >= next_clock {
            next_clock = Instant::now() + Duration::from_secs(1);
            redraw = true;
        }
        if diagnostic.poll() {
            redraw = true;
        }
        match receiver.try_recv() {
            Ok(update) => {
                snapshot = update.snapshot;
                loading = update.loading;
                redraw = true;
            }
            Err(TryRecvError::Disconnected) if loading => {
                snapshot
                    .collection_warnings
                    .push("background collection stopped unexpectedly".into());
                loading = false;
                redraw = true;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }

        if redraw {
            let rows = terminal::size().map_or(24, |(_, rows)| rows);
            render_cockpit(&snapshot, selected, loading, rows, stdout)?;
            if let Some(query) = search_query.as_deref() {
                render_search_overlay(stdout, "Search selected view", query)?;
            }
            if diagnostic.open {
                render_diagnostic_overlay(stdout, &diagnostic)?;
            }
            redraw = false;
        }

        if event::poll(Duration::from_millis(100))? {
            let event = event::read()?;
            if matches!(event, Event::Resize(_, _)) {
                redraw = true;
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if diagnostic.open {
                diagnostic.handle_key(key);
                redraw = true;
                continue;
            }
            if let Some(query) = search_query.as_mut() {
                match key.code {
                    KeyCode::Esc => search_query = None,
                    KeyCode::Enter if !query.is_empty() => {
                        let query = search_query.take().unwrap_or_default();
                        launch_search(View::ALL[selected], &query)?;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(character);
                    }
                    _ => {}
                }
                redraw = true;
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    redraw = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = move_selection(selected, 1, View::ALL.len());
                    redraw = true;
                }
                KeyCode::Enter => {
                    launch(View::ALL[selected])?;
                    redraw = true;
                }
                KeyCode::Char('/') => {
                    search_query = Some(String::new());
                    redraw = true;
                }
                KeyCode::Char('!') => {
                    diagnostic.open = true;
                    redraw = true;
                }
                KeyCode::Char('?') => {
                    show_cockpit_help(stdout)?;
                    redraw = true;
                }
                KeyCode::Char('r') if !loading => {
                    snapshot = SystemSnapshot::empty(hostname());
                    receiver = spawn_cockpit_collection(since.clone(), log_files.clone());
                    loading = true;
                    redraw = true;
                }
                _ => {}
            }
        }
    }
}

struct CockpitUpdate {
    snapshot: SystemSnapshot,
    loading: bool,
}

fn spawn_cockpit_collection(
    since: Option<String>,
    log_files: Vec<PathBuf>,
) -> Receiver<CockpitUpdate> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let base = collect_base_snapshot();
        if sender
            .send(CockpitUpdate {
                snapshot: base.clone(),
                loading: true,
            })
            .is_err()
        {
            return;
        }
        let snapshot = enrich_snapshot(base, since.as_deref(), &log_files);
        let _ = sender.send(CockpitUpdate {
            snapshot,
            loading: false,
        });
    });
    receiver
}

struct SpecialistUpdate {
    snapshot: SystemSnapshot,
    loading_more: bool,
    status: &'static str,
}

fn send_specialist_update(
    sender: &mpsc::Sender<SpecialistUpdate>,
    snapshot: SystemSnapshot,
    args: &ViewArgs,
    loading_more: bool,
    status: &'static str,
) -> bool {
    let snapshot = filter_snapshot(
        snapshot,
        args.filter.as_deref(),
        args.service.as_deref(),
        args.process.as_deref(),
        args.severity.as_deref(),
        args.limit,
    );
    sender
        .send(SpecialistUpdate {
            snapshot,
            loading_more,
            status,
        })
        .is_ok()
}

fn spawn_specialist_collection(view: View, args: ViewArgs) -> Receiver<SpecialistUpdate> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut snapshot = domain_base_snapshot(view);
        match view {
            View::Processes => {
                let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
            }
            View::Services => {
                snapshot.services = collect_services(&mut snapshot.collection_warnings);
                let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
            }
            View::Logs => {
                #[cfg(target_os = "macos")]
                {
                    let quick_period = args.since.as_deref().unwrap_or("1m");
                    snapshot.logs = collect_logs(
                        &mut snapshot.collection_warnings,
                        Some(quick_period),
                        args.limit,
                    );
                    let file_sources = collect_file_logs(
                        &args.log_file,
                        &mut snapshot.logs,
                        &mut snapshot.collection_warnings,
                        args.limit,
                    );
                    snapshot.log_sources = vec![platform_log_source()];
                    snapshot.log_sources.extend(file_sources);
                    let loading_older = args.since.is_none();
                    if !send_specialist_update(
                        &sender,
                        snapshot.clone(),
                        &args,
                        loading_older,
                        if loading_older {
                            "Recent messages ready; loading the previous hour…"
                        } else {
                            "Ready"
                        },
                    ) || !loading_older
                    {
                        return;
                    }
                    snapshot.logs =
                        collect_logs(&mut snapshot.collection_warnings, Some("1h"), args.limit);
                    let file_sources = collect_file_logs(
                        &args.log_file,
                        &mut snapshot.logs,
                        &mut snapshot.collection_warnings,
                        args.limit,
                    );
                    snapshot.log_sources = vec![platform_log_source()];
                    snapshot.log_sources.extend(file_sources);
                    let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
                }
                #[cfg(target_os = "linux")]
                {
                    snapshot.logs = collect_logs(
                        &mut snapshot.collection_warnings,
                        args.since.as_deref(),
                        args.limit,
                    );
                    let file_sources = collect_file_logs(
                        &args.log_file,
                        &mut snapshot.logs,
                        &mut snapshot.collection_warnings,
                        args.limit,
                    );
                    snapshot.log_sources = vec![platform_log_source()];
                    snapshot.log_sources.extend(file_sources);
                    let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
                }
            }
            View::Disk => {
                let mut mounts = collect_mounts(&mut snapshot.collection_warnings);
                apply_inode_usage(&mut mounts, &mut snapshot.collection_warnings);
                snapshot.filesystems = filesystems(&mounts);
                snapshot.mounts = mounts;
                if !send_specialist_update(
                    &sender,
                    snapshot.clone(),
                    &args,
                    true,
                    "Filesystems ready; checking devices and open deleted files…",
                ) {
                    return;
                }
                snapshot.deleted_open_files =
                    collect_deleted_open_files(&mut snapshot.collection_warnings);
                snapshot.block_devices = collect_block_devices(&mut snapshot.collection_warnings);
                let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
            }
            View::Net => {
                snapshot.interfaces = collect_interfaces(&mut snapshot.collection_warnings);
                snapshot.routes = collect_routes(&mut snapshot.collection_warnings);
                if !send_specialist_update(
                    &sender,
                    snapshot.clone(),
                    &args,
                    true,
                    "Interfaces and routes ready; checking listeners…",
                ) {
                    return;
                }
                snapshot.sockets = collect_sockets(&mut snapshot.collection_warnings);
                snapshot.cellular_modems = collect_cellular(&mut snapshot.collection_warnings);
                let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
            }
            View::Health => {
                snapshot.services = collect_services(&mut snapshot.collection_warnings);
                let mut mounts = collect_mounts(&mut snapshot.collection_warnings);
                apply_inode_usage(&mut mounts, &mut snapshot.collection_warnings);
                snapshot.filesystems = filesystems(&mounts);
                snapshot.mounts = mounts;
                snapshot.findings = diagnose(&snapshot);
                if !send_specialist_update(
                    &sender,
                    snapshot.clone(),
                    &args,
                    true,
                    "Core checks ready; checking logs, network and open files…",
                ) {
                    return;
                }
                #[cfg(target_os = "macos")]
                let log_since = args.since.as_deref().or(Some("1m"));
                #[cfg(target_os = "linux")]
                let log_since = args.since.as_deref();
                snapshot.logs =
                    collect_logs(&mut snapshot.collection_warnings, log_since, args.limit);
                let file_sources = collect_file_logs(
                    &args.log_file,
                    &mut snapshot.logs,
                    &mut snapshot.collection_warnings,
                    args.limit,
                );
                snapshot.log_sources = vec![platform_log_source()];
                snapshot.log_sources.extend(file_sources);
                snapshot.interfaces = collect_interfaces(&mut snapshot.collection_warnings);
                snapshot.routes = collect_routes(&mut snapshot.collection_warnings);
                snapshot.sockets = collect_sockets(&mut snapshot.collection_warnings);
                snapshot.deleted_open_files =
                    collect_deleted_open_files(&mut snapshot.collection_warnings);
                snapshot.block_devices = collect_block_devices(&mut snapshot.collection_warnings);
                snapshot.findings = diagnose(&snapshot);
                snapshot
                    .relationships
                    .extend(domain_relationships(&snapshot));
                let _ = send_specialist_update(&sender, snapshot, &args, false, "Ready");
            }
        }
    });
    receiver
}

fn render_cockpit(
    snapshot: &SystemSnapshot,
    selected: usize,
    loading: bool,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    let host = &snapshot.host;
    let columns = terminal::size().map_or(88, |(width, _)| width);
    if columns < 36 || rows < 10 {
        writeln!(stdout, "LENS")?;
        writeln!(stdout, "Terminal too small ({columns}x{rows}).")?;
        writeln!(stdout, "Resize to at least 36x10 or press q to quit.")?;
        stdout.flush()?;
        return Ok(());
    }
    let width = usize::from(columns);
    let colour = terminal_colour_enabled();
    let rule = "─".repeat(width.saturating_sub(2));
    writeln!(stdout, "{}", ink(&format!("╭{rule}╮"), Ink::Border, colour))?;
    if width >= 64 {
        writeln!(
            stdout,
            "  {}{}{}  {}  {}  {}{}",
            ink("DATAPLICITY", Ink::Brand, colour),
            ink(" / ", Ink::Muted, colour),
            ink("LENS", Ink::Bright, colour),
            ink("◆", Ink::Border, colour),
            ink(
                &truncate_text(
                    &host.hostname,
                    width.saturating_sub(if width >= 88 { 52 } else { 42 }),
                ),
                Ink::Info,
                colour
            ),
            if loading {
                badge(" CHECKING ", Ink::Attention, colour)
            } else {
                badge(" LIVE ", Ink::Success, colour)
            },
            if width >= 88 {
                format!("  {}", ink(&local_clock(), Ink::Muted, colour))
            } else {
                String::new()
            }
        )?;
    } else {
        writeln!(
            stdout,
            "  {}  {}  {}",
            ink("LENS", Ink::Brand, colour),
            ink(
                &truncate_text(&host.hostname, width.saturating_sub(24)),
                Ink::Info,
                colour
            ),
            if loading {
                badge(" … ", Ink::Attention, colour)
            } else {
                badge(" LIVE ", Ink::Success, colour)
            },
        )?;
    }
    if rows >= 30 {
        writeln!(
            stdout,
            "  {}",
            ink(
                &format!(
                    "{}{}{}",
                    host.os_name
                        .as_deref()
                        .unwrap_or("Operating system unknown"),
                    if width >= 72 {
                        format!("  •  kernel {}", host.kernel)
                    } else {
                        String::new()
                    },
                    if width >= 52 {
                        format!("  •  up {}", human_duration(host.uptime_seconds))
                    } else {
                        String::new()
                    }
                ),
                Ink::Muted,
                colour,
            )
        )?;
    }
    writeln!(stdout, "{}", ink(&format!("├{rule}┤"), Ink::Border, colour))?;
    writeln!(
        stdout,
        "  {} {}    {} {}    {} {}",
        ink("CPU", Ink::Label, colour),
        ink(&format!("{:>5.1}%", host.cpu_percent), Ink::Info, colour),
        ink("Memory", Ink::Label, colour),
        ink(
            &format!("{:>5.1}%", host.memory.used_percent()),
            Ink::Attention,
            colour,
        ),
        ink("LOAD", Ink::Label, colour),
        ink(
            &format!(
                "{:.2}  {:.2}  {:.2}",
                host.load.one, host.load.five, host.load.fifteen
            ),
            Ink::Info,
            colour,
        ),
    )?;
    if width >= 52 && rows >= 30 {
        writeln!(
            stdout,
            "  {} {}   {} {}{}{}",
            ink("TASKS", Ink::Label, colour),
            ink(&host.process_counts.total.to_string(), Ink::Bright, colour),
            ink("RUNNING", Ink::Label, colour),
            ink(
                &host.process_counts.running.to_string(),
                Ink::Success,
                colour
            ),
            if width >= 76 {
                format!(
                    "   {} {}",
                    ink("SLEEPING", Ink::Label, colour),
                    ink(
                        &host.process_counts.sleeping.to_string(),
                        Ink::Muted,
                        colour
                    )
                )
            } else {
                String::new()
            },
            if width >= 64 {
                format!(
                    "   {} {}",
                    ink("ZOMBIES", Ink::Label, colour),
                    ink(
                        &host.process_counts.zombie.to_string(),
                        Ink::Critical,
                        colour
                    )
                )
            } else {
                String::new()
            },
        )?;
    }

    if rows >= 32 {
        let mut processes: Vec<_> = snapshot.processes.iter().collect();
        processes.sort_by(|left, right| {
            right
                .cpu_percent
                .partial_cmp(&left.cpu_percent)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    right
                        .memory_percent
                        .partial_cmp(&left.memory_percent)
                        .unwrap_or(Ordering::Equal)
                })
        });
        writeln!(
            stdout,
            "\n  {}",
            ink("BUSIEST PROCESSES", Ink::Label, colour)
        )?;
        let extra = usize::from(rows.saturating_sub(36));
        let process_capacity = if rows < 36 {
            usize::from(rows.saturating_sub(31)).max(1)
        } else {
            3 + (extra * 2 / 3)
        };
        for process in processes.into_iter().take(process_capacity) {
            if width >= 70 {
                writeln!(
                    stdout,
                    "  {}  {}  {} {}  {} {}",
                    ink(&format!("{:>6}", process.pid), Ink::Muted, colour),
                    ink(
                        &format!(
                            "{:<width$}",
                            truncate_text(&process.name, width.saturating_sub(50)),
                            width = width.saturating_sub(50)
                        ),
                        Ink::Bright,
                        colour
                    ),
                    ink("CPU", Ink::Label, colour),
                    ink(&format!("{:>5.1}%", process.cpu_percent), Ink::Info, colour),
                    ink("MEM", Ink::Label, colour),
                    ink(
                        &format!("{:>5.1}%", process.memory_percent),
                        Ink::Attention,
                        colour
                    ),
                )?;
            } else {
                writeln!(
                    stdout,
                    "  {}  {}  {}",
                    ink(&format!("{:>6}", process.pid), Ink::Muted, colour),
                    ink(
                        &format!(
                            "{:<width$}",
                            truncate_text(&process.name, width.saturating_sub(24)),
                            width = width.saturating_sub(24)
                        ),
                        Ink::Bright,
                        colour
                    ),
                    ink(&format!("{:>5.1}%", process.cpu_percent), Ink::Info, colour),
                )?;
            }
        }
    }

    let critical = snapshot
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Critical)
        .count();
    let attention = snapshot
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Attention)
        .count();
    writeln!(stdout, "\n  {}", ink("HEALTH", Ink::Label, colour))?;
    if loading {
        writeln!(
            stdout,
            "  {}",
            ink(
                "Checking services, logs, storage and network in the background…",
                Ink::Muted,
                colour,
            )
        )?;
    } else if snapshot.findings.is_empty() {
        writeln!(stdout, "  {}", badge(" ALL CLEAR ", Ink::Success, colour))?;
    } else {
        writeln!(
            stdout,
            "  {}  {}  {}",
            badge(&format!(" {critical} CRITICAL "), Ink::Critical, colour),
            badge(&format!(" {attention} ATTENTION "), Ink::Attention, colour),
            ink(
                &format!("{critical} critical · {attention} attention"),
                Ink::Muted,
                colour,
            ),
        )?;
        if rows >= 36 {
            let extra = usize::from(rows.saturating_sub(36));
            let finding_capacity = 2 + (extra - extra * 2 / 3);
            for finding in snapshot.findings.iter().take(finding_capacity) {
                writeln!(
                    stdout,
                    "  {} {}",
                    ink("•", Ink::Critical, colour),
                    ink(
                        &truncate_text(&finding.title, width.saturating_sub(6)),
                        Ink::Muted,
                        colour
                    )
                )?;
            }
        }
    }

    writeln!(stdout, "\n  {}", ink("EXPLORE", Ink::Label, colour))?;
    for (index, view) in View::ALL.iter().enumerate() {
        let marker = if index == selected { "▶" } else { " " };
        let summary = cockpit_view_summary(*view, snapshot, loading);
        let row = if width >= 48 {
            format!(
                "{marker} {:<12} {}",
                view.title(),
                truncate_text(&summary, width.saturating_sub(18))
            )
        } else {
            format!("{marker} {}", view.title())
        };
        if index == selected {
            writeln!(stdout, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(stdout, "{}", ink(&row, Ink::Muted, colour))?;
        }
    }
    writeln!(
        stdout,
        "\n{}",
        ink(&format!("├{rule}┤"), Ink::Border, colour)
    )?;
    if width >= 82 {
        writeln!(
            stdout,
            "  {} {}   {} {}   {} {}   {} {}   {} {}   {} {}",
            keycap("↑↓", colour),
            ink("move", Ink::Muted, colour),
            keycap("↵", colour),
            ink("open", Ink::Muted, colour),
            keycap("/", colour),
            ink("search", Ink::Muted, colour),
            keycap("?", colour),
            ink("help", Ink::Muted, colour),
            keycap("r", colour),
            ink("refresh", Ink::Muted, colour),
            keycap("q", colour),
            ink("quit", Ink::Muted, colour)
        )?;
    } else {
        writeln!(
            stdout,
            "  {} {}   {} {}   {} {}",
            keycap("↑↓", colour),
            ink("move", Ink::Muted, colour),
            keycap("↵", colour),
            ink("open", Ink::Muted, colour),
            keycap("q", colour),
            ink("quit", Ink::Muted, colour)
        )?;
    }
    if rows >= 22 {
        writeln!(stdout, "{}", ink(&format!("╰{rule}╯"), Ink::Border, colour))?;
    }
    stdout.flush()?;
    Ok(())
}

fn cockpit_view_summary(view: View, snapshot: &SystemSnapshot, loading: bool) -> String {
    if loading && (view != View::Processes || snapshot.processes.is_empty()) {
        return "checking…".into();
    }
    match view {
        View::Processes => format!("{} processes", snapshot.processes.len()),
        View::Services => format!("{} services", snapshot.services.len()),
        View::Logs => format!("{} recent entries", snapshot.logs.len()),
        View::Disk => snapshot
            .mounts
            .iter()
            .find(|mount| mount.target == "/")
            .map_or_else(
                || format!("{} mounts", snapshot.mounts.len()),
                |root| format!("root filesystem · {:.0}% used", root.used_percent),
            ),
        View::Net => format!(
            "{} interfaces · {} listeners · {} modems",
            snapshot.interfaces.len(),
            snapshot.sockets.len(),
            snapshot.cellular_modems.len()
        ),
        View::Health => format!("{} findings", snapshot.findings.len()),
    }
}

#[derive(Debug, Clone, Copy)]
enum Ink {
    Bright,
    Brand,
    Info,
    Success,
    Attention,
    Critical,
    Label,
    Muted,
    Border,
}

impl Ink {
    const fn foreground(self) -> &'static str {
        match self {
            Self::Bright => "1;38;2;238;243;252",
            Self::Brand => "1;38;2;190;125;255",
            Self::Info => "1;38;2;91;215;255",
            Self::Success => "1;38;2;88;224;166",
            Self::Attention => "1;38;2;255;190;92",
            Self::Critical => "1;38;2;255;105;125",
            Self::Label => "1;38;2;139;155;180",
            Self::Muted => "38;2;125;140;165",
            Self::Border => "38;2;48;62;84",
        }
    }

    const fn badge_colours(self) -> &'static str {
        match self {
            Self::Success => "1;38;2;112;239;188;48;2;17;62;50",
            Self::Attention => "1;38;2;255;201;112;48;2;81;52;20",
            Self::Critical => "1;38;2;255;166;178;48;2;92;32;45",
            Self::Brand => "1;38;2;222;193;255;48;2;70;49;104",
            _ => "1;38;2;207;216;230;48;2;31;42;61",
        }
    }
}

fn terminal_colour_enabled() -> bool {
    env::var_os("NO_COLOR").is_none() && env::var("TERM").is_ok_and(|term| term != "dumb")
}

fn ink(text: &str, colour: Ink, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{}m{text}\x1b[0m", colour.foreground())
    } else {
        text.to_owned()
    }
}

fn badge(text: &str, colour: Ink, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{}m{text}\x1b[0m", colour.badge_colours())
    } else {
        format!("[{text}]")
    }
}

fn keycap(key: &str, enabled: bool) -> String {
    badge(&format!(" {key} "), Ink::Brand, enabled)
}

fn selected_row(text: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[1;38;2;242;235;255;48;2;70;49;104m{text}\x1b[0m")
    } else {
        format!(">{text}")
    }
}

fn move_selection(selected: usize, delta: isize, length: usize) -> usize {
    selected
        .saturating_add_signed(delta)
        .min(length.saturating_sub(1))
}

fn specialist_item_count(view: View, snapshot: &SystemSnapshot) -> usize {
    match view {
        View::Services => snapshot.services.len(),
        View::Logs => snapshot.logs.len(),
        View::Disk => snapshot.mounts.len() + snapshot.block_devices.len(),
        View::Net => {
            snapshot.interfaces.len()
                + snapshot.routes.len()
                + snapshot.sockets.len()
                + snapshot.cellular_modems.len()
        }
        View::Health => snapshot.findings.len(),
        View::Processes => 0,
    }
}

fn specialist_has_data(view: View, snapshot: &SystemSnapshot) -> bool {
    match view {
        View::Processes => !snapshot.processes.is_empty(),
        View::Services => !snapshot.services.is_empty(),
        View::Logs => !snapshot.logs.is_empty(),
        View::Disk => !snapshot.mounts.is_empty() || !snapshot.block_devices.is_empty(),
        View::Net => {
            !snapshot.interfaces.is_empty()
                || !snapshot.routes.is_empty()
                || !snapshot.cellular_modems.is_empty()
        }
        View::Health => !snapshot.findings.is_empty(),
    }
}

fn preserve_specialist_selection(
    view: View,
    previous: &SystemSnapshot,
    next: &SystemSnapshot,
    selected: usize,
) -> usize {
    match view {
        View::Logs => previous
            .logs
            .get(selected)
            .and_then(|current| {
                next.logs.iter().position(|candidate| {
                    candidate.timestamp == current.timestamp
                        && candidate.source == current.source
                        && candidate.message == current.message
                })
            })
            .unwrap_or_else(|| selected.min(next.logs.len().saturating_sub(1))),
        View::Health => previous
            .findings
            .get(selected)
            .and_then(|current| {
                next.findings
                    .iter()
                    .position(|candidate| candidate.id == current.id)
            })
            .unwrap_or_else(|| selected.min(next.findings.len().saturating_sub(1))),
        _ => selected.min(specialist_item_count(view, next).saturating_sub(1)),
    }
}

fn truncate_text(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut value: String = text.chars().take(width - 1).collect();
    value.push('…');
    value
}

fn viewport_start(selected: usize, length: usize, capacity: usize) -> usize {
    if capacity == 0 || length <= capacity {
        0
    } else {
        selected
            .saturating_sub(capacity / 2)
            .min(length.saturating_sub(capacity))
    }
}

fn severity_ink(severity: Severity) -> Ink {
    match severity {
        Severity::Information => Ink::Info,
        Severity::Attention => Ink::Attention,
        Severity::Critical => Ink::Critical,
    }
}

fn log_ink(priority: Option<&str>) -> Ink {
    match priority.unwrap_or_default().to_ascii_lowercase().as_str() {
        "emerg" | "alert" | "crit" | "critical" | "err" | "error" => Ink::Critical,
        "warning" | "warn" | "notice" => Ink::Attention,
        _ => Ink::Muted,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_specialist(
    view: View,
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    loading: bool,
    status: &str,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    let columns = terminal::size().map_or(100, |(width, _)| width);
    if columns < 36 || rows < 10 {
        writeln!(stdout, "LENS / {}", view.title().to_ascii_uppercase())?;
        writeln!(stdout, "Terminal too small ({columns}x{rows}).")?;
        writeln!(stdout, "Resize to at least 36x10 or press q to quit.")?;
        stdout.flush()?;
        return Ok(());
    }
    let width = usize::from(columns);
    let colour = terminal_colour_enabled();
    let rule = "─".repeat(width.saturating_sub(2));
    writeln!(stdout, "{}", ink(&format!("╭{rule}╮"), Ink::Border, colour))?;
    if width >= 64 {
        writeln!(
            stdout,
            "  {}{}{}  {}  {}  {}{}",
            ink("DATAPLICITY", Ink::Brand, colour),
            ink(" / ", Ink::Muted, colour),
            ink(&view.title().to_ascii_uppercase(), Ink::Bright, colour),
            ink("◆", Ink::Border, colour),
            ink(
                &truncate_text(
                    &snapshot.host.hostname,
                    width.saturating_sub(if width >= 88 { 52 } else { 42 }),
                ),
                Ink::Info,
                colour
            ),
            if loading {
                badge(" LOADING ", Ink::Attention, colour)
            } else {
                badge(" LIVE ", Ink::Success, colour)
            },
            if width >= 88 {
                format!("  {}", ink(&local_clock(), Ink::Muted, colour))
            } else {
                String::new()
            }
        )?;
    } else {
        writeln!(
            stdout,
            "  {}  {}  {}",
            ink(&view.title().to_ascii_uppercase(), Ink::Brand, colour),
            ink(
                &truncate_text(&snapshot.host.hostname, width.saturating_sub(24)),
                Ink::Info,
                colour
            ),
            if loading {
                badge(" … ", Ink::Attention, colour)
            } else {
                badge(" LIVE ", Ink::Success, colour)
            }
        )?;
    }
    writeln!(
        stdout,
        "  {}",
        ink(
            &truncate_text(status, width.saturating_sub(4)),
            Ink::Muted,
            colour
        )
    )?;
    writeln!(stdout, "{}", ink(&format!("├{rule}┤"), Ink::Border, colour))?;

    match view {
        View::Health if loading && !specialist_has_data(view, snapshot) => writeln!(
            stdout,
            "\n  {}",
            ink("Running system checks…", Ink::Muted, colour)
        )?,
        View::Services => {
            render_service_specialist(snapshot, selected, inspecting, rows, width, stdout)?
        }
        View::Logs => render_log_specialist(snapshot, selected, inspecting, rows, width, stdout)?,
        View::Disk => render_disk_specialist(snapshot, selected, inspecting, rows, width, stdout)?,
        View::Net => render_net_specialist(snapshot, selected, inspecting, rows, width, stdout)?,
        View::Health => {
            render_health_specialist(snapshot, selected, inspecting, rows, width, stdout)?
        }
        _ if loading && !specialist_has_data(view, snapshot) => writeln!(
            stdout,
            "\n  {}",
            ink(
                &format!("Loading {} data…", view.title().to_ascii_lowercase()),
                Ink::Muted,
                colour,
            )
        )?,
        _ => render_plain(view, snapshot, stdout)?,
    }

    writeln!(
        stdout,
        "\n{}",
        ink(&format!("├{rule}┤"), Ink::Border, colour)
    )?;
    if width < 64 {
        writeln!(
            stdout,
            "  {} {}   {} {}   {} {}",
            keycap("↑↓", colour),
            ink("move", Ink::Muted, colour),
            keycap("↵", colour),
            ink("open", Ink::Muted, colour),
            keycap("q", colour),
            ink("quit", Ink::Muted, colour)
        )?;
    } else if width >= 80 && view != View::Processes && specialist_item_count(view, snapshot) > 0 {
        if inspecting {
            writeln!(
                stdout,
                "  {} {}   {} {}   {} {}",
                keycap("Esc", colour),
                ink("back", Ink::Muted, colour),
                keycap("r", colour),
                ink("refresh", Ink::Muted, colour),
                keycap("q", colour),
                ink("quit", Ink::Muted, colour),
            )?;
        } else {
            writeln!(
                stdout,
                "  {} {}   {} {}   {} {}   {} {}   {} {}   {} {}",
                keycap("↑↓", colour),
                ink("move", Ink::Muted, colour),
                keycap("↵", colour),
                ink("inspect", Ink::Muted, colour),
                keycap("/", colour),
                ink("search", Ink::Muted, colour),
                keycap("r", colour),
                ink("refresh", Ink::Muted, colour),
                keycap("!", colour),
                ink("shell", Ink::Muted, colour),
                keycap("q", colour),
                ink("quit", Ink::Muted, colour),
            )?;
        }
    } else {
        writeln!(
            stdout,
            "  {} {}   {} {}   {} {}",
            keycap("r", colour),
            ink("refresh", Ink::Muted, colour),
            keycap("!", colour),
            ink("shell", Ink::Muted, colour),
            keycap("q", colour),
            ink("quit", Ink::Muted, colour),
        )?;
    }
    writeln!(stdout, "{}", ink(&format!("╰{rule}╯"), Ink::Border, colour))?;
    stdout.flush()?;
    Ok(())
}

fn render_log_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    if snapshot.logs.is_empty() {
        writeln!(
            out,
            "\n  {}",
            ink("No matching log messages yet.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    let item = &snapshot.logs[selected.min(snapshot.logs.len() - 1)];
    if inspecting {
        writeln!(out, "\n  {}", ink("LOG MESSAGE", Ink::Label, colour))?;
        writeln!(
            out,
            "  {}  {}",
            badge(
                &format!(
                    " {} ",
                    item.priority
                        .as_deref()
                        .unwrap_or("info")
                        .to_ascii_uppercase()
                ),
                log_ink(item.priority.as_deref()),
                colour,
            ),
            ink(&item.timestamp, Ink::Muted, colour),
        )?;
        writeln!(
            out,
            "  {} {}",
            ink("SOURCE", Ink::Label, colour),
            ink(&item.source, Ink::Bright, colour)
        )?;
        if let Some(unit) = &item.unit {
            writeln!(
                out,
                "  {} {}",
                ink("SERVICE", Ink::Label, colour),
                ink(unit, Ink::Info, colour)
            )?;
        }
        if item.repeated > 1 {
            writeln!(
                out,
                "  {} {} times",
                ink("REPEATED", Ink::Label, colour),
                item.repeated
            )?;
        }
        writeln!(out, "\n  {}", ink("MESSAGE", Ink::Label, colour))?;
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(&item.message, width.saturating_sub(4)),
                Ink::Bright,
                colour
            )
        )?;
        return Ok(());
    }

    if width >= 70 {
        writeln!(
            out,
            "  {}  {}  {}  {}",
            ink("LEVEL", Ink::Label, colour),
            ink("TIME", Ink::Label, colour),
            ink("SOURCE", Ink::Label, colour),
            ink("MESSAGE", Ink::Label, colour)
        )?;
    } else {
        writeln!(
            out,
            "  {}  {}",
            ink("TIME", Ink::Label, colour),
            ink("MESSAGE", Ink::Label, colour)
        )?;
    }
    let capacity = usize::from(rows.saturating_sub(9)).max(1);
    let start = viewport_start(selected, snapshot.logs.len(), capacity);
    let message_width = width.saturating_sub(50).max(16);
    for (index, item) in snapshot.logs.iter().enumerate().skip(start).take(capacity) {
        let priority = item.priority.as_deref().unwrap_or("info");
        let time = item.timestamp.get(11..19).unwrap_or(&item.timestamp);
        let source = item.unit.as_deref().unwrap_or(&item.source);
        let repeat = if item.repeated > 1 {
            format!(" ×{}", item.repeated)
        } else {
            String::new()
        };
        let row = if width >= 70 {
            format!(
                "  {:<8} {:<8} {:<22} {}{}",
                truncate_text(priority, 8),
                time,
                truncate_text(source, 22),
                truncate_text(&item.message, message_width.saturating_sub(repeat.len())),
                repeat
            )
        } else {
            format!(
                "  {:<8} {}{}",
                time,
                truncate_text(&item.message, width.saturating_sub(14 + repeat.len())),
                repeat
            )
        };
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(
                out,
                "{}",
                ink(&row, log_ink(item.priority.as_deref()), colour)
            )?;
        }
    }
    Ok(())
}

fn render_service_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    if snapshot.services.is_empty() {
        writeln!(
            out,
            "\n  {}",
            ink("No matching services.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    let service = &snapshot.services[selected.min(snapshot.services.len() - 1)];
    if inspecting {
        writeln!(out, "\n  {}", ink("SERVICE", Ink::Label, colour))?;
        writeln!(out, "  {}", ink(&service.name, Ink::Bright, colour))?;
        writeln!(
            out,
            "  {} {:<12}  {} {}",
            ink("STATE", Ink::Label, colour),
            service.active,
            ink("DETAIL", Ink::Label, colour),
            service.sub
        )?;
        writeln!(
            out,
            "  {} {}",
            ink("LOAD", Ink::Label, colour),
            service.load
        )?;
        if let Some(restarts) = service.restart_count {
            writeln!(
                out,
                "  {} {}",
                ink("RESTARTS", Ink::Label, colour),
                restarts
            )?;
        }
        writeln!(out, "\n  {}", ink("DESCRIPTION", Ink::Label, colour))?;
        writeln!(
            out,
            "  {}",
            truncate_text(&service.description, width.saturating_sub(4))
        )?;
        return Ok(());
    }
    if width >= 70 {
        writeln!(
            out,
            "  {:<34} {:<12} {:<14} {}",
            ink("SERVICE", Ink::Label, colour),
            ink("ACTIVE", Ink::Label, colour),
            ink("STATE", Ink::Label, colour),
            ink("DESCRIPTION", Ink::Label, colour)
        )?;
    } else {
        writeln!(
            out,
            "  {:<width$} {}",
            ink("SERVICE", Ink::Label, colour),
            ink("STATE", Ink::Label, colour),
            width = width.saturating_sub(16)
        )?;
    }
    let capacity = usize::from(rows.saturating_sub(9)).max(1);
    let start = viewport_start(selected, snapshot.services.len(), capacity);
    for (index, service) in snapshot
        .services
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
    {
        let row = if width >= 70 {
            format!(
                "  {:<34} {:<12} {:<14} {}",
                truncate_text(&service.name, 34),
                service.active,
                service.sub,
                truncate_text(&service.description, width.saturating_sub(66).max(4))
            )
        } else {
            format!(
                "  {:<name_width$} {}",
                truncate_text(&service.name, width.saturating_sub(16)),
                service.active,
                name_width = width.saturating_sub(16)
            )
        };
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(
                out,
                "{}",
                ink(
                    &row,
                    if service.active == "failed" {
                        Ink::Critical
                    } else if service.active == "active" {
                        Ink::Bright
                    } else {
                        Ink::Muted
                    },
                    colour
                )
            )?;
        }
    }
    Ok(())
}

fn render_disk_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    let count = snapshot.mounts.len() + snapshot.block_devices.len();
    if count == 0 {
        writeln!(
            out,
            "\n  {}",
            ink("No storage data is available.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    if inspecting {
        if let Some(mount) = snapshot.mounts.get(selected) {
            writeln!(out, "\n  {}", ink("MOUNT", Ink::Label, colour))?;
            writeln!(out, "  {}", ink(&mount.target, Ink::Bright, colour))?;
            writeln!(
                out,
                "  {} {}",
                ink("SOURCE", Ink::Label, colour),
                mount.source
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("FILESYSTEM", Ink::Label, colour),
                mount.filesystem
            )?;
            writeln!(
                out,
                "  {} {:.1}%  ({} used, {} available)",
                ink("CAPACITY", Ink::Label, colour),
                mount.used_percent,
                human_bytes(mount.used_bytes),
                human_bytes(mount.available_bytes)
            )?;
            if let (Some(used), Some(total)) = (mount.inode_used, mount.inode_total) {
                writeln!(
                    out,
                    "  {} {used} of {total}",
                    ink("INODES", Ink::Label, colour)
                )?;
            }
        } else {
            let device = &snapshot.block_devices[selected - snapshot.mounts.len()];
            writeln!(out, "\n  {}", ink("BLOCK DEVICE", Ink::Label, colour))?;
            writeln!(out, "  {}", ink(&device.name, Ink::Bright, colour))?;
            writeln!(out, "  {} {}", ink("TYPE", Ink::Label, colour), device.kind)?;
            writeln!(
                out,
                "  {} {}",
                ink("SIZE", Ink::Label, colour),
                human_bytes(device.size_bytes)
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("MOUNTS", Ink::Label, colour),
                if device.mountpoints.is_empty() {
                    "-".into()
                } else {
                    device.mountpoints.join(", ")
                }
            )?;
        }
        return Ok(());
    }
    if width >= 70 {
        writeln!(
            out,
            "  {:<9} {:<34} {:>9}  {}",
            ink("TYPE", Ink::Label, colour),
            ink("TARGET", Ink::Label, colour),
            ink("USE", Ink::Label, colour),
            ink("SOURCE", Ink::Label, colour)
        )?;
    } else {
        writeln!(
            out,
            "  {:<8} {:<width$} {}",
            ink("TYPE", Ink::Label, colour),
            ink("TARGET", Ink::Label, colour),
            ink("USE", Ink::Label, colour),
            width = width.saturating_sub(23)
        )?;
    }
    let capacity = usize::from(rows.saturating_sub(9)).max(1);
    let start = viewport_start(selected, count, capacity);
    for index in start..(start + capacity).min(count) {
        let row = if width < 70 {
            if let Some(mount) = snapshot.mounts.get(index) {
                format!(
                    "  {:<8} {:<target_width$} {:>6.1}%",
                    "mount",
                    truncate_text(&mount.target, width.saturating_sub(23)),
                    mount.used_percent,
                    target_width = width.saturating_sub(23)
                )
            } else {
                let device = &snapshot.block_devices[index - snapshot.mounts.len()];
                format!(
                    "  {:<8} {:<target_width$} {:>7}",
                    "device",
                    truncate_text(&device.name, width.saturating_sub(23)),
                    human_bytes(device.size_bytes),
                    target_width = width.saturating_sub(23)
                )
            }
        } else if let Some(mount) = snapshot.mounts.get(index) {
            format!(
                "  {:<9} {:<34} {:>8.1}%  {}",
                "mount",
                truncate_text(&mount.target, 34),
                mount.used_percent,
                truncate_text(&mount.source, width.saturating_sub(60).max(12))
            )
        } else {
            let device = &snapshot.block_devices[index - snapshot.mounts.len()];
            format!(
                "  {:<9} {:<34} {:>9}  {}",
                "device",
                truncate_text(&device.name, 34),
                human_bytes(device.size_bytes),
                device.kind
            )
        };
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(out, "{}", ink(&row, Ink::Muted, colour))?;
        }
    }
    Ok(())
}

fn render_net_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    let interface_end = snapshot.interfaces.len();
    let route_end = interface_end + snapshot.routes.len();
    let socket_end = route_end + snapshot.sockets.len();
    let count = socket_end + snapshot.cellular_modems.len();
    if count == 0 {
        writeln!(
            out,
            "\n  {}",
            ink("No network data is available.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    if inspecting {
        if let Some(interface) = snapshot.interfaces.get(selected) {
            writeln!(out, "\n  {}", ink("INTERFACE", Ink::Label, colour))?;
            writeln!(
                out,
                "  {}  {}",
                ink(&interface.name, Ink::Bright, colour),
                badge(
                    &format!(" {} ", interface.state),
                    if interface.state.eq_ignore_ascii_case("up") {
                        Ink::Success
                    } else {
                        Ink::Muted
                    },
                    colour
                )
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("ADDRESSES", Ink::Label, colour),
                if interface.addresses.is_empty() {
                    "-".into()
                } else {
                    interface.addresses.join(", ")
                }
            )?;
        } else if selected < route_end {
            let route = &snapshot.routes[selected - interface_end];
            writeln!(out, "\n  {}", ink("ROUTE", Ink::Label, colour))?;
            writeln!(out, "  {}", ink(&route.raw, Ink::Bright, colour))?;
            writeln!(
                out,
                "  {} {}",
                ink("DESTINATION", Ink::Label, colour),
                route.destination
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("GATEWAY", Ink::Label, colour),
                route.gateway.as_deref().unwrap_or("-")
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("INTERFACE", Ink::Label, colour),
                route.interface.as_deref().unwrap_or("-")
            )?;
        } else if selected < socket_end {
            let socket = &snapshot.sockets[selected - route_end];
            writeln!(out, "\n  {}", ink("LISTENER", Ink::Label, colour))?;
            writeln!(out, "  {}", ink(&socket.local, Ink::Bright, colour))?;
            writeln!(
                out,
                "  {} {}",
                ink("PROTOCOL", Ink::Label, colour),
                socket.protocol
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("STATE", Ink::Label, colour),
                socket.state
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("OWNER", Ink::Label, colour),
                socket.owner.as_deref().unwrap_or("unavailable")
            )?;
        } else {
            let modem = &snapshot.cellular_modems[selected - socket_end];
            writeln!(out, "\n  {}", ink("CELLULAR MODEM", Ink::Label, colour))?;
            writeln!(
                out,
                "  {}",
                ink(
                    modem.model.as_deref().unwrap_or(&modem.path),
                    Ink::Bright,
                    colour
                )
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("STATE", Ink::Label, colour),
                modem.state
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("NETWORK", Ink::Label, colour),
                modem.operator_name.as_deref().unwrap_or("unavailable")
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("RADIO", Ink::Label, colour),
                if modem.access_technologies.is_empty() {
                    "unavailable".into()
                } else {
                    modem.access_technologies.join(", ")
                }
            )?;
            if let Some(signal) = modem.signal_quality_percent {
                writeln!(out, "  {} {signal}%", ink("SIGNAL", Ink::Label, colour))?;
            }
            if let Some(sim) = &modem.sim {
                writeln!(out, "  {} {}", ink("SIM", Ink::Label, colour), sim.path)?;
                if let Some(iccid) = &sim.iccid {
                    writeln!(out, "  {} {iccid}", ink("ICCID", Ink::Label, colour))?;
                }
            }
        }
        return Ok(());
    }
    if width >= 70 {
        writeln!(
            out,
            "  {:<11} {:<18} {:<12} {}",
            ink("TYPE", Ink::Label, colour),
            ink("NAME", Ink::Label, colour),
            ink("STATE", Ink::Label, colour),
            ink("DETAIL", Ink::Label, colour)
        )?;
    } else {
        writeln!(
            out,
            "  {:<10} {:<name_width$} {}",
            ink("TYPE", Ink::Label, colour),
            ink("NAME", Ink::Label, colour),
            ink("STATE", Ink::Label, colour),
            name_width = width.saturating_sub(25)
        )?;
    }
    let capacity = usize::from(rows.saturating_sub(9)).max(1);
    let start = viewport_start(selected, count, capacity);
    for index in start..(start + capacity).min(count) {
        let row = if width < 70 {
            if let Some(interface) = snapshot.interfaces.get(index) {
                format!(
                    "  {:<10} {:<name_width$} {}",
                    "interface",
                    truncate_text(&interface.name, width.saturating_sub(25)),
                    interface.state,
                    name_width = width.saturating_sub(25)
                )
            } else if index < route_end {
                let route = &snapshot.routes[index - interface_end];
                format!(
                    "  {:<10} {:<name_width$} {}",
                    "route",
                    truncate_text(&route.destination, width.saturating_sub(25)),
                    route.interface.as_deref().unwrap_or("-"),
                    name_width = width.saturating_sub(25)
                )
            } else if index < socket_end {
                let socket = &snapshot.sockets[index - route_end];
                format!(
                    "  {:<10} {:<name_width$} {}",
                    "listener",
                    truncate_text(&socket.local, width.saturating_sub(25)),
                    socket.state,
                    name_width = width.saturating_sub(25)
                )
            } else {
                let modem = &snapshot.cellular_modems[index - socket_end];
                format!(
                    "  {:<10} {:<name_width$} {}",
                    "cellular",
                    truncate_text(
                        modem.model.as_deref().unwrap_or(&modem.path),
                        width.saturating_sub(25)
                    ),
                    modem.state,
                    name_width = width.saturating_sub(25)
                )
            }
        } else if let Some(interface) = snapshot.interfaces.get(index) {
            format!(
                "  {:<11} {:<18} {:<12} {}",
                "interface",
                truncate_text(&interface.name, 18),
                interface.state,
                truncate_text(
                    &interface.addresses.join(" "),
                    width.saturating_sub(47).max(12)
                )
            )
        } else if index < route_end {
            let route = &snapshot.routes[index - interface_end];
            format!(
                "  {:<11} {:<18} {:<12} {}",
                "route",
                truncate_text(&route.destination, 18),
                route.interface.as_deref().unwrap_or("-"),
                truncate_text(&route.raw, width.saturating_sub(47).max(12))
            )
        } else if index < socket_end {
            let socket = &snapshot.sockets[index - route_end];
            format!(
                "  {:<11} {:<18} {:<12} {}",
                "listener",
                truncate_text(&socket.local, 18),
                socket.state,
                truncate_text(
                    socket.owner.as_deref().unwrap_or("unavailable"),
                    width.saturating_sub(47).max(12)
                )
            )
        } else {
            let modem = &snapshot.cellular_modems[index - socket_end];
            format!(
                "  {:<11} {:<18} {:<12} {}",
                "cellular",
                truncate_text(modem.model.as_deref().unwrap_or(&modem.path), 18),
                modem.state,
                truncate_text(
                    &format!(
                        "{} {}{}",
                        modem.access_technologies.join(","),
                        modem.operator_name.as_deref().unwrap_or(""),
                        modem
                            .signal_quality_percent
                            .map_or_else(String::new, |signal| format!(" {signal}%"))
                    ),
                    width.saturating_sub(47).max(12)
                )
            )
        };
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(out, "{}", ink(&row, Ink::Muted, colour))?;
        }
    }
    Ok(())
}

fn render_health_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    if snapshot.findings.is_empty() {
        writeln!(out, "\n  {}", badge(" ALL CLEAR ", Ink::Success, colour))?;
        writeln!(
            out,
            "  {}",
            ink("No findings from the available checks.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    let finding = &snapshot.findings[selected.min(snapshot.findings.len() - 1)];
    if inspecting {
        writeln!(
            out,
            "\n  {}  {}",
            badge(
                &format!(" {} ", finding.severity.label().to_ascii_uppercase()),
                severity_ink(finding.severity),
                colour,
            ),
            ink(&finding.title, Ink::Bright, colour),
        )?;
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(&finding.summary, width.saturating_sub(4)),
                Ink::Muted,
                colour
            )
        )?;
        if !finding.evidence.is_empty() {
            writeln!(out, "\n  {}", ink("EVIDENCE", Ink::Label, colour))?;
            for evidence in &finding.evidence {
                let unit = evidence.unit.as_deref().unwrap_or_default();
                writeln!(
                    out,
                    "  {}  {} {}",
                    ink("•", severity_ink(finding.severity), colour),
                    evidence.label,
                    format!("{} {unit}", evidence.value).trim()
                )?;
            }
        }
        if !finding.suggested_actions.is_empty() {
            writeln!(out, "\n  {}", ink("WHAT TO CHECK", Ink::Label, colour))?;
            for action in &finding.suggested_actions {
                writeln!(
                    out,
                    "  {}  {}",
                    ink("→", Ink::Info, colour),
                    truncate_text(action, width.saturating_sub(6))
                )?;
            }
        }
        return Ok(());
    }

    writeln!(out, "  {} findings\n", snapshot.findings.len())?;
    let capacity = usize::from(rows.saturating_sub(10)).max(1);
    let start = viewport_start(selected, snapshot.findings.len(), capacity);
    for (index, finding) in snapshot
        .findings
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
    {
        let row = if width >= 70 {
            format!(
                "  {:<11} {:<30} {}",
                finding.severity.label().to_ascii_uppercase(),
                truncate_text(&finding.title, 30),
                truncate_text(&finding.summary, width.saturating_sub(47))
            )
        } else {
            format!(
                "  {:<10} {}",
                finding.severity.label().to_ascii_uppercase(),
                truncate_text(&finding.title, width.saturating_sub(14))
            )
        };
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(out, "{}", ink(&row, severity_ink(finding.severity), colour))?;
        }
    }
    Ok(())
}

fn specialist_loop(view: View, args: &ViewArgs, stdout: &mut impl Write) -> Result<()> {
    let mut active_args = args.clone();
    let mut snapshot = SystemSnapshot::empty(hostname());
    let mut receiver = spawn_specialist_collection(view, active_args.clone());
    let mut loading = true;
    let mut status = format!("Loading {} data…", view.title().to_ascii_lowercase());
    let mut selected = 0usize;
    let mut inspecting = false;
    let mut search_query: Option<String> = None;
    let mut diagnostic = DiagnosticShell::new();
    let mut next_clock = Instant::now() + Duration::from_secs(1);
    let mut redraw = true;
    loop {
        if Instant::now() >= next_clock {
            next_clock = Instant::now() + Duration::from_secs(1);
            redraw = true;
        }
        if diagnostic.poll() {
            redraw = true;
        }
        match receiver.try_recv() {
            Ok(update) => {
                selected =
                    preserve_specialist_selection(view, &snapshot, &update.snapshot, selected);
                snapshot = update.snapshot;
                loading = update.loading_more;
                status = update.status.to_owned();
                redraw = true;
            }
            Err(TryRecvError::Disconnected) if loading => {
                snapshot
                    .collection_warnings
                    .push(format!("{} collection stopped unexpectedly", view.title()));
                loading = false;
                redraw = true;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }

        if redraw {
            let (_, rows) = terminal::size().unwrap_or((100, 30));
            render_specialist(
                view, &snapshot, selected, inspecting, loading, &status, rows, stdout,
            )?;
            if let Some(query) = search_query.as_deref() {
                render_search_overlay(stdout, &format!("Search {}", view.title()), query)?;
            }
            if diagnostic.open {
                render_diagnostic_overlay(stdout, &diagnostic)?;
            }
            redraw = false;
        }

        if event::poll(Duration::from_millis(100))? {
            let event = event::read()?;
            if matches!(event, Event::Resize(_, _)) {
                redraw = true;
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if diagnostic.open {
                diagnostic.handle_key(key);
                redraw = true;
                continue;
            }
            if let Some(query) = search_query.as_mut() {
                match key.code {
                    KeyCode::Esc => search_query = None,
                    KeyCode::Enter if !query.is_empty() => {
                        let query = search_query.take().unwrap_or_default();
                        active_args.filter = Some(query.clone());
                        snapshot = SystemSnapshot::empty(hostname());
                        receiver = spawn_specialist_collection(view, active_args.clone());
                        loading = true;
                        status = format!(
                            "Searching {} for {query:?}…",
                            view.title().to_ascii_lowercase()
                        );
                        selected = 0;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(character);
                    }
                    _ => {}
                }
                redraw = true;
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc if inspecting => {
                    inspecting = false;
                    redraw = true;
                }
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Char('k') if !inspecting => {
                    selected = move_selection(selected, -1, specialist_item_count(view, &snapshot));
                    redraw = true;
                }
                KeyCode::Down | KeyCode::Char('j') if !inspecting => {
                    selected = move_selection(selected, 1, specialist_item_count(view, &snapshot));
                    redraw = true;
                }
                KeyCode::Enter if !inspecting && specialist_item_count(view, &snapshot) > 0 => {
                    inspecting = true;
                    redraw = true;
                }
                KeyCode::Char('/') if !inspecting && view != View::Processes => {
                    search_query = Some(String::new());
                    redraw = true;
                }
                KeyCode::Char('!') => {
                    diagnostic.open = true;
                    redraw = true;
                }
                KeyCode::Char('r') => {
                    snapshot = SystemSnapshot::empty(hostname());
                    receiver = spawn_specialist_collection(view, active_args.clone());
                    loading = true;
                    status = format!("Loading {} data…", view.title().to_ascii_lowercase());
                    selected = 0;
                    inspecting = false;
                    redraw = true;
                }
                _ => {}
            }
        }
    }
}

fn render_search_overlay(stdout: &mut impl Write, title: &str, query: &str) -> Result<()> {
    let (columns, rows) = terminal::size().unwrap_or((80, 24));
    let width = columns.saturating_sub(2).clamp(4, 74);
    let x = columns.saturating_sub(width) / 2;
    let y = rows.saturating_sub(5) / 2;
    let inner = usize::from(width.saturating_sub(2));
    let title = truncate_text(title, inner.saturating_sub(3));
    let input = truncate_text(&format!("> {query}"), inner);
    let help = truncate_text("Enter search · Esc cancel", inner);
    let rule = "─".repeat(inner.saturating_sub(title.chars().count() + 1));

    execute!(stdout, cursor::MoveTo(x, y))?;
    write!(stdout, "╭─{title}{rule}╮")?;
    execute!(stdout, cursor::MoveTo(x, y.saturating_add(1)))?;
    write!(stdout, "│{input:<inner$}│")?;
    execute!(stdout, cursor::MoveTo(x, y.saturating_add(2)))?;
    write!(stdout, "│{:<inner$}│", "")?;
    execute!(stdout, cursor::MoveTo(x, y.saturating_add(3)))?;
    write!(stdout, "│{help:<inner$}│")?;
    execute!(stdout, cursor::MoveTo(x, y.saturating_add(4)))?;
    write!(stdout, "╰{}╯", "─".repeat(inner))?;
    stdout.flush()?;
    Ok(())
}

struct DiagnosticShell {
    open: bool,
    input: String,
    output: Vec<String>,
    running: bool,
    receiver: Option<Receiver<String>>,
}

impl DiagnosticShell {
    fn new() -> Self {
        Self {
            open: false,
            input: String::new(),
            output: Vec::new(),
            running: false,
            receiver: None,
        }
    }

    fn poll(&mut self) -> bool {
        let Some(receiver) = self.receiver.as_ref() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(output) => {
                self.output.extend(output.lines().map(str::to_owned));
                if self.output.len() > 500 {
                    self.output.drain(..self.output.len() - 500);
                }
                self.running = false;
                self.receiver = None;
                true
            }
            Err(TryRecvError::Disconnected) => {
                self.output
                    .push("Diagnostic command stopped unexpectedly.".to_owned());
                self.running = false;
                self.receiver = None;
                true
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            KeyCode::Esc => self.open = false,
            KeyCode::Enter if !self.running && !self.input.trim().is_empty() => {
                let command = self.input.trim().to_owned();
                self.output.push(format!("$ {command}"));
                self.input.clear();
                let (sender, receiver) = mpsc::channel();
                self.receiver = Some(receiver);
                self.running = true;
                thread::spawn(move || {
                    let _ = sender.send(run_shell_command(&command));
                });
            }
            KeyCode::Backspace if !self.running => {
                self.input.pop();
            }
            KeyCode::Char(character) if !self.running => self.input.push(character),
            _ => {}
        }
    }
}

fn run_shell_command(command: &str) -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    match Command::new(shell).args(["-lc", command]).output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let text = sanitise_terminal_output(&text);
            if text.trim().is_empty() {
                format!("[exit {}]", output.status.code().unwrap_or_default())
            } else if output.status.success() {
                text
            } else {
                format!(
                    "{text}\n[exit {}]",
                    output.status.code().unwrap_or_default()
                )
            }
        }
        Err(error) => format!("Unable to start shell: {error}"),
    }
}

fn sanitise_terminal_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            matches!(character, '\n' | '\t') || (!character.is_control() && *character != '\u{1b}')
        })
        .collect()
}

fn local_clock() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| "--:--:--".to_owned())
}

fn render_diagnostic_overlay(stdout: &mut impl Write, shell: &DiagnosticShell) -> Result<()> {
    let (columns, rows) = terminal::size().unwrap_or((80, 24));
    let (x, y, width, height) = if columns >= 120 && rows >= 20 {
        let x = columns * 55 / 100;
        (x, 2, columns.saturating_sub(x + 1), rows.saturating_sub(4))
    } else {
        (1, 1, columns.saturating_sub(2), rows.saturating_sub(2))
    };
    if width < 4 || height < 6 {
        return Ok(());
    }
    let inner = usize::from(width - 2);
    let output_rows = usize::from(height.saturating_sub(5));
    let empty_output = [
        "Run a command without leaving this view.",
        "Lens keeps updating behind this panel.",
        "",
        "Try one of these:",
        "  uptime",
        "  df -h",
        "  ps aux | head",
    ];
    let visible_output: Vec<&str> = if shell.output.is_empty() {
        empty_output.to_vec()
    } else {
        let start = shell.output.len().saturating_sub(output_rows);
        shell.output[start..].iter().map(String::as_str).collect()
    };
    execute!(stdout, cursor::MoveTo(x, y))?;
    let title = "COMMAND OUTPUT";
    write!(
        stdout,
        "╭─{title}{}╮",
        "─".repeat(inner.saturating_sub(title.len() + 1))
    )?;
    for (row, line) in visible_output.iter().take(output_rows).enumerate() {
        execute!(stdout, cursor::MoveTo(x, y + 1 + row as u16))?;
        let line = truncate_text(line, inner);
        write!(stdout, "│{line:<inner$}│")?;
    }
    for row in visible_output.len().min(output_rows)..output_rows {
        execute!(stdout, cursor::MoveTo(x, y + 1 + row as u16))?;
        write!(stdout, "│{:<inner$}│", "")?;
    }
    execute!(stdout, cursor::MoveTo(x, y + height - 4))?;
    let command_title = " COMMAND ";
    write!(
        stdout,
        "├{command_title}{}┤",
        "─".repeat(inner.saturating_sub(command_title.len()))
    )?;
    execute!(stdout, cursor::MoveTo(x, y + height - 3))?;
    let prompt = if shell.running {
        "Running…".to_owned()
    } else {
        format!("$ {}", shell.input)
    };
    let prompt = truncate_text(&prompt, inner);
    write!(stdout, "│{prompt:<inner$}│")?;
    execute!(stdout, cursor::MoveTo(x, y + height - 2))?;
    let help = truncate_text("Enter to run · Esc to close · results appear above", inner);
    write!(stdout, "│{help:<inner$}│")?;
    execute!(stdout, cursor::MoveTo(x, y + height - 1))?;
    write!(stdout, "╰{}╯", "─".repeat(inner))?;
    stdout.flush()?;
    Ok(())
}

fn show_cockpit_help(stdout: &mut impl Write) -> Result<()> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    writeln!(stdout, "Lens cockpit help\n")?;
    writeln!(stdout, "↑/↓ or j/k   select a view")?;
    writeln!(stdout, "Enter         open selected view")?;
    writeln!(stdout, "/             search selected view")?;
    writeln!(stdout, "!             open diagnostic shell")?;
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
    enrich_snapshot(collect_base_snapshot(), since, log_files)
}

fn collect_base_snapshot() -> SystemSnapshot {
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
    snapshot
}

fn domain_base_snapshot(view: View) -> SystemSnapshot {
    if matches!(view, View::Processes | View::Services) {
        collect_base_snapshot()
    } else {
        let mut snapshot = SystemSnapshot::empty(hostname());
        snapshot.schema_version = SchemaVersion(SCHEMA_VERSION.to_owned());
        snapshot
    }
}

fn collect_view(
    view: View,
    since: Option<&str>,
    log_files: &[PathBuf],
    limit: usize,
) -> SystemSnapshot {
    let mut snapshot = domain_base_snapshot(view);
    match view {
        View::Processes => {}
        View::Services => {
            snapshot.services = collect_services(&mut snapshot.collection_warnings);
        }
        View::Logs => {
            snapshot.logs = collect_logs(&mut snapshot.collection_warnings, since, limit);
            let file_sources = collect_file_logs(
                log_files,
                &mut snapshot.logs,
                &mut snapshot.collection_warnings,
                limit,
            );
            snapshot.log_sources = vec![platform_log_source()];
            snapshot.log_sources.extend(file_sources);
        }
        View::Disk => {
            let mut mounts = collect_mounts(&mut snapshot.collection_warnings);
            apply_inode_usage(&mut mounts, &mut snapshot.collection_warnings);
            snapshot.filesystems = filesystems(&mounts);
            snapshot.mounts = mounts;
            snapshot.deleted_open_files =
                collect_deleted_open_files(&mut snapshot.collection_warnings);
            snapshot.block_devices = collect_block_devices(&mut snapshot.collection_warnings);
        }
        View::Net => {
            snapshot.interfaces = collect_interfaces(&mut snapshot.collection_warnings);
            snapshot.routes = collect_routes(&mut snapshot.collection_warnings);
            snapshot.sockets = collect_sockets(&mut snapshot.collection_warnings);
            snapshot.cellular_modems = collect_cellular(&mut snapshot.collection_warnings);
        }
        View::Health => return enrich_snapshot(snapshot, since, log_files),
    }
    snapshot.relationships = domain_relationships(&snapshot);
    snapshot
}

fn enrich_snapshot(
    mut snapshot: SystemSnapshot,
    since: Option<&str>,
    log_files: &[PathBuf],
) -> SystemSnapshot {
    let (service_result, log_result, disk_result, net_result) = thread::scope(|scope| {
        let services = scope.spawn(|| {
            let mut warnings = Vec::new();
            let values = collect_services(&mut warnings);
            (values, warnings)
        });
        let logs = scope.spawn(|| {
            let mut warnings = Vec::new();
            let mut values = collect_logs(&mut warnings, since, 1000);
            let sources = collect_file_logs(log_files, &mut values, &mut warnings, 1000);
            (values, sources, warnings)
        });
        let disk = scope.spawn(|| {
            let mut warnings = Vec::new();
            let mut mounts = collect_mounts(&mut warnings);
            apply_inode_usage(&mut mounts, &mut warnings);
            let deleted = collect_deleted_open_files(&mut warnings);
            let devices = collect_block_devices(&mut warnings);
            (mounts, deleted, devices, warnings)
        });
        let net = scope.spawn(|| {
            let mut warnings = Vec::new();
            let interfaces = collect_interfaces(&mut warnings);
            let routes = collect_routes(&mut warnings);
            let sockets = collect_sockets(&mut warnings);
            let cellular = collect_cellular(&mut warnings);
            (interfaces, routes, sockets, cellular, warnings)
        });
        (services.join(), logs.join(), disk.join(), net.join())
    });
    let (services, service_warnings) = service_result.unwrap_or_default();
    let (logs, file_sources, log_warnings) = log_result.unwrap_or_default();
    let (mounts, deleted_open_files, block_devices, disk_warnings) =
        disk_result.unwrap_or_default();
    let (interfaces, routes, sockets, cellular_modems, net_warnings) =
        net_result.unwrap_or_default();
    snapshot.collection_warnings.extend(service_warnings);
    snapshot.collection_warnings.extend(log_warnings);
    snapshot.collection_warnings.extend(disk_warnings);
    snapshot.collection_warnings.extend(net_warnings);
    snapshot.services = services;
    snapshot.log_sources = vec![platform_log_source()];
    snapshot.log_sources.extend(file_sources);
    snapshot.logs = logs;
    snapshot.filesystems = filesystems(&mounts);
    snapshot.deleted_open_files = deleted_open_files;
    snapshot.block_devices = block_devices;
    snapshot.mounts = mounts;
    snapshot.interfaces = interfaces;
    snapshot.routes = routes;
    snapshot.sockets = sockets;
    snapshot.cellular_modems = cellular_modems;
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
    let timeout = env::var("LENS_COLLECTOR_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::from_secs(8), Duration::from_millis);
    command_with_timeout(program, args, warnings, timeout)
}

fn command_with_timeout(
    program: &str,
    args: &[&str],
    warnings: &mut Vec<String>,
    timeout: Duration,
) -> Option<String> {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            warnings.push(format!("{program} unavailable: {error}"));
            return None;
        }
    };
    let stdout = child.stdout.take().map(|mut stream| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr = child.stderr.take().map(|mut stream| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            bytes
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                warnings.push(format!(
                    "{program} timed out after {:.1}s",
                    timeout.as_secs_f64()
                ));
                break None;
            }
            Err(error) => {
                warnings.push(format!("{program} unavailable: {error}"));
                break None;
            }
        }
    };
    let status = status?;
    let stdout = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if status.success() {
        Some(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        warnings.push(format!("{program} unavailable: {detail}"));
        None
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
fn collect_logs(warnings: &mut Vec<String>, since: Option<&str>, limit: usize) -> Vec<LogEntry> {
    let limit = limit.to_string();
    let mut args = vec!["--no-pager", "--output=short-iso"];
    if limit != "0" {
        args.extend(["-n", limit.as_str()]);
    }
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
fn collect_logs(warnings: &mut Vec<String>, since: Option<&str>, limit: usize) -> Vec<LogEntry> {
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
    let start = if limit == 0 {
        0
    } else {
        lines.len().saturating_sub(limit)
    };
    for line in &lines[start..] {
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
    let timestamp = match (fields.next(), fields.next()) {
        (Some(date), Some(time)) => format!("{date}T{time}"),
        _ => String::new(),
    };
    let _hostname = fields.next();
    let process = fields.next().unwrap_or_default();
    let unit = process
        .trim_end_matches(':')
        .split('[')
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let message = fields.collect::<Vec<_>>().join(" ");
    LogEntry {
        timestamp,
        source: "macos-unified-log".into(),
        unit,
        priority: log_priority(&message),
        message,
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
    limit: usize,
) -> Vec<LogSource> {
    let mut sources = Vec::new();
    for path in paths {
        match read_log_file(path, limit) {
            Ok(text) => {
                let id = path.display().to_string();
                sources.push(LogSource {
                    id: id.clone(),
                    kind: "file".into(),
                });
                let lines: Vec<_> = text.lines().collect();
                let start = if limit == 0 {
                    0
                } else {
                    lines.len().saturating_sub(limit)
                };
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

fn read_log_file(path: &Path, limit: usize) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    if limit == 0 {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut position = file.metadata()?.len();
    let mut chunks = Vec::new();
    let mut newlines = 0usize;
    while position > 0 && newlines <= limit {
        let chunk_size = position.min(64 * 1024) as usize;
        position -= chunk_size as u64;
        file.seek(SeekFrom::Start(position))?;
        let mut chunk = vec![0; chunk_size];
        file.read_exact(&mut chunk)?;
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunks.push(chunk);
    }
    chunks.reverse();
    let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.lines().collect();
    Ok(lines[lines.len().saturating_sub(limit)..].join("\n"))
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

#[cfg(target_os = "linux")]
fn clean_key_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "--").then(|| value.to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key).then_some(value.trim())
    })
}

#[cfg(any(target_os = "linux", test))]
fn key_values(text: &str, prefix: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .starts_with(prefix)
                .then(|| value.trim().to_owned())
                .filter(|value| !value.is_empty() && value != "--")
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_cellular(warnings: &mut Vec<String>) -> Vec<CellularModem> {
    let installed = env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|path| path.join("mmcli").is_file()));
    if !installed {
        return Vec::new();
    }
    let warning_count = warnings.len();
    let Some(list) = command("mmcli", &["-L", "-K"], warnings) else {
        if warnings[warning_count..].iter().any(|warning| {
            warning
                .to_ascii_lowercase()
                .contains("no modems were found")
        }) {
            warnings.truncate(warning_count);
        }
        return Vec::new();
    };
    key_values(&list, "modem-list.value[")
        .into_iter()
        .filter_map(|path| collect_cellular_modem(&path, warnings))
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_cellular_modem(path: &str, warnings: &mut Vec<String>) -> Option<CellularModem> {
    let text = command("mmcli", &["-m", path, "-K"], warnings)?;
    let sim_path = key_value(&text, "modem.generic.sim").and_then(clean_key_value);
    let sim = sim_path.and_then(|path| {
        command("mmcli", &["-i", &path, "-K"], warnings).map(|sim| CellularSim {
            path,
            active: true,
            iccid: key_value(&sim, "sim.properties.iccid").and_then(clean_key_value),
            operator_code: key_value(&sim, "sim.properties.operator-code")
                .and_then(clean_key_value),
            operator_name: key_value(&sim, "sim.properties.operator-name")
                .and_then(clean_key_value),
        })
    });
    Some(CellularModem {
        path: path.to_owned(),
        manufacturer: key_value(&text, "modem.generic.manufacturer").and_then(clean_key_value),
        model: key_value(&text, "modem.generic.model").and_then(clean_key_value),
        state: key_value(&text, "modem.generic.state")
            .and_then(clean_key_value)
            .unwrap_or_else(|| "unknown".into()),
        access_technologies: key_values(&text, "modem.generic.access-technologies.value["),
        signal_quality_percent: key_value(&text, "modem.generic.signal-quality.value")
            .and_then(|value| value.parse().ok()),
        operator_code: key_value(&text, "modem.3gpp.operator-code").and_then(clean_key_value),
        operator_name: key_value(&text, "modem.3gpp.operator-name").and_then(clean_key_value),
        sim,
    })
}

#[cfg(target_os = "macos")]
fn collect_cellular(_warnings: &mut Vec<String>) -> Vec<CellularModem> {
    Vec::new()
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

fn actionable_mount(mount: &Mount) -> bool {
    !matches!(
        mount.filesystem.as_str(),
        "devfs" | "devtmpfs" | "proc" | "sysfs" | "cgroup" | "cgroup2" | "debugfs"
    ) && !mount
        .target
        .starts_with("/Library/Developer/CoreSimulator/")
        && !mount.target.starts_with("/System/Volumes/")
}

fn actionable_down_interface(interface: &Interface) -> bool {
    !matches!(interface.name.as_str(), "lo" | "lo0")
        && !interface.addresses.is_empty()
        && !interface.name.starts_with("gif")
        && !interface.name.starts_with("stf")
        && !interface.name.starts_with("awdl")
        && !interface.name.starts_with("llw")
        && !interface.name.starts_with("utun")
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
        .filter(|mount| actionable_mount(mount) && mount.used_percent >= 90.0)
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
    if (!snapshot.interfaces.is_empty() || !snapshot.routes.is_empty())
        && snapshot
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
    for interface in snapshot.interfaces.iter().filter(|interface| {
        actionable_down_interface(interface) && interface.state.eq_ignore_ascii_case("down")
    }) {
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
            severity: Severity::Information,
            title: "Wildcard listeners".into(),
            summary: format!(
                "{} listeners accept connections on any local address.",
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
                "Review the listener owners in lens-net if this host should not accept inbound connections."
                    .into(),
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
    snapshot.cellular_modems.retain(|item| {
        matches(&format!(
            "{} {} {} {} {} {}",
            item.path,
            item.manufacturer.as_deref().unwrap_or(""),
            item.model.as_deref().unwrap_or(""),
            item.state,
            item.access_technologies.join(" "),
            item.operator_name.as_deref().unwrap_or("")
        ))
    });
    snapshot
        .findings
        .retain(|item| matches(&format!("{} {} {}", item.id, item.title, item.summary)));
    if limit > 0 {
        snapshot.processes.truncate(limit);
        snapshot.services.truncate(limit);
        snapshot.logs.truncate(limit);
        snapshot.mounts.truncate(limit);
        snapshot.block_devices.truncate(limit);
        snapshot.deleted_open_files.truncate(limit);
        snapshot.interfaces.truncate(limit);
        snapshot.routes.truncate(limit);
        snapshot.sockets.truncate(limit);
        snapshot.cellular_modems.truncate(limit);
        snapshot.findings.truncate(limit);
    }
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
            if !snapshot.cellular_modems.is_empty() {
                writeln!(out, "\nCELLULAR")?;
                for item in &snapshot.cellular_modems {
                    writeln!(
                        out,
                        "{:<24} {:<12} {:<10} {:>4}  {}",
                        item.model.as_deref().unwrap_or(&item.path),
                        item.state,
                        item.access_technologies.join(","),
                        item.signal_quality_percent
                            .map_or_else(|| "-".into(), |signal| format!("{signal}%")),
                        item.operator_name.as_deref().unwrap_or("")
                    )?;
                    if let Some(sim) = &item.sim {
                        writeln!(
                            out,
                            "  SIM {}{}",
                            sim.iccid.as_deref().unwrap_or(&sim.path),
                            sim.operator_name
                                .as_deref()
                                .map_or_else(String::new, |name| format!(" · {name}"))
                        )?;
                    }
                }
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
    #[cfg(target_os = "macos")]
    let platform_name = command_text("scutil", &["--get", "ComputerName"]);
    #[cfg(not(target_os = "macos"))]
    let platform_name: Option<String> = None;

    platform_name
        .or_else(|| env::var("HOSTNAME").ok())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|value| value.trim().to_owned())
        })
        .or_else(|| command_text("hostname", &[]))
        .unwrap_or_else(|| "unknown-host".into())
}

fn command_text(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
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

fn human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
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
    snapshot.cellular_modems = vec![CellularModem {
        path: "/org/freedesktop/ModemManager1/Modem/0".into(),
        manufacturer: Some("Quectel".into()),
        model: Some("EG25-G".into()),
        state: "connected".into(),
        access_technologies: vec!["lte".into()],
        signal_quality_percent: Some(78),
        operator_code: Some("50501".into()),
        operator_name: Some("Example Mobile".into()),
        sim: Some(CellularSim {
            path: "/org/freedesktop/ModemManager1/SIM/0".into(),
            active: true,
            iccid: Some("8944500000000000000".into()),
            operator_code: Some("50501".into()),
            operator_name: Some("Example Mobile".into()),
        }),
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
    fn parses_modemmanager_key_value_output() {
        let list =
            "modem-list.length : 1\nmodem-list.value[1] : /org/freedesktop/ModemManager1/Modem/0\n";
        assert_eq!(
            key_values(list, "modem-list.value["),
            ["/org/freedesktop/ModemManager1/Modem/0"]
        );
        let modem = "modem.generic.model : EG25-G\nmodem.generic.signal-quality.value : 78\nmodem.generic.access-technologies.value[1] : lte\n";
        assert_eq!(key_value(modem, "modem.generic.model"), Some("EG25-G"));
        assert_eq!(
            key_values(modem, "modem.generic.access-technologies.value["),
            ["lte"]
        );
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

        let log = parse_macos_log_entry(
            "2026-08-05 08:03:36.754714+1000 localhost worker[42]: [network] request failed",
        );
        assert_eq!(log.timestamp, "2026-08-05T08:03:36.754714+1000");
        assert_eq!(log.unit.as_deref(), Some("worker"));
        assert_eq!(log.message, "[network] request failed");
        assert_eq!(log.priority.as_deref(), Some("error"));
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
    fn cockpit_storage_selection_routes_to_lens_disk() {
        let storage = View::ALL[3];
        assert_eq!(storage, View::Disk);
        assert_eq!(storage.title(), "Storage");
        assert_eq!(storage.binary(), "lens-disk");
    }

    #[test]
    fn cockpit_leads_with_host_status_while_details_load() {
        let snapshot = demo_snapshot();
        let mut loading_output = Vec::new();
        render_cockpit(&snapshot, 0, true, 36, &mut loading_output).expect("loading cockpit");
        let loading_output = String::from_utf8(loading_output).expect("UTF-8");
        assert!(loading_output.contains("CPU"));
        assert!(loading_output.contains("Memory"));
        assert!(loading_output.contains("BUSIEST PROCESSES"));
        assert!(loading_output.contains("Checking services, logs, storage and network"));
        assert!(loading_output.contains("Processes    1 processes"));
        assert!(loading_output.contains("Services     checking"));

        let mut compact_output = Vec::new();
        render_cockpit(&snapshot, 0, true, 20, &mut compact_output).expect("compact cockpit");
        let compact_output = String::from_utf8(compact_output).expect("UTF-8");
        assert!(!compact_output.contains("BUSIEST PROCESSES"));
        assert!(compact_output.lines().count() <= 20);

        let mut complete_output = Vec::new();
        render_cockpit(&snapshot, 3, false, 30, &mut complete_output).expect("complete cockpit");
        let complete_output = String::from_utf8(complete_output).expect("UTF-8");
        assert!(complete_output.contains("critical ·"));
        assert!(complete_output.contains("▶ Storage"));
        assert!(complete_output.contains("root filesystem · 97% used"));
    }

    #[test]
    fn cockpit_uptime_is_compact() {
        assert_eq!(human_duration(90), "1m");
        assert_eq!(human_duration(7_500), "2h 5m");
        assert_eq!(human_duration(183_600), "2d 3h");
    }

    #[test]
    fn log_specialist_supports_list_and_detail_views() {
        let snapshot = demo_snapshot();
        let mut list = Vec::new();
        render_log_specialist(&snapshot, 0, false, 30, 120, &mut list).expect("log list");
        let list = String::from_utf8(list).expect("UTF-8");
        assert!(list.contains("LEVEL"));
        assert!(list.contains("mosquitto.service"));
        assert!(list.contains("No space left on device"));

        let mut detail = Vec::new();
        render_log_specialist(&snapshot, 0, true, 30, 120, &mut detail).expect("log detail");
        let detail = String::from_utf8(detail).expect("UTF-8");
        assert!(detail.contains("LOG MESSAGE"));
        assert!(detail.contains("REPEATED"));
        assert!(detail.contains("12 times"));
    }

    #[test]
    fn health_specialist_explains_selected_finding() {
        let snapshot = demo_snapshot();
        let mut list = Vec::new();
        render_health_specialist(&snapshot, 0, false, 30, 120, &mut list).expect("health list");
        let list = String::from_utf8(list).expect("UTF-8");
        assert!(list.contains("findings"));
        assert!(list.contains(&snapshot.findings[0].title));

        let mut detail = Vec::new();
        render_health_specialist(&snapshot, 0, true, 30, 120, &mut detail).expect("health detail");
        let detail = String::from_utf8(detail).expect("UTF-8");
        assert!(detail.contains("EVIDENCE"));
        assert!(detail.contains("WHAT TO CHECK"));
        assert!(detail.contains(&snapshot.findings[0].summary));
    }

    #[test]
    fn specialist_selection_and_viewports_remain_bounded() {
        assert_eq!(move_selection(0, -1, 0), 0);
        assert_eq!(move_selection(0, 1, 0), 0);
        assert_eq!(viewport_start(8, 10, 4), 6);
        assert_eq!(viewport_start(2, 10, 4), 0);
        assert_eq!(truncate_text("abcdef", 4), "abc…");
    }

    #[test]
    fn specialist_updates_preserve_the_selected_item() {
        let previous = demo_snapshot();
        let mut next = previous.clone();
        next.logs.reverse();
        next.findings.reverse();
        assert_eq!(
            &next.logs[preserve_specialist_selection(View::Logs, &previous, &next, 0)].message,
            &previous.logs[0].message
        );
        assert_eq!(
            &next.findings[preserve_specialist_selection(View::Health, &previous, &next, 0)].id,
            &previous.findings[0].id
        );
    }

    #[test]
    fn terminal_writer_restores_line_starts_in_raw_mode() {
        let mut output = Vec::new();
        {
            let mut terminal = TerminalWriter::new(&mut output);
            write!(terminal, "first\nsecond\r\nthird\n").expect("terminal output");
        }
        assert_eq!(output, b"first\r\nsecond\r\nthird\r\n");
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
    fn zero_limit_means_unbounded() {
        let mut snapshot = demo_snapshot();
        snapshot.logs.extend(snapshot.logs.clone());
        let count = snapshot.logs.len();
        let filtered = filter_snapshot(snapshot, None, None, None, None, 0);
        assert_eq!(filtered.logs.len(), count);
    }

    #[test]
    fn log_files_read_the_requested_tail_and_tolerate_non_utf8() {
        let path = env::temp_dir().join(format!("lens-log-tail-{}", std::process::id()));
        std::fs::write(&path, b"first\nsecond\nthird\xff\n").expect("write fixture");
        let tail = read_log_file(&path, 2).expect("read tail");
        assert!(!tail.contains("first"));
        assert!(tail.contains("second"));
        assert!(tail.contains("third"));
        assert!(tail.contains('\u{fffd}'));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn collector_timeout_is_reported_and_bounded() {
        let mut warnings = Vec::new();
        let started = Instant::now();
        let output = command_with_timeout(
            "sh",
            &["-c", "sleep 2 & wait"],
            &mut warnings,
            Duration::from_millis(30),
        );
        assert!(output.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(warnings.iter().any(|warning| warning.contains("timed out")));
    }

    #[test]
    fn health_ignores_pseudo_storage_and_unused_virtual_interfaces() {
        let mut snapshot = SystemSnapshot::empty("test-host");
        snapshot.mounts.push(Mount {
            source: "devfs".into(),
            target: "/dev".into(),
            filesystem: "devfs".into(),
            total_bytes: 1,
            used_bytes: 1,
            available_bytes: 0,
            used_percent: 100.0,
            inode_total: None,
            inode_used: None,
        });
        snapshot.interfaces.push(Interface {
            name: "gif0".into(),
            state: "DOWN".into(),
            addresses: Vec::new(),
        });
        let findings = diagnose(&snapshot);
        assert!(
            !findings
                .iter()
                .any(|finding| finding.id.starts_with("disk."))
        );
        assert!(
            !findings
                .iter()
                .any(|finding| finding.id.starts_with("net.interface-down"))
        );
    }

    #[test]
    fn every_specialist_domain_has_list_and_detail_rendering() {
        let snapshot = demo_snapshot();
        for renderer in [View::Services, View::Disk, View::Net] {
            let mut list = Vec::new();
            render_specialist(renderer, &snapshot, 0, false, false, "Ready", 30, &mut list)
                .expect("list");
            let mut detail = Vec::new();
            render_specialist(
                renderer,
                &snapshot,
                0,
                true,
                false,
                "Ready",
                30,
                &mut detail,
            )
            .expect("detail");
            assert!(!list.is_empty());
            assert!(!detail.is_empty());
        }
    }

    #[test]
    fn specialist_lists_reflow_at_compact_width() {
        let snapshot = demo_snapshot();
        let mut output = Vec::new();
        render_service_specialist(&snapshot, 0, false, 16, 50, &mut output)
            .expect("compact services");
        render_log_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact logs");
        render_disk_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact storage");
        render_net_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact network");
        render_health_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact health");
        assert!(!output.is_empty());
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
        assert!(!snapshot.cellular_modems.is_empty());
        assert!(!snapshot.findings.is_empty());
    }
}
