#![forbid(unsafe_code)]

mod hardware;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, VecDeque},
    env,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU8, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use anyhow::bail;
use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, ValueEnum};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::ResetColor,
    terminal::{self, ClearType},
};
use lens_core::{
    AssertionError, AssertionPolicy, FailOnSeverity, MatchMode, PrimaryDomain, UsageError,
    exit_code_from_error, parse_fields_list, project_snapshot_value,
};
use lens_model::{
    AccountInfo, CellularModem, CellularSim, CertificateInfo, Cgroup, ClockContext, DnsContext,
    GroupInfo, HardwareDevice, HardwareIdentity, IoCounters, Process, ProcessId, ProcessState,
    SchemaVersion, ServiceReference, TemperatureSensor, Timestamp, User,
};
pub use lens_model::{
    BlockDevice, DeletedOpenFile, EntityId, Filesystem, Interface, LogEntry, LogSource, Mount,
    Relationship, RelationshipKind, Route, Service, Snapshot as SystemSnapshot, Socket,
};
use lens_output::{jsonl_record_types_for_fields, write_json_lines_filtered, write_json_value};
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
    Hardware,
    System,
    Health,
}

impl View {
    pub const ALL: [Self; 8] = [
        Self::Processes,
        Self::Services,
        Self::Logs,
        Self::Disk,
        Self::Net,
        Self::Hardware,
        Self::System,
        Self::Health,
    ];

    pub const fn binary(self) -> &'static str {
        match self {
            Self::Processes => "lens-top",
            Self::Services => "lens-services",
            Self::Logs => "lens-logs",
            Self::Disk => "lens-disk",
            Self::Net => "lens-net",
            Self::Hardware => "lens-hardware",
            Self::System => "lens-system",
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
            Self::Hardware => "Hardware",
            Self::System => "System",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ThemeMode {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl ServiceAction {
    const ALL: [Self; 5] = [
        Self::Restart,
        Self::Start,
        Self::Stop,
        Self::Enable,
        Self::Disable,
    ];

    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start the service now",
            Self::Stop => "Stop the service now",
            Self::Restart => "Restart the service",
            Self::Enable => "Enable at boot",
            Self::Disable => "Disable at boot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SpecialistSort {
    Name,
    Restarts,
    UsedPercent,
    Port,
    Severity,
}

#[derive(Debug, Clone, Parser)]
#[command(version, about = "A coherent view of a Linux or macOS system")]
pub struct ViewArgs {
    /// Emit stable JSON rather than human-readable output.
    #[arg(long, conflicts_with = "jsonl")]
    pub json: bool,
    /// Emit stable JSON Lines rather than human-readable output.
    #[arg(long, conflicts_with_all = ["json", "plain"])]
    pub jsonl: bool,
    /// Emit stable plain text explicitly (the default outside an interactive terminal).
    #[arg(long, conflicts_with_all = ["json", "jsonl"])]
    pub plain: bool,
    /// Print one snapshot and exit instead of opening the interactive view.
    #[arg(long)]
    pub once: bool,
    /// Suppress stdout on success (errors still go to stderr).
    #[arg(long)]
    pub quiet: bool,
    /// Project JSON/JSONL to these top-level snapshot fields (comma-separated).
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,
    /// Choose colours for an auto-detected, dark or light terminal background.
    #[arg(long, value_enum, default_value_t = ThemeMode::Auto)]
    pub theme: ThemeMode,
    /// Use deterministic committed sample data.
    #[arg(long, hide = true)]
    pub demo: bool,
    /// Case-insensitive filter applied to rows and findings.
    #[arg(long)]
    pub filter: Option<String>,
    /// How `--filter` and name selectors bind (default: contains).
    #[arg(long, value_enum, default_value_t = MatchMode::Contains)]
    pub r#match: MatchMode,
    /// Sort primary rows when the domain supports it.
    #[arg(long, value_enum)]
    pub sort: Option<SpecialistSort>,
    /// Exact or matched service unit name (lens-services / action resolution).
    #[arg(long, value_name = "UNIT")]
    pub name: Option<String>,
    /// Restrict services to an active state (for example active, failed).
    #[arg(long, value_name = "STATE")]
    pub active: Option<String>,
    /// Restrict services whose unit load state is loaded (true) or not (false).
    #[arg(long, value_name = "BOOL")]
    pub enabled: Option<bool>,
    /// Restrict log records and services to a unit name.
    #[arg(long)]
    pub service: Option<String>,
    /// Restrict log records to a matching source, unit or message.
    #[arg(long)]
    pub process: Option<String>,
    /// Restrict log records to a journal priority label.
    #[arg(long)]
    pub severity: Option<String>,
    /// Restrict log messages containing this text.
    #[arg(long, value_name = "TEXT")]
    pub contains: Option<String>,
    /// Restrict logs to an exact unit name.
    #[arg(long, value_name = "UNIT")]
    pub unit: Option<String>,
    /// Restrict journal collection to entries newer than this journalctl time expression.
    #[arg(long)]
    pub since: Option<String>,
    /// Read an additional plain-text log file (repeatable).
    #[arg(long, value_name = "PATH")]
    pub log_file: Vec<PathBuf>,
    /// Exact mount target (lens-disk).
    #[arg(long, value_name = "PATH")]
    pub mount: Option<String>,
    /// Filesystem type filter (lens-disk).
    #[arg(long, value_name = "TYPE")]
    pub fstype: Option<String>,
    /// Minimum mount used percent (lens-disk).
    #[arg(long, value_name = "PERCENT")]
    pub min_used_percent: Option<f64>,
    /// Local TCP/UDP port (lens-net).
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Socket protocol filter: tcp or udp (lens-net).
    #[arg(long, value_name = "PROTO")]
    pub proto: Option<String>,
    /// Only listening sockets (lens-net).
    #[arg(long)]
    pub listening: bool,
    /// Network interface name (lens-net).
    #[arg(long, value_name = "IFACE")]
    pub interface: Option<String>,
    /// Hardware device class/kind (lens-hardware).
    #[arg(long, value_name = "CLASS")]
    pub class: Option<String>,
    /// Hardware serial number (lens-hardware).
    #[arg(long, value_name = "SERIAL")]
    pub serial: Option<String>,
    /// System context section: clock, dns, users, groups, certificates.
    #[arg(long, value_name = "SECTION")]
    pub section: Option<String>,
    /// Minimum finding severity for lens-health filters (information, attention, critical).
    #[arg(long, value_name = "SEVERITY")]
    pub min_severity: Option<String>,
    /// Exact finding id (lens-health).
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
    /// Exit 3 when the filtered primary row set is empty.
    #[arg(long)]
    pub fail_if_empty: bool,
    /// Exit 3 when any filtered primary rows remain.
    #[arg(long)]
    pub fail_if_any: bool,
    /// Exit 3 unless the filtered primary row count equals N.
    #[arg(long, value_name = "N")]
    pub expect_count: Option<usize>,
    /// Exit 3 unless the filtered primary row count is at least N.
    #[arg(long, value_name = "N")]
    pub expect_count_min: Option<usize>,
    /// Exit 3 unless the filtered primary row count is at most N.
    #[arg(long, value_name = "N")]
    pub expect_count_max: Option<usize>,
    /// Exit 3 when findings reach this severity (warning or critical).
    #[arg(long, value_enum)]
    pub fail_on: Option<FailOnSeverity>,
    /// Exit 3 when collection_warnings is non-empty.
    #[arg(long)]
    pub fail_on_collection_warnings: bool,
    /// Generate a manual page and exit.
    #[arg(long, value_name = "PATH", hide = true)]
    pub generate_man: Option<PathBuf>,
    /// Generate shell completion and exit.
    #[arg(long, value_enum, hide = true)]
    pub generate_completion: Option<CompletionShell>,
    /// Output path used with --generate-completion.
    #[arg(
        long,
        value_name = "PATH",
        requires = "generate_completion",
        hide = true
    )]
    pub generate_output: Option<PathBuf>,
    /// Maximum rows per result type; use 0 for every available row.
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
    /// Change a service state (lens-services on Linux only).
    #[arg(long, value_enum)]
    pub action: Option<ServiceAction>,
    /// Exact service unit targeted by --action.
    #[arg(long)]
    pub target: Option<String>,
    /// Confirm a requested state change for non-interactive use.
    #[arg(long)]
    pub yes: bool,
    /// Print the planned state change without executing it.
    #[arg(long)]
    pub dry_run: bool,
    /// After a service action, require this active state (lens-services).
    #[arg(long, value_name = "STATE")]
    pub expect_active: Option<String>,
    /// Bound wait for --expect-active (for example 2s, 500ms). Default: 2s.
    #[arg(long, value_name = "DURATION")]
    pub wait: Option<String>,
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

pub fn exit_code(error: &anyhow::Error) -> i32 {
    exit_code_from_error(error.as_ref())
}

fn usage_err(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UsageError::new(message))
}

fn assertion_err(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(AssertionError::new(message))
}

pub fn run_view(view: View) -> Result<()> {
    let args = parse_view_args(view.binary());
    validate_view_args(view, &args)?;
    set_terminal_theme(args.theme);
    if generate_assets(view.binary(), &args)? {
        return Ok(());
    }
    if args.action.is_some() {
        return run_service_action(view, &args);
    }
    let force_oneshot = args.json
        || args.jsonl
        || args.plain
        || args.once
        || args.demo
        || args.quiet
        || args.fields.is_some()
        || assertion_policy_from_args(&args).is_active()
        || !io::stdout().is_terminal();
    if !force_oneshot {
        let mut terminal = CockpitTerminal::enter()?;
        specialist_loop(view, &args, &mut terminal.stdout)?;
        return Ok(());
    }
    let snapshot = if args.demo {
        demo_snapshot()
    } else {
        collect_view(view, args.since.as_deref(), &args.log_file, args.limit)
    };
    let filtered = apply_view_filters(view, snapshot, &args)?;
    emit_snapshot_output(OutputView::Specialist(view), &args, &filtered)?;
    evaluate_assertions(view, &args, &filtered)
}

#[derive(Debug, Clone, Copy)]
enum OutputView {
    Specialist(View),
    Cockpit,
}

pub fn run_cockpit() -> Result<()> {
    let args = parse_view_args("lens");
    validate_view_args_cockpit(&args)?;
    set_terminal_theme(args.theme);
    if generate_assets("lens", &args)? {
        return Ok(());
    }
    if args.action.is_some() {
        return Err(usage_err(
            "service actions are available through lens-services",
        ));
    }
    let force_oneshot = args.json
        || args.jsonl
        || args.plain
        || args.once
        || args.demo
        || args.quiet
        || args.fields.is_some()
        || assertion_policy_from_args(&args).is_active()
        || !io::stdout().is_terminal();
    if force_oneshot {
        let snapshot = if args.demo {
            demo_snapshot()
        } else {
            collect_with_options(args.since.as_deref(), &args.log_file)
        };
        let snapshot = apply_view_filters(View::Processes, snapshot, &args)?;
        emit_snapshot_output(OutputView::Cockpit, &args, &snapshot)?;
        return evaluate_assertions(View::Processes, &args, &snapshot);
    }

    let mut terminal = CockpitTerminal::enter()?;
    cockpit_loop(
        &mut terminal.stdout,
        args.since.clone(),
        args.log_file.clone(),
    )
}

fn assertion_policy_from_args(args: &ViewArgs) -> AssertionPolicy {
    AssertionPolicy {
        fail_if_empty: args.fail_if_empty,
        fail_if_any: args.fail_if_any,
        expect_count: args.expect_count,
        expect_count_min: args.expect_count_min,
        expect_count_max: args.expect_count_max,
        fail_on: args.fail_on,
        fail_on_collection_warnings: args.fail_on_collection_warnings,
    }
}

fn primary_domain_for(view: View) -> PrimaryDomain {
    match view {
        View::Processes => PrimaryDomain::Processes,
        View::Services => PrimaryDomain::Services,
        View::Logs => PrimaryDomain::Logs,
        View::Disk => PrimaryDomain::Mounts,
        View::Net => PrimaryDomain::Sockets,
        View::Hardware => PrimaryDomain::HardwareDevices,
        View::System => PrimaryDomain::SystemRows,
        View::Health => PrimaryDomain::Findings,
    }
}

fn evaluate_assertions(view: View, args: &ViewArgs, snapshot: &SystemSnapshot) -> Result<()> {
    let policy = assertion_policy_from_args(args);
    policy
        .validate()
        .map_err(|error| usage_err(error.message))?;
    match policy.evaluate(snapshot, primary_domain_for(view)) {
        Ok(()) => Ok(()),
        Err(error) => Err(assertion_err(error.message)),
    }
}

fn emit_snapshot_output(
    view: OutputView,
    args: &ViewArgs,
    snapshot: &SystemSnapshot,
) -> Result<()> {
    if args.quiet {
        return Ok(());
    }
    let fields = match &args.fields {
        Some(raw) => Some(parse_fields_list(raw).map_err(|error| usage_err(error.message))?),
        None => None,
    };
    if args.fields.is_some() && !args.json && !args.jsonl {
        return Err(usage_err("--fields requires --json or --jsonl"));
    }
    let write_result: Result<()> = if args.json {
        if let Some(fields) = fields {
            let value = project_snapshot_value(snapshot, &fields).context("project JSON fields")?;
            write_json_value(&mut io::stdout().lock(), &value).context("write JSON")
        } else {
            serde_json::to_writer_pretty(io::stdout().lock(), snapshot).context("write JSON")?;
            writeln!(io::stdout()).context("write JSON")
        }
    } else if args.jsonl {
        let record_types = if let Some(fields) = fields.as_ref() {
            jsonl_record_types_for_fields(fields)
        } else {
            match view {
                OutputView::Specialist(specialist) => jsonl_defaults_for_view(specialist),
                OutputView::Cockpit => vec![
                    "host",
                    "process",
                    "service",
                    "log",
                    "mount",
                    "socket",
                    "finding",
                    "collection_warning",
                ],
            }
        };
        write_json_lines_filtered(&mut io::stdout().lock(), snapshot, &record_types)
            .context("write JSON Lines")
    } else {
        match view {
            OutputView::Cockpit => render_overview(snapshot, &mut io::stdout().lock()),
            OutputView::Specialist(specialist) => {
                render_plain(specialist, snapshot, &mut io::stdout().lock())
            }
        }
    };
    match write_result {
        Ok(()) => Ok(()),
        Err(error) if is_broken_pipe(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn jsonl_defaults_for_view(view: View) -> Vec<&'static str> {
    match view {
        View::Processes => vec!["host", "process", "finding"],
        View::Services => vec!["host", "service", "finding"],
        View::Logs => vec!["host", "log", "finding"],
        View::Disk => vec![
            "host",
            "mount",
            "block_device",
            "deleted_open_file",
            "finding",
        ],
        View::Net => vec!["host", "interface", "route", "socket", "finding"],
        View::Hardware => vec!["host", "hardware_device", "finding"],
        View::System => vec!["host", "finding"],
        View::Health => vec!["host", "finding", "collection_warning"],
    }
}

fn parse_view_args(name: &'static str) -> ViewArgs {
    let matches = view_command(name).get_matches();
    ViewArgs::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn view_command(name: &'static str) -> clap::Command {
    let mut command = ViewArgs::command().name(name);
    let supports_service = matches!(name, "lens" | "lens-services" | "lens-logs");
    let supports_log_filters = matches!(name, "lens" | "lens-logs");
    let supports_log_source = matches!(name, "lens" | "lens-logs" | "lens-health");
    let supports_actions = name == "lens-services";
    let supports_name = matches!(name, "lens-services");
    let supports_service_state = name == "lens-services";
    let supports_disk = name == "lens-disk";
    let supports_net = name == "lens-net";
    let supports_hardware = name == "lens-hardware";
    let supports_system = name == "lens-system";
    let supports_health = matches!(name, "lens-health" | "lens");
    for (argument, visible) in [
        ("service", supports_service),
        ("process", supports_log_filters),
        ("severity", supports_log_filters),
        ("contains", supports_log_filters),
        ("unit", supports_log_filters),
        ("since", supports_log_source),
        ("log_file", supports_log_source),
        ("name", supports_name),
        ("active", supports_service_state),
        ("enabled", supports_service_state),
        ("mount", supports_disk),
        ("fstype", supports_disk),
        ("min_used_percent", supports_disk),
        ("port", supports_net),
        ("proto", supports_net),
        ("listening", supports_net),
        ("interface", supports_net),
        ("class", supports_hardware),
        ("serial", supports_hardware),
        ("section", supports_system),
        ("min_severity", supports_health),
        ("id", supports_health),
        ("fail_on", supports_health),
        ("action", supports_actions),
        ("target", supports_actions),
        ("yes", supports_actions),
        ("dry_run", supports_actions),
        ("expect_active", supports_actions),
        ("wait", supports_actions),
    ] {
        if !visible {
            command = command.mut_arg(argument, |arg| arg.hide(true));
        }
    }
    command
}

fn validate_view_args_cockpit(args: &ViewArgs) -> Result<()> {
    assertion_policy_from_args(args)
        .validate()
        .map_err(|error| usage_err(error.message))?;
    if args.fields.is_some() && !args.json && !args.jsonl {
        return Err(usage_err("--fields requires --json or --jsonl"));
    }
    Ok(())
}

fn validate_view_args(view: View, args: &ViewArgs) -> Result<()> {
    assertion_policy_from_args(args)
        .validate()
        .map_err(|error| usage_err(error.message))?;
    if args.fields.is_some() && !args.json && !args.jsonl {
        return Err(usage_err("--fields requires --json or --jsonl"));
    }
    if args.service.is_some() && !matches!(view, View::Services | View::Logs) {
        return Err(usage_err(
            "--service is only available in lens-services and lens-logs",
        ));
    }
    if args.name.is_some() && view != View::Services {
        return Err(usage_err("--name is only available in lens-services"));
    }
    if (args.active.is_some() || args.enabled.is_some()) && view != View::Services {
        return Err(usage_err(
            "--active and --enabled are only available in lens-services",
        ));
    }
    if args.process.is_some() && view != View::Logs {
        return Err(usage_err("--process is only available in lens-logs"));
    }
    if args.severity.is_some() && view != View::Logs {
        return Err(usage_err("--severity is only available in lens-logs"));
    }
    if (args.contains.is_some() || args.unit.is_some()) && view != View::Logs {
        return Err(usage_err(
            "--contains and --unit are only available in lens-logs",
        ));
    }
    if (args.since.is_some() || !args.log_file.is_empty())
        && !matches!(view, View::Logs | View::Health)
    {
        return Err(usage_err(
            "--since and --log-file are only available in lens-logs and lens-health",
        ));
    }
    if (args.mount.is_some() || args.fstype.is_some() || args.min_used_percent.is_some())
        && view != View::Disk
    {
        return Err(usage_err(
            "--mount, --fstype and --min-used-percent are only available in lens-disk",
        ));
    }
    if (args.port.is_some() || args.proto.is_some() || args.listening || args.interface.is_some())
        && view != View::Net
    {
        return Err(usage_err(
            "--port, --proto, --listening and --interface are only available in lens-net",
        ));
    }
    if (args.class.is_some() || args.serial.is_some()) && view != View::Hardware {
        return Err(usage_err(
            "--class and --serial are only available in lens-hardware",
        ));
    }
    if args.section.is_some() && view != View::System {
        return Err(usage_err("--section is only available in lens-system"));
    }
    if (args.min_severity.is_some() || args.id.is_some()) && view != View::Health {
        return Err(usage_err(
            "--min-severity and --id are only available in lens-health",
        ));
    }
    if args.fail_on.is_some() && !matches!(view, View::Health) {
        // Allow on health only for specialists; cockpit validates separately.
        return Err(usage_err(
            "--fail-on is only available in lens-health and lens",
        ));
    }
    if let Some(percent) = args.min_used_percent
        && !(0.0..=100.0).contains(&percent)
    {
        return Err(usage_err("--min-used-percent must be between 0 and 100"));
    }
    if let Some(proto) = args.proto.as_deref() {
        let proto = proto.to_ascii_lowercase();
        if proto != "tcp" && proto != "udp" {
            return Err(usage_err("--proto must be tcp or udp"));
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ActionOutcome {
    action: ServiceAction,
    target: String,
    status: &'static str,
    verified_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expect_active: Option<String>,
}

fn run_service_action(view: View, args: &ViewArgs) -> Result<()> {
    if view != View::Services {
        return Err(usage_err("--action is only supported by lens-services"));
    }
    let action = args.action.ok_or_else(|| usage_err("missing --action"))?;
    let target = resolve_service_action_target(args)?;
    if !args.dry_run && !args.yes {
        return Err(usage_err(
            "state changes require --yes; use --dry-run to inspect the plan safely",
        ));
    }
    if args.dry_run {
        return write_action_outcome(
            args,
            &ActionOutcome {
                action,
                target,
                status: "planned",
                verified_state: None,
                dry_run: Some(true),
                expect_active: args.expect_active.clone(),
            },
        );
    }
    #[cfg(target_os = "macos")]
    {
        let _ = action;
        Err(usage_err(format!(
            "service actions are not yet supported safely for launchd; no change was made to {target}"
        )))
    }
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
            &[verb, "--", target.as_str()],
            &mut warnings,
            Duration::from_secs(15),
        )
        .is_none()
        {
            bail!("{}", warnings.join("; "));
        }
        let wait = parse_wait_duration(args.wait.as_deref()).map_err(usage_err)?;
        let deadline = Instant::now() + wait;
        let verified_state = loop {
            let mut verify_warnings = Vec::new();
            let service = collect_services(&mut verify_warnings)
                .into_iter()
                .find(|service| service.name == target);
            let state = service
                .as_ref()
                .map(|service| format!("{} / {}", service.active, service.sub));
            if let Some(expected) = args.expect_active.as_deref() {
                if service
                    .as_ref()
                    .is_some_and(|item| item.active.eq_ignore_ascii_case(expected))
                {
                    break state;
                }
                if Instant::now() >= deadline {
                    return Err(assertion_err(format!(
                        "service {target} did not reach active state '{expected}' within {}",
                        format_wait(wait)
                    )));
                }
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            break state;
        };
        write_action_outcome(
            args,
            &ActionOutcome {
                action,
                target,
                status: "completed",
                verified_state,
                dry_run: Some(false),
                expect_active: args.expect_active.clone(),
            },
        )
    }
}

fn resolve_service_action_target(args: &ViewArgs) -> Result<String> {
    if let Some(target) = args.target.as_deref() {
        if target.trim().is_empty()
            || target.starts_with('-')
            || target.chars().any(char::is_whitespace)
        {
            return Err(usage_err("--target must be one exact service unit name"));
        }
        return Ok(target.to_owned());
    }
    if args.name.is_none()
        && args.service.is_none()
        && args.filter.is_none()
        && args.active.is_none()
    {
        return Err(usage_err(
            "--action requires --target or a selector that resolves to exactly one service (--name/--service/--filter/--active)",
        ));
    }
    let snapshot = if args.demo {
        demo_snapshot()
    } else {
        let mut warnings = Vec::new();
        let mut snapshot = SystemSnapshot::empty(hostname());
        snapshot.services = collect_services(&mut warnings);
        snapshot.collection_warnings = warnings;
        snapshot
    };
    let filtered = apply_view_filters(View::Services, snapshot, args)?;
    match filtered.services.as_slice() {
        [service] => Ok(service.name.clone()),
        [] => Err(usage_err(
            "service action selector matched no units; refuse to act",
        )),
        _ => Err(usage_err(format!(
            "service action selector matched {} units; refuse to act without a unique target",
            filtered.services.len()
        ))),
    }
}

#[cfg(target_os = "linux")]
fn parse_wait_duration(raw: Option<&str>) -> Result<Duration, String> {
    let Some(raw) = raw else {
        return Ok(Duration::from_secs(2));
    };
    let raw = raw.trim();
    if let Some(ms) = raw.strip_suffix("ms") {
        let value: u64 = ms
            .parse()
            .map_err(|_| format!("invalid --wait duration '{raw}'"))?;
        return Ok(Duration::from_millis(value.max(1)));
    }
    if let Some(secs) = raw.strip_suffix('s') {
        let value: u64 = secs
            .parse()
            .map_err(|_| format!("invalid --wait duration '{raw}'"))?;
        return Ok(Duration::from_secs(value.max(1)));
    }
    if let Ok(secs) = raw.parse::<u64>() {
        return Ok(Duration::from_secs(secs.max(1)));
    }
    Err(format!("invalid --wait duration '{raw}' (use 2s or 500ms)"))
}

#[cfg(target_os = "linux")]
fn format_wait(duration: Duration) -> String {
    if duration.as_millis() % 1000 == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn write_action_outcome(args: &ViewArgs, outcome: &ActionOutcome) -> Result<()> {
    if args.quiet {
        return Ok(());
    }
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), outcome)?;
        println!();
    } else {
        println!(
            "{} {}: {}",
            outcome.action.cli_name(),
            outcome.target,
            outcome.status
        );
        if let Some(state) = &outcome.verified_state {
            println!("Verified state: {state}");
        }
    }
    Ok(())
}

fn generate_assets(name: &'static str, args: &ViewArgs) -> Result<bool> {
    if let Some(path) = &args.generate_man {
        let command = view_command(name);
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
        let mut command = view_command(name);
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
        let _ = execute!(
            self.stdout,
            ResetColor,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0),
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
        let _ = self.stdout.flush();
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
    frame: Vec<u8>,
}

impl<W> TerminalWriter<W> {
    const fn new(inner: W) -> Self {
        Self {
            inner,
            trailing_carriage_return: false,
            frame: Vec::new(),
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
        self.frame.extend_from_slice(&converted);
        self.trailing_carriage_return = previous_was_carriage_return;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Buffer a complete frame and publish it as one synchronized terminal update. Remote
        // terminals otherwise expose the clear between frames as a full-screen flash on slow links.
        self.inner.write_all(b"\x1b[?2026h")?;
        self.inner.write_all(&self.frame)?;
        self.inner.write_all(b"\x1b[?2026l")?;
        self.frame.clear();
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
    let mut cpu_activity = CpuActivity::default();
    let mut network_activity = NetworkActivity::default();
    let (live_request, live_receiver) = spawn_cockpit_live_sampler();
    let mut live_sample_in_flight = live_request.send(()).is_ok();
    let mut next_live_sample = Instant::now() + Duration::from_secs(1);
    let mut latest_live_snapshot: Option<SystemSnapshot> = None;
    let mut live_error_reported = false;
    let mut search_query: Option<String> = None;
    let mut diagnostic = DiagnosticShell::new();
    let mut next_clock = Instant::now() + Duration::from_secs(60);
    let mut redraw = true;
    loop {
        if Instant::now() >= next_clock {
            next_clock = Instant::now() + Duration::from_secs(60);
            redraw = true;
        }
        if diagnostic.poll() {
            redraw = true;
        }
        if live_sample_in_flight {
            match live_receiver.try_recv() {
                Ok(Ok(mut sample)) => {
                    cpu_activity.observe(&mut sample.snapshot);
                    let counters: BTreeMap<_, _> = sample
                        .interface_counters
                        .into_iter()
                        .map(|(name, rx, tx)| (name, (rx, tx)))
                        .collect();
                    for interface in &mut snapshot.interfaces {
                        if let Some((rx, tx)) = counters.get(&interface.name) {
                            interface.rx_bytes = Some(*rx);
                            interface.tx_bytes = Some(*tx);
                        }
                    }
                    network_activity.observe(&snapshot.interfaces, Instant::now());
                    snapshot.host = sample.snapshot.host.clone();
                    snapshot.processes.clone_from(&sample.snapshot.processes);
                    latest_live_snapshot = Some(sample.snapshot);
                    live_error_reported = false;
                    live_sample_in_flight = false;
                    next_live_sample = Instant::now() + Duration::from_secs(1);
                    redraw = true;
                }
                Ok(Err(error)) => {
                    if !live_error_reported {
                        snapshot
                            .collection_warnings
                            .push(format!("live activity unavailable: {error}"));
                        live_error_reported = true;
                    }
                    live_sample_in_flight = false;
                    next_live_sample = Instant::now() + Duration::from_secs(1);
                    redraw = true;
                }
                Err(TryRecvError::Disconnected) => {
                    live_sample_in_flight = false;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if !live_sample_in_flight && Instant::now() >= next_live_sample {
            live_sample_in_flight = live_request.send(()).is_ok();
        }
        match receiver.try_recv() {
            Ok(update) => {
                snapshot = update.snapshot;
                if let Some(live) = &latest_live_snapshot {
                    snapshot.host = live.host.clone();
                    snapshot.processes.clone_from(&live.processes);
                }
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
            render_cockpit(
                &snapshot,
                &cpu_activity,
                &network_activity,
                selected,
                loading,
                rows,
                stdout,
            )?;
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
        let snapshot = enrich_snapshot(base, cockpit_log_since(since.as_deref()), &log_files);
        let _ = sender.send(CockpitUpdate {
            snapshot,
            loading: false,
        });
    });
    receiver
}

struct CockpitLiveSample {
    snapshot: SystemSnapshot,
    interface_counters: Vec<(String, u64, u64)>,
}

fn spawn_cockpit_live_sampler() -> (
    mpsc::Sender<()>,
    Receiver<std::result::Result<CockpitLiveSample, String>>,
) {
    let (request_sender, request_receiver) = mpsc::channel();
    let (sample_sender, sample_receiver) = mpsc::channel();
    thread::spawn(move || {
        #[cfg(target_os = "linux")]
        let mut collector = LinuxCollector::default();
        #[cfg(target_os = "macos")]
        let mut collector = MacOsCollector::default();
        collector.set_refresh_interval(Duration::from_secs(1));

        while request_receiver.recv().is_ok() {
            let sample = collector
                .collect()
                .map(|snapshot| {
                    let mut warnings = Vec::new();
                    let interface_counters = collect_interface_counters(&mut warnings);
                    CockpitLiveSample {
                        snapshot,
                        interface_counters,
                    }
                })
                .map_err(|error| error.to_string());
            if sample_sender.send(sample).is_err() {
                break;
            }
        }
    });
    (request_sender, sample_receiver)
}

#[derive(Debug, Default)]
struct CpuActivity {
    previous_total_ticks: Option<u64>,
    previous_idle_ticks: u64,
    previous_process_ticks: BTreeMap<(u32, u64), u64>,
    history: VecDeque<u64>,
}

impl CpuActivity {
    fn observe(&mut self, snapshot: &mut SystemSnapshot) {
        let total_ticks = snapshot.host.total_cpu_ticks;
        let idle_ticks = snapshot.host.idle_cpu_ticks;
        if let Some(previous_total) = self.previous_total_ticks {
            let total_delta = total_ticks.saturating_sub(previous_total);
            let idle_delta = idle_ticks.saturating_sub(self.previous_idle_ticks);
            snapshot.host.cpu_percent = if total_delta == 0 {
                0.0
            } else {
                (total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64) * 100.0
            };
            push_bounded(
                &mut self.history,
                snapshot.host.cpu_percent.clamp(0.0, 100.0).round() as u64,
                60,
            );

            #[cfg(target_os = "linux")]
            {
                for process in &mut snapshot.processes {
                    let key = (process.pid.0, process.start_time_ticks);
                    if let Some(previous_ticks) = self.previous_process_ticks.get(&key) {
                        let process_delta = process.cpu_time_ticks.saturating_sub(*previous_ticks);
                        process.cpu_percent = if total_delta == 0 {
                            0.0
                        } else {
                            (process_delta as f64 / total_delta as f64)
                                * snapshot.host.cpu_count.max(1) as f64
                                * 100.0
                        };
                    }
                }
            }
        }
        self.previous_total_ticks = Some(total_ticks);
        self.previous_idle_ticks = idle_ticks;
        self.previous_process_ticks = snapshot
            .processes
            .iter()
            .map(|process| {
                (
                    (process.pid.0, process.start_time_ticks),
                    process.cpu_time_ticks,
                )
            })
            .collect();
    }
}

struct SpecialistUpdate {
    snapshot: SystemSnapshot,
    loading_more: bool,
    status: &'static str,
}

#[derive(Debug, Default)]
struct NetworkActivity {
    previous: BTreeMap<String, (u64, u64)>,
    rates: BTreeMap<String, (u64, u64)>,
    rx_history: VecDeque<u64>,
    tx_history: VecDeque<u64>,
    last_sample: Option<Instant>,
}

impl NetworkActivity {
    fn observe(&mut self, interfaces: &[Interface], now: Instant) {
        let current: BTreeMap<_, _> = interfaces
            .iter()
            .filter_map(|interface| {
                Some((
                    interface.name.clone(),
                    (interface.rx_bytes?, interface.tx_bytes?),
                ))
            })
            .collect();
        let Some(previous_at) = self.last_sample else {
            self.previous = current;
            self.last_sample = Some(now);
            return;
        };
        let elapsed = now.saturating_duration_since(previous_at).as_secs_f64();
        if elapsed < 0.25 {
            return;
        }
        self.rates.clear();
        let mut total_rx = 0u64;
        let mut total_tx = 0u64;
        for (name, (rx, tx)) in &current {
            let Some((previous_rx, previous_tx)) = self.previous.get(name) else {
                continue;
            };
            let rx_rate = (rx.saturating_sub(*previous_rx) as f64 / elapsed) as u64;
            let tx_rate = (tx.saturating_sub(*previous_tx) as f64 / elapsed) as u64;
            self.rates.insert(name.clone(), (rx_rate, tx_rate));
            if name != "lo" && !name.starts_with("lo0") {
                total_rx = total_rx.saturating_add(rx_rate);
                total_tx = total_tx.saturating_add(tx_rate);
            }
        }
        push_bounded(&mut self.rx_history, total_rx, 60);
        push_bounded(&mut self.tx_history, total_tx, 60);
        self.previous = current;
        self.last_sample = Some(now);
    }

    fn interface_rate(&self, name: &str) -> Option<(u64, u64)> {
        self.rates.get(name).copied()
    }

    fn current(&self) -> Option<(u64, u64)> {
        Some((
            *self.rx_history.back()?,
            *self.tx_history.back().unwrap_or(&0),
        ))
    }
}

fn push_bounded(values: &mut VecDeque<u64>, value: u64, capacity: usize) {
    if values.len() == capacity {
        values.pop_front();
    }
    values.push_back(value);
}

fn activity_sparkline(values: &VecDeque<u64>, width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let start = values.len().saturating_sub(width);
    let visible = values.iter().skip(start);
    let maximum = visible.clone().copied().max().unwrap_or(0);
    visible
        .map(|value| {
            if maximum == 0 {
                BARS[0]
            } else {
                let index = ((*value as u128 * 7) / maximum as u128) as usize;
                BARS[index]
            }
        })
        .collect()
}

fn percentage_sparkline(values: &VecDeque<u64>, width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let start = values.len().saturating_sub(width);
    values
        .iter()
        .skip(start)
        .map(|value| {
            let index = ((*value).min(100) as usize * 7) / 100;
            BARS[index]
        })
        .collect()
}

fn spawn_interface_counter_collection() -> Receiver<Vec<(String, u64, u64)>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut warnings = Vec::new();
        let _ = sender.send(collect_interface_counters(&mut warnings));
    });
    receiver
}

fn send_specialist_update(
    sender: &mpsc::Sender<SpecialistUpdate>,
    view: View,
    snapshot: SystemSnapshot,
    args: &ViewArgs,
    loading_more: bool,
    status: &'static str,
) -> bool {
    let snapshot = apply_view_filters(view, snapshot, args)
        .unwrap_or_else(|_| SystemSnapshot::empty("localhost"));
    sender
        .send(SpecialistUpdate {
            snapshot,
            loading_more,
            status,
        })
        .is_ok()
}

fn filter_snapshot(
    snapshot: SystemSnapshot,
    filter: Option<&str>,
    service: Option<&str>,
    process: Option<&str>,
    severity: Option<&str>,
    limit: usize,
) -> SystemSnapshot {
    let mut args = ViewArgs::parse_from(["lens-services"]);
    args.filter = filter.map(str::to_owned);
    args.service = service.map(str::to_owned);
    args.process = process.map(str::to_owned);
    args.severity = severity.map(str::to_owned);
    args.limit = limit;
    apply_view_filters(View::Logs, snapshot, &args)
        .unwrap_or_else(|_| SystemSnapshot::empty("localhost"))
}

fn spawn_specialist_collection(view: View, args: ViewArgs) -> Receiver<SpecialistUpdate> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut snapshot = domain_base_snapshot(view);
        match view {
            View::Processes => {
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
            }
            View::Services => {
                snapshot.services = collect_services(&mut snapshot.collection_warnings);
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
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
                        view,
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
                    let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
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
                    let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
                }
            }
            View::Disk => {
                let mut mounts = collect_mounts(&mut snapshot.collection_warnings);
                apply_inode_usage(&mut mounts, &mut snapshot.collection_warnings);
                snapshot.filesystems = filesystems(&mounts);
                snapshot.mounts = mounts;
                if !send_specialist_update(
                    &sender,
                    view,
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
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
            }
            View::Net => {
                snapshot.interfaces = collect_interfaces(&mut snapshot.collection_warnings);
                snapshot.routes = collect_routes(&mut snapshot.collection_warnings);
                if !send_specialist_update(
                    &sender,
                    view,
                    snapshot.clone(),
                    &args,
                    true,
                    "Interfaces and routes ready; checking listeners…",
                ) {
                    return;
                }
                snapshot.sockets = collect_sockets(&mut snapshot.collection_warnings);
                snapshot.cellular_modems = collect_cellular(&mut snapshot.collection_warnings);
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
            }
            View::Hardware => {
                collect_hardware_context(&mut snapshot);
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
            }
            View::System => {
                collect_system_context(&mut snapshot);
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
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
                    view,
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
                collect_hardware_context(&mut snapshot);
                snapshot.findings = diagnose(&snapshot);
                snapshot
                    .relationships
                    .extend(domain_relationships(&snapshot));
                let _ = send_specialist_update(&sender, view, snapshot, &args, false, "Ready");
            }
        }
    });
    receiver
}

fn render_cockpit(
    snapshot: &SystemSnapshot,
    cpu_activity: &CpuActivity,
    network_activity: &NetworkActivity,
    selected: usize,
    loading: bool,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    let mut frame = Vec::new();
    render_cockpit_content(
        snapshot,
        cpu_activity,
        network_activity,
        selected,
        loading,
        rows,
        &mut frame,
    )?;
    present_frame(stdout, &frame)
}

fn render_cockpit_content(
    snapshot: &SystemSnapshot,
    cpu_activity: &CpuActivity,
    network_activity: &NetworkActivity,
    selected: usize,
    loading: bool,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    let host = &snapshot.host;
    let columns = terminal::size().map_or(88, |(width, _)| width);
    if columns < 36 || rows < 10 {
        writeln!(stdout, "LENS")?;
        writeln!(stdout, "Terminal too small ({columns}x{rows}).")?;
        writeln!(stdout, "Resize to at least 36x10 or press q to quit.")?;
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

    if rows >= 24 {
        render_cockpit_activity(
            snapshot,
            cpu_activity,
            network_activity,
            width,
            rows,
            stdout,
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
    if width >= 110 && rows >= 26 {
        let gap = 2usize;
        let available = width.saturating_sub(4 + gap);
        let left_width = available / 2;
        let right_width = available.saturating_sub(left_width);
        for row_index in 0..4 {
            let left_index = row_index;
            let right_index = row_index + 4;
            let left = cockpit_explore_cell(
                View::ALL[left_index],
                left_index == selected,
                snapshot,
                loading,
                left_width,
            );
            let right = cockpit_explore_cell(
                View::ALL[right_index],
                right_index == selected,
                snapshot,
                loading,
                right_width,
            );
            let left = if left_index == selected {
                selected_row(&left, colour)
            } else {
                ink(&left, Ink::Muted, colour)
            };
            let right = if right_index == selected {
                selected_row(&right, colour)
            } else {
                ink(&right, Ink::Muted, colour)
            };
            writeln!(stdout, "  {left}  {right}")?;
        }
    } else {
        for (index, view) in View::ALL.iter().enumerate() {
            let row = cockpit_explore_cell(*view, index == selected, snapshot, loading, width)
                .trim_end()
                .to_owned();
            if index == selected {
                writeln!(stdout, "{}", selected_row(&row, colour))?;
            } else {
                writeln!(stdout, "{}", ink(&row, Ink::Muted, colour))?;
            }
        }
    }
    if rows >= 30 {
        clear_gap_and_anchor_footer(stdout, rows)?;
    } else {
        writeln!(stdout)?;
    }
    writeln!(stdout, "{}", ink(&format!("├{rule}┤"), Ink::Border, colour))?;
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
        write!(stdout, "{}", ink(&format!("╰{rule}╯"), Ink::Border, colour))?;
    }
    Ok(())
}

fn cockpit_explore_cell(
    view: View,
    selected: bool,
    snapshot: &SystemSnapshot,
    loading: bool,
    width: usize,
) -> String {
    let marker = if selected { "▶" } else { " " };
    let summary = cockpit_view_summary(view, snapshot, loading);
    let row = if width >= 48 {
        format!(
            "{marker} {:<12} {}",
            view.title(),
            truncate_text(&summary, width.saturating_sub(18))
        )
    } else {
        format!("{marker} {}", view.title())
    };
    let row = truncate_text(&row, width);
    format!(
        "{row}{}",
        " ".repeat(width.saturating_sub(row.chars().count()))
    )
}

fn render_cockpit_activity(
    snapshot: &SystemSnapshot,
    cpu_activity: &CpuActivity,
    network_activity: &NetworkActivity,
    width: usize,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    if width >= 140 && rows >= 28 {
        return render_wide_cockpit_activity(
            snapshot,
            cpu_activity,
            network_activity,
            width,
            stdout,
        );
    }
    let colour = terminal_colour_enabled();
    let chart_width = width.saturating_sub(44).div_ceil(2).clamp(8, 32);
    let cpu_chart = percentage_sparkline(&cpu_activity.history, chart_width);
    let cpu_chart = if cpu_chart.is_empty() {
        "waiting".to_owned()
    } else {
        cpu_chart
    };
    let cpu_chart = fixed_chart_cell(&cpu_chart, chart_width);
    writeln!(
        stdout,
        "\n  {}",
        ink("LIVE ACTIVITY · 60s", Ink::Label, colour)
    )?;
    if snapshot.host.cpu_count == 0 {
        writeln!(
            stdout,
            "  {} collecting host counters…",
            ink("CPU", Ink::Label, colour)
        )?;
    } else if width >= 64 {
        writeln!(
            stdout,
            "  {} {:>5.1}%  {}  {} logical CPU{}",
            ink("CPU", Ink::Label, colour),
            snapshot.host.cpu_percent,
            ink(&cpu_chart, Ink::Info, colour),
            snapshot.host.cpu_count,
            if snapshot.host.cpu_count == 1 {
                ""
            } else {
                "s"
            }
        )?;
    } else {
        writeln!(
            stdout,
            "  {} {:>5.1}%  {}  {} CPU{}",
            ink("CPU", Ink::Label, colour),
            snapshot.host.cpu_percent,
            ink(&cpu_chart, Ink::Info, colour),
            snapshot.host.cpu_count,
            if snapshot.host.cpu_count == 1 {
                ""
            } else {
                "s"
            }
        )?;
    }
    if let Some((rx, tx)) = network_activity.current() {
        let rx_chart = fixed_chart_cell(
            &activity_sparkline(&network_activity.rx_history, chart_width),
            chart_width,
        );
        let tx_chart = fixed_chart_cell(
            &activity_sparkline(&network_activity.tx_history, chart_width),
            chart_width,
        );
        if width >= 64 {
            writeln!(
                stdout,
                "  {} ↓ {:>8}/s {}  ↑ {:>8}/s {}",
                ink("NET", Ink::Label, colour),
                human_bytes(rx),
                ink(&rx_chart, Ink::Info, colour),
                human_bytes(tx),
                ink(&tx_chart, Ink::Attention, colour),
            )?;
        } else {
            writeln!(
                stdout,
                "  {} ↓ {}/s  ↑ {}/s",
                ink("NET", Ink::Label, colour),
                human_bytes(rx),
                human_bytes(tx),
            )?;
        }
    } else {
        writeln!(
            stdout,
            "  {} collecting interface counters…",
            ink("NET", Ink::Label, colour)
        )?;
    }
    Ok(())
}

fn render_wide_cockpit_activity(
    snapshot: &SystemSnapshot,
    cpu_activity: &CpuActivity,
    network_activity: &NetworkActivity,
    width: usize,
    stdout: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    let gap = 2usize;
    let available = width.saturating_sub(4 + gap);
    let cpu_width = available / 2;
    let network_width = available.saturating_sub(cpu_width);
    let cpu_graph_width = cpu_width.saturating_sub(11);
    let network_graph_width = network_width.saturating_sub(12);

    let cpu_peak = cpu_activity.history.iter().copied().max().unwrap_or(0);
    let cpu_graph = block_chart_rows(&cpu_activity.history, cpu_graph_width, 100, 4);
    let cpu_lines = panel_lines(
        "CPU PULSE · ● LIVE · 60s",
        &[
            format!(
                "{:>5.1}% current  ·  {:>3}% peak  ·  {} logical CPU{}",
                snapshot.host.cpu_percent,
                cpu_peak,
                snapshot.host.cpu_count,
                if snapshot.host.cpu_count == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            format!("100 ┤{}", cpu_graph[0]),
            format!(" 75 ┤{}", cpu_graph[1]),
            format!(" 50 ┤{}", cpu_graph[2]),
            format!(" 25 ┤{}", cpu_graph[3]),
        ],
        cpu_width,
    );

    let (rx, tx) = network_activity.current().unwrap_or_default();
    let rx_peak = network_activity
        .rx_history
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let tx_peak = network_activity
        .tx_history
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let rx_graph = block_chart_rows(
        &network_activity.rx_history,
        network_graph_width,
        rx_peak,
        2,
    );
    let tx_graph = block_chart_rows(
        &network_activity.tx_history,
        network_graph_width,
        tx_peak,
        2,
    );
    let network_lines = panel_lines(
        "NETWORK FLOW · ● LIVE · 60s",
        &[
            format!(
                "↓ {}/s  peak {}/s  ·  ↑ {}/s  peak {}/s",
                human_bytes(rx),
                human_bytes(rx_peak),
                human_bytes(tx),
                human_bytes(tx_peak),
            ),
            format!("RX max ┤{}", rx_graph[0]),
            format!("RX   0 ┤{}", rx_graph[1]),
            format!("TX max ┤{}", tx_graph[0]),
            format!("TX   0 ┤{}", tx_graph[1]),
        ],
        network_width,
    );

    writeln!(stdout)?;
    for (cpu_line, network_line) in cpu_lines.iter().zip(&network_lines) {
        writeln!(
            stdout,
            "  {}  {}",
            ink(cpu_line, Ink::Info, colour),
            ink(network_line, Ink::Attention, colour),
        )?;
    }
    Ok(())
}

fn panel_lines(title: &str, content: &[String], width: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(content.len() + 2);
    let title_width = title.chars().count();
    lines.push(format!(
        "╭─ {title} {}╮",
        "─".repeat(width.saturating_sub(title_width + 5))
    ));
    let content_width = width.saturating_sub(4);
    for line in content {
        let line = truncate_text(line, content_width);
        let padding = content_width.saturating_sub(line.chars().count());
        lines.push(format!("│ {line}{} │", " ".repeat(padding)));
    }
    lines.push(format!("╰{}╯", "─".repeat(width.saturating_sub(2))));
    lines
}

fn fixed_chart_cell(chart: &str, width: usize) -> String {
    let chart = truncate_text(chart, width);
    format!(
        "{}{}",
        " ".repeat(width.saturating_sub(chart.chars().count())),
        chart
    )
}

fn block_chart_rows(
    values: &VecDeque<u64>,
    width: usize,
    maximum: u64,
    height: usize,
) -> Vec<String> {
    const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let visible_start = values.len().saturating_sub(width);
    let visible: Vec<_> = values.iter().skip(visible_start).copied().collect();
    let left_padding = width.saturating_sub(visible.len());
    let levels = height.saturating_mul(8);
    (0..height)
        .map(|row| {
            let lower_level = height.saturating_sub(row + 1).saturating_mul(8);
            let mut output = " ".repeat(left_padding);
            for value in &visible {
                let filled = if maximum == 0 {
                    0
                } else {
                    (*value as u128 * levels as u128).div_ceil(maximum as u128) as usize
                };
                let row_fill = filled.saturating_sub(lower_level).min(8);
                output.push(if row == height.saturating_sub(1) && row_fill == 0 {
                    BLOCKS[1]
                } else {
                    BLOCKS[row_fill]
                });
            }
            output
        })
        .collect()
}

fn present_frame(stdout: &mut impl Write, frame: &[u8]) -> Result<()> {
    let mut update = Vec::with_capacity(frame.len().saturating_add(256));
    execute!(update, cursor::MoveTo(0, 0))?;
    execute!(update, terminal::Clear(ClearType::CurrentLine))?;
    for byte in frame {
        update.push(*byte);
        if *byte == b'\n' {
            execute!(update, terminal::Clear(ClearType::CurrentLine))?;
        }
    }
    execute!(update, terminal::Clear(ClearType::FromCursorDown))?;
    stdout.write_all(&update)?;
    stdout.flush()?;
    Ok(())
}

fn clear_gap_and_anchor_footer(stdout: &mut impl Write, rows: u16) -> Result<()> {
    // Detail screens are usually shorter than their list screens. Erase the vacated rows before
    // jumping to the anchored footer so content from the previous frame cannot remain visible.
    // This is part of the buffered frame, avoiding a separate visible clear on slow terminals.
    execute!(
        stdout,
        terminal::Clear(ClearType::FromCursorDown),
        cursor::MoveTo(0, rows.saturating_sub(3))
    )?;
    Ok(())
}

fn cockpit_view_summary(view: View, snapshot: &SystemSnapshot, loading: bool) -> String {
    if loading && (view != View::Processes || snapshot.processes.is_empty()) {
        return "checking…".into();
    }
    match view {
        View::Processes => format!("{} processes", snapshot.processes.len()),
        View::Services => format!("{} services", snapshot.services.len()),
        View::Logs if snapshot.logs.is_empty() && log_collection_failed(snapshot) => {
            "log collection unavailable".into()
        }
        View::Logs => cockpit_log_summary(snapshot),
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
        View::Hardware => format!(
            "{} sensors · {} USB · {} serial",
            snapshot.temperatures.len(),
            snapshot
                .hardware_devices
                .iter()
                .filter(|device| device.kind == "usb")
                .count(),
            snapshot
                .hardware_devices
                .iter()
                .filter(|device| device.kind == "serial")
                .count()
        ),
        View::System => {
            let clock = match snapshot.clock.ntp_synchronized {
                Some(true) => "clock synced",
                Some(false) => "clock not synced",
                None => "clock status unknown",
            };
            format!(
                "{} · {} DNS · {} login users",
                clock,
                snapshot.dns.nameservers.len(),
                interactive_accounts(snapshot).count()
            )
        }
        View::Health => format!("{} findings", snapshot.findings.len()),
    }
}

fn cockpit_log_summary(snapshot: &SystemSnapshot) -> String {
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for entry in &snapshot.logs {
        match entry
            .priority
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "emerg" | "alert" | "crit" | "critical" | "err" | "error" => errors += 1,
            "warning" | "warn" => warnings += 1,
            _ => {}
        }
    }
    match (errors, warnings, snapshot.logs.is_empty()) {
        (0, 0, true) => "no matching log entries".into(),
        (0, 0, false) => "no flagged log entries".into(),
        (errors, 0, _) => format!("{errors} errors"),
        (0, warnings, _) => format!("{warnings} warnings"),
        (errors, warnings, _) => format!("{errors} errors · {warnings} warnings"),
    }
}

fn interactive_accounts(snapshot: &SystemSnapshot) -> impl Iterator<Item = &AccountInfo> {
    snapshot.accounts.iter().filter(|account| {
        Path::new(&account.shell)
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|shell| {
                matches!(
                    shell,
                    "sh" | "bash" | "zsh" | "fish" | "csh" | "tcsh" | "ksh" | "dash" | "ash"
                )
            })
    })
}

fn log_collection_failed(snapshot: &SystemSnapshot) -> bool {
    snapshot.collection_warnings.iter().any(|warning| {
        warning.contains("journalctl")
            || warning.contains("/usr/bin/log")
            || warning.contains("log timed out")
    })
}

fn cockpit_log_since(requested: Option<&str>) -> Option<&str> {
    #[cfg(target_os = "macos")]
    return requested.or(Some("1m"));
    #[cfg(not(target_os = "macos"))]
    return requested;
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

static TERMINAL_THEME: AtomicU8 = AtomicU8::new(0);

fn set_terminal_theme(theme: ThemeMode) {
    let value = match theme {
        ThemeMode::Auto => 0,
        ThemeMode::Dark => 1,
        ThemeMode::Light => 2,
    };
    TERMINAL_THEME.store(value, AtomicOrdering::Relaxed);
}

fn terminal_has_light_background() -> bool {
    match TERMINAL_THEME.load(AtomicOrdering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            match env::var("LENS_THEME")
                .ok()
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("light") => return true,
                Some("dark") => return false,
                _ => {}
            }
            env::var("COLORFGBG")
                .ok()
                .and_then(|value| value.rsplit([';', ':']).next()?.parse::<u8>().ok())
                .is_some_and(|background| background == 7 || background >= 9)
        }
    }
}

impl Ink {
    const fn foreground(self, light: bool) -> &'static str {
        match (self, light) {
            (Self::Bright, _) => "1;39",
            (Self::Brand, _) => "1;38;2;143;91;215",
            (Self::Info, _) => "1;38;2;0;126;163",
            (Self::Success, _) => "1;38;2;0;137;94",
            (Self::Attention, _) => "1;38;2;166;95;0",
            (Self::Critical, _) => "1;38;2;199;51;80",
            (Self::Label, true) => "1;38;2;65;78;96",
            (Self::Muted, true) => "38;2;72;85;102",
            (Self::Border, true) => "38;2;105;116;132",
            (Self::Label, false) => "1;38;2;105;116;132",
            (Self::Muted, false) => "38;2;105;116;132",
            (Self::Border, false) => "38;2;48;62;84",
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
        format!(
            "\x1b[{}m{text}\x1b[0m",
            colour.foreground(terminal_has_light_background())
        )
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
        View::Hardware => hardware_rows(snapshot).len(),
        View::System => system_rows(snapshot).len(),
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
        View::Hardware => {
            hardware_identity_present(&snapshot.hardware)
                || !snapshot.temperatures.is_empty()
                || !snapshot.hardware_devices.is_empty()
        }
        View::System => !snapshot.dns.source.is_empty() || !snapshot.accounts.is_empty(),
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
        View::System => selected.min(specialist_item_count(view, next).saturating_sub(1)),
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

fn mask_identifier(value: &str) -> String {
    let visible: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if value.chars().count() <= 4 {
        "•".repeat(value.chars().count())
    } else {
        format!("••••{visible}")
    }
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
    network_activity: &NetworkActivity,
    selected: usize,
    inspecting: bool,
    loading: bool,
    status: &str,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    let mut frame = Vec::new();
    render_specialist_content(
        view,
        snapshot,
        network_activity,
        selected,
        inspecting,
        loading,
        status,
        rows,
        &mut frame,
    )?;
    present_frame(stdout, &frame)
}

#[allow(clippy::too_many_arguments)]
fn render_specialist_content(
    view: View,
    snapshot: &SystemSnapshot,
    network_activity: &NetworkActivity,
    selected: usize,
    inspecting: bool,
    loading: bool,
    status: &str,
    rows: u16,
    stdout: &mut impl Write,
) -> Result<()> {
    let columns = terminal::size().map_or(100, |(width, _)| width);
    if columns < 36 || rows < 10 {
        writeln!(stdout, "LENS / {}", view.title().to_ascii_uppercase())?;
        writeln!(stdout, "Terminal too small ({columns}x{rows}).")?;
        writeln!(stdout, "Resize to at least 36x10 or press q to quit.")?;
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
        View::Net => render_net_specialist(
            snapshot,
            network_activity,
            selected,
            inspecting,
            rows,
            width,
            stdout,
        )?,
        View::Hardware => {
            render_hardware_specialist(snapshot, selected, inspecting, rows, width, stdout)?
        }
        View::System => {
            render_system_specialist(snapshot, selected, inspecting, rows, width, stdout)?
        }
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

    if rows >= 30 {
        clear_gap_and_anchor_footer(stdout, rows)?;
    } else {
        writeln!(stdout)?;
    }
    writeln!(stdout, "{}", ink(&format!("├{rule}┤"), Ink::Border, colour))?;
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
    } else if width >= 80
        && view == View::System
        && !inspecting
        && specialist_item_count(view, snapshot) > 0
    {
        writeln!(
            stdout,
            "  {} {}   {} {}   {} {}   {} {}   {} {}   {} {}",
            keycap("↑↓", colour),
            ink("move", Ink::Muted, colour),
            keycap("Tab", colour),
            ink("section", Ink::Muted, colour),
            keycap("1-5", colour),
            ink("jump", Ink::Muted, colour),
            keycap("↵", colour),
            ink("inspect", Ink::Muted, colour),
            keycap("/", colour),
            ink("search", Ink::Muted, colour),
            keycap("q", colour),
            ink("quit", Ink::Muted, colour),
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
            let action_hint = if view == View::Services && cfg!(target_os = "linux") {
                format!(
                    "   {} {}",
                    keycap("a", colour),
                    ink("action", Ink::Muted, colour)
                )
            } else {
                String::new()
            };
            writeln!(
                stdout,
                "  {} {}   {} {}   {} {}{}   {} {}   {} {}   {} {}",
                keycap("↑↓", colour),
                ink("move", Ink::Muted, colour),
                keycap("↵", colour),
                ink("inspect", Ink::Muted, colour),
                keycap("/", colour),
                ink("search", Ink::Muted, colour),
                action_hint,
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
    write!(stdout, "{}", ink(&format!("╰{rule}╯"), Ink::Border, colour))?;
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
            if width >= 80 {
                let value_width = width.saturating_sub(28) / 2;
                writeln!(
                    out,
                    "  {} {:<value_width$}  {} {}",
                    ink("SOURCE", Ink::Label, colour),
                    truncate_text(&mount.source, value_width),
                    ink("FILESYSTEM", Ink::Label, colour),
                    mount.filesystem,
                )?;
                writeln!(
                    out,
                    "  {} {:<value_width$}  {} {}",
                    ink("USED", Ink::Label, colour),
                    human_bytes(mount.used_bytes),
                    ink("AVAILABLE", Ink::Label, colour),
                    human_bytes(mount.available_bytes),
                )?;
                let inode_summary = match (mount.inode_used, mount.inode_total) {
                    (Some(used), Some(total)) => format!("{used} of {total}"),
                    _ => "unavailable".into(),
                };
                writeln!(
                    out,
                    "  {} {:<value_width$}  {} {}",
                    ink("CAPACITY", Ink::Label, colour),
                    format!("{:.1}%", mount.used_percent),
                    ink("INODES", Ink::Label, colour),
                    inode_summary,
                )?;
            } else {
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
            }
        } else {
            let device = &snapshot.block_devices[selected - snapshot.mounts.len()];
            writeln!(out, "\n  {}", ink("BLOCK DEVICE", Ink::Label, colour))?;
            writeln!(out, "  {}", ink(&device.name, Ink::Bright, colour))?;
            if width >= 80 {
                let value_width = width.saturating_sub(22) / 2;
                writeln!(
                    out,
                    "  {} {:<value_width$}  {} {}",
                    ink("TYPE", Ink::Label, colour),
                    device.kind,
                    ink("SIZE", Ink::Label, colour),
                    human_bytes(device.size_bytes)
                )?;
            } else {
                writeln!(out, "  {} {}", ink("TYPE", Ink::Label, colour), device.kind)?;
                writeln!(
                    out,
                    "  {} {}",
                    ink("SIZE", Ink::Label, colour),
                    human_bytes(device.size_bytes)
                )?;
            }
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
    activity: &NetworkActivity,
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
            if let Some((rx, tx)) = activity.interface_rate(&interface.name) {
                writeln!(
                    out,
                    "  {} ↓ {}/s  ↑ {}/s",
                    ink("ACTIVITY", Ink::Label, colour),
                    human_bytes(rx),
                    human_bytes(tx)
                )?;
            } else if interface.rx_bytes.is_some() {
                writeln!(
                    out,
                    "  {} collecting the next sample…",
                    ink("ACTIVITY", Ink::Label, colour)
                )?;
            }
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
                    writeln!(
                        out,
                        "  {} {}",
                        ink("ICCID", Ink::Label, colour),
                        mask_identifier(iccid)
                    )?;
                }
            }
        }
        return Ok(());
    }
    let has_activity = activity.current().is_some();
    if let Some((rx, tx)) = activity.current() {
        writeln!(
            out,
            "  {}  ↓ {}/s  ↑ {}/s",
            ink("ACTIVITY", Ink::Label, colour),
            human_bytes(rx),
            human_bytes(tx)
        )?;
        if width >= 70 {
            let chart_width = width.saturating_sub(13).min(60);
            writeln!(
                out,
                "  {} {}",
                ink("RX", Ink::Info, colour),
                ink(
                    &activity_sparkline(&activity.rx_history, chart_width),
                    Ink::Info,
                    colour
                )
            )?;
            writeln!(
                out,
                "  {} {}",
                ink("TX", Ink::Attention, colour),
                ink(
                    &activity_sparkline(&activity.tx_history, chart_width),
                    Ink::Attention,
                    colour
                )
            )?;
        }
    } else if snapshot
        .interfaces
        .iter()
        .any(|interface| interface.rx_bytes.is_some())
    {
        writeln!(
            out,
            "  {} collecting the next sample…",
            ink("ACTIVITY", Ink::Label, colour)
        )?;
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
    let activity_rows = if has_activity && width >= 70 { 3 } else { 1 };
    let capacity = usize::from(rows.saturating_sub(9 + activity_rows)).max(1);
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
        if finding
            .related_entities
            .iter()
            .any(|entity| matches!(entity, EntityId::Mount(_)))
        {
            writeln!(
                out,
                "\n  {}  {}",
                keycap("Enter", colour),
                ink("open the affected mount in Storage", Ink::Info, colour)
            )?;
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

fn hardware_identity_present(identity: &HardwareIdentity) -> bool {
    identity.manufacturer.is_some()
        || identity.model.is_some()
        || identity.board.is_some()
        || identity.board_revision.is_some()
        || identity.serial_number.is_some()
        || identity.firmware_version.is_some()
        || identity.raspberry_pi.is_some()
}

fn hardware_rows(snapshot: &SystemSnapshot) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    if hardware_identity_present(&snapshot.hardware) {
        let title = snapshot
            .hardware
            .model
            .clone()
            .or_else(|| snapshot.hardware.board.clone())
            .unwrap_or_else(|| "System hardware".into());
        let details = [
            snapshot
                .hardware
                .manufacturer
                .as_deref()
                .map(|value| format!("Manufacturer: {value}")),
            snapshot
                .hardware
                .board
                .as_deref()
                .map(|value| format!("Board: {value}")),
            snapshot
                .hardware
                .board_revision
                .as_deref()
                .map(|value| format!("Revision: {value}")),
            snapshot
                .hardware
                .serial_number
                .as_deref()
                .map(|value| format!("Serial: {value}")),
            snapshot
                .hardware
                .firmware_version
                .as_deref()
                .map(|value| format!("Firmware: {value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        rows.push(("SYSTEM".into(), title, details));
        if let Some(pi) = &snapshot.hardware.raspberry_pi {
            let state = if pi.throttled_raw.is_none() {
                "Firmware status unavailable".into()
            } else if pi.active_conditions.is_empty() {
                "Power and thermal state normal".into()
            } else {
                pi.active_conditions.join(", ")
            };
            let mut details = format!(
                "Current: {}\nRecorded: {}",
                if pi.throttled_raw.is_none() {
                    "unavailable".into()
                } else if pi.active_conditions.is_empty() {
                    "none".into()
                } else {
                    pi.active_conditions.join(", ")
                },
                if pi.throttled_raw.is_none() {
                    "unavailable".into()
                } else if pi.historical_conditions.is_empty() {
                    "none".into()
                } else {
                    pi.historical_conditions.join(", ")
                }
            );
            if let Some(raw) = pi.throttled_raw {
                details.push_str(&format!("\nFirmware flags: 0x{raw:x}"));
            }
            rows.push(("RASPBERRY PI".into(), state, details));
        }
    }
    rows.extend(snapshot.temperatures.iter().map(|sensor| {
        let mut details = format!("Source: {}", sensor.source);
        if let Some(maximum) = sensor.max_c {
            details.push_str(&format!("\nMaximum: {maximum:.1} °C"));
        }
        if let Some(critical) = sensor.critical_c {
            details.push_str(&format!("\nCritical: {critical:.1} °C"));
        }
        (
            "TEMPERATURE".into(),
            format!("{} · {:.1} °C", sensor.name, sensor.temperature_c),
            details,
        )
    }));
    rows.extend(snapshot.hardware_devices.iter().map(|device| {
        let details = [
            Some(format!("Path: {}", device.path)),
            device
                .manufacturer
                .as_deref()
                .map(|value| format!("Manufacturer: {value}")),
            device
                .vendor_id
                .as_deref()
                .map(|value| format!("Vendor ID: {value}")),
            device
                .product_id
                .as_deref()
                .map(|value| format!("Product ID: {value}")),
            device
                .serial_number
                .as_deref()
                .map(|value| format!("Serial: {value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        (
            device.kind.to_ascii_uppercase(),
            device.name.clone(),
            details,
        )
    }));
    rows
}

fn render_hardware_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    let items = hardware_rows(snapshot);
    if items.is_empty() {
        writeln!(
            out,
            "\n  {}",
            ink("No hardware inventory was detected.", Ink::Muted, colour)
        )?;
        return Ok(());
    }
    let (kind, value, details) = &items[selected.min(items.len() - 1)];
    if inspecting {
        writeln!(out, "\n  {}", ink(kind, Ink::Label, colour))?;
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(value, width.saturating_sub(4)),
                Ink::Bright,
                colour
            )
        )?;
        for line in details.lines() {
            writeln!(
                out,
                "  {}",
                ink(
                    &truncate_text(line, width.saturating_sub(4)),
                    Ink::Muted,
                    colour
                )
            )?;
        }
        return Ok(());
    }
    writeln!(
        out,
        "  Identity, temperatures, firmware status and attached devices\n"
    )?;
    let capacity = usize::from(rows.saturating_sub(11)).max(1);
    let start = viewport_start(selected, items.len(), capacity);
    for (index, (kind, value, _)) in items.iter().enumerate().skip(start).take(capacity) {
        let row = format!(
            "  {:<13} {}",
            kind,
            truncate_text(value, width.saturating_sub(18))
        );
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(out, "{}", ink(&row, Ink::Muted, colour))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSection {
    Clock,
    Dns,
    Users,
    Groups,
    Certificates,
}

impl SystemSection {
    const ALL: [Self; 5] = [
        Self::Clock,
        Self::Dns,
        Self::Users,
        Self::Groups,
        Self::Certificates,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Clock => "CLOCK/NTP",
            Self::Dns => "DNS",
            Self::Users => "USERS",
            Self::Groups => "GROUPS",
            Self::Certificates => "CERTIFICATES",
        }
    }
}

#[derive(Debug, Clone)]
struct SystemRow {
    section: SystemSection,
    kind: &'static str,
    value: String,
}

fn system_rows(snapshot: &SystemSnapshot) -> Vec<SystemRow> {
    let mut rows = vec![
        SystemRow {
            section: SystemSection::Clock,
            kind: "TIMEZONE",
            value: snapshot
                .clock
                .timezone
                .clone()
                .unwrap_or_else(|| "unavailable".into()),
        },
        SystemRow {
            section: SystemSection::Clock,
            kind: "NTP SYNC",
            value: snapshot.clock.ntp_synchronized.map_or_else(
                || "unavailable".into(),
                |value| if value { "yes".into() } else { "no".into() },
            ),
        },
        SystemRow {
            section: SystemSection::Clock,
            kind: "NTP SERVICE",
            value: snapshot
                .clock
                .ntp_service
                .clone()
                .unwrap_or_else(|| "unavailable".into()),
        },
        SystemRow {
            section: SystemSection::Dns,
            kind: "DNS SOURCE",
            value: if snapshot.dns.source.is_empty() {
                "unavailable".into()
            } else {
                snapshot.dns.source.clone()
            },
        },
    ];
    if snapshot.dns.nameservers.is_empty() {
        rows.push(SystemRow {
            section: SystemSection::Dns,
            kind: "DNS SERVER",
            value: "none visible".into(),
        });
    } else {
        rows.extend(
            snapshot
                .dns
                .nameservers
                .iter()
                .cloned()
                .map(|value| SystemRow {
                    section: SystemSection::Dns,
                    kind: "DNS SERVER",
                    value,
                }),
        );
    }
    if snapshot.dns.search_domains.is_empty() {
        rows.push(SystemRow {
            section: SystemSection::Dns,
            kind: "DNS SEARCH",
            value: "none configured".into(),
        });
    } else {
        rows.extend(
            snapshot
                .dns
                .search_domains
                .iter()
                .cloned()
                .map(|value| SystemRow {
                    section: SystemSection::Dns,
                    kind: "DNS SEARCH",
                    value,
                }),
        );
    }
    let visible_accounts = interactive_accounts(snapshot).collect::<Vec<_>>();
    if visible_accounts.is_empty() {
        rows.push(SystemRow {
            section: SystemSection::Users,
            kind: "USER",
            value: "no login-capable users visible".into(),
        });
    } else {
        rows.extend(visible_accounts.iter().map(|item| SystemRow {
            section: SystemSection::Users,
            kind: "USER",
            value: format!(
                "{} · uid {} · gid {} · {} · {}",
                item.name, item.uid, item.gid, item.home, item.shell
            ),
        }));
    }
    let visible_groups = snapshot
        .groups
        .iter()
        .filter(|group| {
            !group.members.is_empty()
                || visible_accounts
                    .iter()
                    .any(|account| account.gid == group.gid)
        })
        .collect::<Vec<_>>();
    if visible_groups.is_empty() {
        rows.push(SystemRow {
            section: SystemSection::Groups,
            kind: "GROUP",
            value: "no groups relevant to login-capable users".into(),
        });
    } else {
        rows.extend(visible_groups.into_iter().map(|item| SystemRow {
            section: SystemSection::Groups,
            kind: "GROUP",
            value: format!(
                "{} · gid {}{}",
                item.name,
                item.gid,
                if item.members.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", item.members.join(","))
                }
            ),
        }));
    }
    if snapshot.certificates.is_empty() {
        rows.push(SystemRow {
            section: SystemSection::Certificates,
            kind: "CERTIFICATE",
            value: "no locally managed certificates visible".into(),
        });
    } else {
        rows.extend(snapshot.certificates.iter().map(|item| SystemRow {
            section: SystemSection::Certificates,
            kind: "CERTIFICATE",
            value: certificate_summary(item),
        }));
    }
    rows
}

fn system_section_start(snapshot: &SystemSnapshot, section: SystemSection) -> usize {
    system_rows(snapshot)
        .iter()
        .position(|row| row.section == section)
        .unwrap_or(0)
}

fn move_system_section(snapshot: &SystemSnapshot, selected: usize, delta: isize) -> usize {
    let rows = system_rows(snapshot);
    let current = rows
        .get(selected)
        .map_or(SystemSection::Clock, |row| row.section);
    let index = SystemSection::ALL
        .iter()
        .position(|section| *section == current)
        .unwrap_or(0)
        .saturating_add_signed(delta)
        .min(SystemSection::ALL.len() - 1);
    system_section_start(snapshot, SystemSection::ALL[index])
}

fn render_system_section_nav(
    active: SystemSection,
    width: usize,
    colour: bool,
    out: &mut impl Write,
) -> Result<usize> {
    write!(out, "  {}", ink("SECTIONS", Ink::Label, colour))?;
    let mut line_width = 10usize;
    let mut line_count = 1usize;
    for (index, section) in SystemSection::ALL.iter().enumerate() {
        let label = format!("{} {}", index + 1, section.label());
        let separator = if line_width <= 2 { "" } else { "  " };
        if line_width + separator.len() + label.len() > width.saturating_sub(2) {
            writeln!(out)?;
            write!(out, "  ")?;
            line_width = 2;
            line_count += 1;
        } else {
            write!(out, "{separator}")?;
            line_width += separator.len();
        }
        write!(
            out,
            "{}",
            ink(
                &label,
                if *section == active {
                    Ink::Bright
                } else {
                    Ink::Muted
                },
                colour
            )
        )?;
        line_width += label.len();
    }
    writeln!(out)?;
    Ok(line_count)
}

fn certificate_summary(item: &CertificateInfo) -> String {
    let identity = item
        .subject
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(&item.path);
    item.not_after.as_deref().map_or_else(
        || identity.to_owned(),
        |expiry| format!("{identity} · expires {expiry}"),
    )
}

fn render_system_specialist(
    snapshot: &SystemSnapshot,
    selected: usize,
    inspecting: bool,
    rows: u16,
    width: usize,
    out: &mut impl Write,
) -> Result<()> {
    let colour = terminal_colour_enabled();
    let items = system_rows(snapshot);
    if items.is_empty() {
        writeln!(
            out,
            "\n  {}",
            ink(
                "No system context is visible to this user.",
                Ink::Muted,
                colour
            )
        )?;
        return Ok(());
    }
    let item = &items[selected.min(items.len() - 1)];
    let kind = item.kind;
    let value = &item.value;
    if inspecting {
        writeln!(out, "\n  {}", ink(kind, Ink::Label, colour))?;
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(value, width.saturating_sub(4)),
                Ink::Bright,
                colour
            )
        )?;
        if kind == "CERTIFICATE"
            && let Some(certificate) = snapshot
                .certificates
                .iter()
                .find(|item| certificate_summary(item) == *value)
        {
            if let Some(subject) = &certificate.subject {
                writeln!(out, "\n  {:<10} {}", "SUBJECT", subject)?;
            }
            if let Some(issuer) = &certificate.issuer {
                writeln!(out, "  {:<10} {}", "ISSUER", issuer)?;
            }
            if let Some(expiry) = &certificate.not_after {
                writeln!(out, "  {:<10} {}", "EXPIRES", expiry)?;
            }
            writeln!(out, "  {:<10} {}", "PATH", certificate.path)?;
            writeln!(
                out,
                "\n  {}",
                ink(
                    "Public certificate metadata only; private keys are never opened.",
                    Ink::Muted,
                    colour
                )
            )?;
        }
        return Ok(());
    }
    let section_lines = render_system_section_nav(item.section, width, colour, out)?;
    writeln!(
        out,
        "  {}\n",
        ink("Tab changes section", Ink::Muted, colour)
    )?;
    let reserved = 12usize.saturating_add(section_lines);
    let capacity = usize::from(rows).saturating_sub(reserved).max(1);
    // Keep context above and below the cursor when crossing a section boundary.
    // Section jumps remain obvious in the navigation strip without forcing the
    // selected row to the top of the viewport.
    let start = viewport_start(selected, items.len(), capacity);
    for (index, item) in items.iter().enumerate().skip(start).take(capacity) {
        let row = format!(
            "  {:<12} {}",
            item.kind,
            truncate_text(&item.value, width.saturating_sub(17))
        );
        if index == selected {
            writeln!(out, "{}", selected_row(&row, colour))?;
        } else {
            writeln!(out, "{}", ink(&row, Ink::Muted, colour))?;
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
    let mut service_action: Option<ServiceActionDialog> = None;
    let mut network_activity = NetworkActivity::default();
    let mut activity_receiver: Option<Receiver<Vec<(String, u64, u64)>>> = None;
    let mut next_network_sample = Instant::now() + Duration::from_secs(1);
    let clock_interval = if view == View::Net { 1 } else { 60 };
    let mut next_clock = Instant::now() + Duration::from_secs(clock_interval);
    let mut redraw = true;
    loop {
        if Instant::now() >= next_clock {
            next_clock = Instant::now() + Duration::from_secs(clock_interval);
            redraw = true;
        }
        if diagnostic.poll() {
            redraw = true;
        }
        if service_action
            .as_mut()
            .is_some_and(ServiceActionDialog::poll)
        {
            redraw = true;
        }
        if let Some(counter_updates) = activity_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            let counter_updates: BTreeMap<_, _> = counter_updates
                .into_iter()
                .map(|(name, rx, tx)| (name, (rx, tx)))
                .collect();
            for interface in &mut snapshot.interfaces {
                if let Some((rx, tx)) = counter_updates.get(&interface.name) {
                    interface.rx_bytes = Some(*rx);
                    interface.tx_bytes = Some(*tx);
                }
            }
            network_activity.observe(&snapshot.interfaces, Instant::now());
            activity_receiver = None;
            next_network_sample = Instant::now() + Duration::from_secs(1);
            redraw = true;
        }
        if view == View::Net
            && !snapshot.interfaces.is_empty()
            && activity_receiver.is_none()
            && Instant::now() >= next_network_sample
        {
            activity_receiver = Some(spawn_interface_counter_collection());
        }
        match receiver.try_recv() {
            Ok(update) => {
                selected =
                    preserve_specialist_selection(view, &snapshot, &update.snapshot, selected);
                snapshot = update.snapshot;
                if view == View::Net && network_activity.last_sample.is_none() {
                    network_activity.observe(&snapshot.interfaces, Instant::now());
                }
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
                view,
                &snapshot,
                &network_activity,
                selected,
                inspecting,
                loading,
                &status,
                rows,
                stdout,
            )?;
            if let Some(query) = search_query.as_deref() {
                render_search_overlay(stdout, &format!("Search {}", view.title()), query)?;
            }
            if diagnostic.open {
                render_diagnostic_overlay(stdout, &diagnostic)?;
            }
            if let Some(dialog) = service_action.as_ref() {
                render_service_action_overlay(stdout, dialog)?;
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
            if let Some(dialog) = service_action.as_mut() {
                let completed = dialog.stage == ServiceActionStage::Result;
                if dialog.handle_key(key) {
                    service_action = None;
                    if completed {
                        snapshot = SystemSnapshot::empty(hostname());
                        network_activity = NetworkActivity::default();
                        activity_receiver = None;
                        receiver = spawn_specialist_collection(view, active_args.clone());
                        loading = true;
                        status = "Refreshing service state after action…".into();
                    }
                }
                redraw = true;
                continue;
            }
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
                        network_activity = NetworkActivity::default();
                        activity_receiver = None;
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
                KeyCode::Tab if view == View::System && !inspecting => {
                    selected = move_system_section(&snapshot, selected, 1);
                    redraw = true;
                }
                KeyCode::BackTab if view == View::System && !inspecting => {
                    selected = move_system_section(&snapshot, selected, -1);
                    redraw = true;
                }
                KeyCode::Char(section @ '1'..='5') if view == View::System && !inspecting => {
                    let index = section.to_digit(10).unwrap_or(1) as usize - 1;
                    selected = system_section_start(&snapshot, SystemSection::ALL[index]);
                    redraw = true;
                }
                KeyCode::Up | KeyCode::Char('k') if !inspecting => {
                    selected = move_selection(selected, -1, specialist_item_count(view, &snapshot));
                    redraw = true;
                }
                KeyCode::Down | KeyCode::Char('j') if !inspecting => {
                    selected = move_selection(selected, 1, specialist_item_count(view, &snapshot));
                    redraw = true;
                }
                KeyCode::Enter if inspecting && view == View::Health => {
                    let mount = snapshot.findings.get(selected).and_then(|finding| {
                        finding
                            .related_entities
                            .iter()
                            .find_map(|entity| match entity {
                                EntityId::Mount(target) => Some(target.clone()),
                                _ => None,
                            })
                    });
                    if let Some(target) = mount {
                        launch_search(View::Disk, &target)?;
                        redraw = true;
                    }
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
                KeyCode::Char('a') if view == View::Services && !inspecting => {
                    #[cfg(target_os = "linux")]
                    if let Some(service) = snapshot.services.get(selected) {
                        service_action = Some(ServiceActionDialog::new(service.name.clone()));
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        status = "Service actions require systemd on Linux; no change was made."
                            .to_owned();
                    }
                    redraw = true;
                }
                KeyCode::Char('r') => {
                    snapshot = SystemSnapshot::empty(hostname());
                    network_activity = NetworkActivity::default();
                    activity_receiver = None;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceActionStage {
    Choose,
    Confirm,
    Running,
    Result,
}

struct ServiceActionDialog {
    target: String,
    selection: usize,
    stage: ServiceActionStage,
    result: String,
    receiver: Option<Receiver<String>>,
}

impl ServiceActionDialog {
    #[cfg(target_os = "linux")]
    fn new(target: String) -> Self {
        Self {
            target,
            selection: 0,
            stage: ServiceActionStage::Choose,
            result: String::new(),
            receiver: None,
        }
    }

    fn poll(&mut self) -> bool {
        let Some(receiver) = self.receiver.as_ref() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.result = result;
                self.stage = ServiceActionStage::Result;
                self.receiver = None;
                true
            }
            Err(TryRecvError::Disconnected) => {
                self.result = "Service action stopped unexpectedly; check the unit state.".into();
                self.stage = ServiceActionStage::Result;
                self.receiver = None;
                true
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match self.stage {
            ServiceActionStage::Choose => match key.code {
                KeyCode::Esc => return true,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selection = (self.selection + 1).min(ServiceAction::ALL.len() - 1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selection = self.selection.saturating_sub(1);
                }
                KeyCode::Enter => self.stage = ServiceActionStage::Confirm,
                _ => {}
            },
            ServiceActionStage::Confirm => match key.code {
                KeyCode::Esc => self.stage = ServiceActionStage::Choose,
                KeyCode::Char('y') | KeyCode::Char('Y') => self.execute(),
                _ => {}
            },
            ServiceActionStage::Running => {}
            ServiceActionStage::Result => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return true;
                }
            }
        }
        false
    }

    fn execute(&mut self) {
        let action = ServiceAction::ALL[self.selection];
        let target = self.target.clone();
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.stage = ServiceActionStage::Running;
        thread::spawn(move || {
            let _ = sender.send(run_service_action_command(action, &target));
        });
    }
}

fn run_service_action_command(action: ServiceAction, target: &str) -> String {
    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return format!("Unable to locate lens-services: {error}"),
    };
    match Command::new(executable)
        .args(["--action", action.cli_name(), "--target", target, "--yes"])
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let text = sanitise_terminal_output(&text);
            if output.status.success() {
                text.trim().to_owned()
            } else {
                format!("Action failed: {}", text.trim())
            }
        }
        Err(error) => format!("Unable to run service action: {error}"),
    }
}

fn render_service_action_overlay(
    stdout: &mut impl Write,
    dialog: &ServiceActionDialog,
) -> Result<()> {
    let (columns, rows) = terminal::size().unwrap_or((80, 24));
    let width = columns.saturating_sub(2).clamp(38, 74);
    let height = 12u16.min(rows.saturating_sub(2));
    let x = columns.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    let inner = usize::from(width.saturating_sub(2));
    let colour = terminal_colour_enabled();
    execute!(stdout, cursor::MoveTo(x, y))?;
    write!(
        stdout,
        "╭─SERVICE ACTION{}╮",
        "─".repeat(inner.saturating_sub(15))
    )?;
    let action = ServiceAction::ALL[dialog.selection];
    let mut lines = match dialog.stage {
        ServiceActionStage::Choose => ServiceAction::ALL
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = if index == dialog.selection {
                    "▶"
                } else {
                    " "
                };
                format!(" {marker} {}", item.label())
            })
            .collect::<Vec<_>>(),
        ServiceActionStage::Confirm => vec![
            String::new(),
            format!(
                "{} {}?",
                action.cli_name().to_ascii_uppercase(),
                dialog.target
            ),
            String::new(),
            "Lens will run one exact systemd action and verify the unit state.".into(),
            String::new(),
            "y confirm · Esc back".into(),
        ],
        ServiceActionStage::Running => vec![
            String::new(),
            format!("Running {} on {}…", action.cli_name(), dialog.target),
            String::new(),
            "Waiting for systemd and verification.".into(),
        ],
        ServiceActionStage::Result => vec![
            String::new(),
            dialog.result.clone(),
            String::new(),
            "Enter or Esc to close".into(),
        ],
    };
    if dialog.stage == ServiceActionStage::Choose {
        lines.push(String::new());
        lines.push(format!(
            "Target: {} · Enter review · Esc cancel",
            dialog.target
        ));
    }
    for row in 0..usize::from(height.saturating_sub(2)) {
        execute!(stdout, cursor::MoveTo(x, y + 1 + row as u16))?;
        let line = lines.get(row).map_or("", String::as_str);
        let line = truncate_text(line, inner);
        let padded = format!("{line:<inner$}");
        let styled = if dialog.stage == ServiceActionStage::Choose && row == dialog.selection {
            selected_row(&padded, colour)
        } else {
            ink(&padded, Ink::Bright, colour)
        };
        write!(stdout, "│{styled}│")?;
    }
    execute!(stdout, cursor::MoveTo(x, y + height - 1))?;
    write!(stdout, "╰{}╯", "─".repeat(inner))?;
    stdout.flush()?;
    Ok(())
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
    now.format(format_description!("[hour]:[minute]"))
        .unwrap_or_else(|_| "--:--".to_owned())
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
        View::Hardware => collect_hardware_context(&mut snapshot),
        View::System => collect_system_context(&mut snapshot),
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
    let (service_result, log_result, disk_result, net_result, hardware_result) =
        thread::scope(|scope| {
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
            let hardware = scope.spawn(hardware::collect);
            (
                services.join(),
                logs.join(),
                disk.join(),
                net.join(),
                hardware.join(),
            )
        });
    let (services, service_warnings) = service_result.unwrap_or_default();
    let (logs, file_sources, log_warnings) = log_result.unwrap_or_default();
    let (mounts, deleted_open_files, block_devices, disk_warnings) =
        disk_result.unwrap_or_default();
    let (interfaces, routes, sockets, cellular_modems, net_warnings) =
        net_result.unwrap_or_default();
    let hardware = hardware_result.unwrap_or_default();
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
    snapshot.hardware = hardware.identity;
    snapshot.temperatures = hardware.temperatures;
    snapshot.hardware_devices = hardware.devices;
    collect_system_context(&mut snapshot);
    snapshot.findings = diagnose(&snapshot);
    snapshot
        .relationships
        .extend(domain_relationships(&snapshot));
    snapshot
}

fn collect_hardware_context(snapshot: &mut SystemSnapshot) {
    let hardware = hardware::collect();
    snapshot.hardware = hardware.identity;
    snapshot.temperatures = hardware.temperatures;
    snapshot.hardware_devices = hardware.devices;
}

fn collect_system_context(snapshot: &mut SystemSnapshot) {
    snapshot.clock = collect_clock_context(&mut snapshot.collection_warnings);
    snapshot.dns = collect_dns_context(&mut snapshot.collection_warnings);
    snapshot.certificates = collect_certificates(&mut snapshot.collection_warnings);
    snapshot.accounts = collect_accounts("/etc/passwd", &mut snapshot.collection_warnings);
    snapshot.groups = collect_groups("/etc/group", &mut snapshot.collection_warnings);
}

fn collect_clock_context(warnings: &mut Vec<String>) -> ClockContext {
    #[cfg(not(target_os = "linux"))]
    let _ = warnings;
    let timezone = std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::fs::read_link("/etc/localtime").ok().and_then(|path| {
                path.to_string_lossy()
                    .split("zoneinfo/")
                    .nth(1)
                    .map(str::to_owned)
            })
        });
    #[cfg(target_os = "linux")]
    let status = command(
        "timedatectl",
        &["show", "--property=NTPSynchronized", "--property=NTP"],
        warnings,
    );
    #[cfg(not(target_os = "linux"))]
    let status: Option<String> = None;
    let ntp_synchronized = status.as_deref().and_then(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("NTPSynchronized="))
            .and_then(parse_bool)
    });
    let ntp_service = status.as_deref().and_then(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("NTP="))
            .map(|value| {
                if value.eq_ignore_ascii_case("yes") {
                    "enabled"
                } else {
                    "disabled"
                }
                .to_owned()
            })
    });
    ClockContext {
        timezone,
        ntp_synchronized,
        ntp_service,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn collect_dns_context(warnings: &mut Vec<String>) -> DnsContext {
    let path = "/etc/resolv.conf";
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            warnings.push(format!("DNS configuration {path} unavailable: {error}"));
            return DnsContext {
                source: path.into(),
                ..DnsContext::default()
            };
        }
    };
    let mut dns = DnsContext {
        source: path.into(),
        ..DnsContext::default()
    };
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("nameserver") => dns.nameservers.extend(fields.map(str::to_owned)),
            Some("search" | "domain") => dns.search_domains.extend(fields.map(str::to_owned)),
            _ => {}
        }
    }
    dns
}

fn collect_certificates(warnings: &mut Vec<String>) -> Vec<CertificateInfo> {
    #[cfg(target_os = "macos")]
    {
        let Some(text) = command("/usr/bin/security", &["find-certificate", "-a"], warnings) else {
            return Vec::new();
        };
        let mut keychain = "default keychains".to_owned();
        let mut values = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(value) = line
                .strip_prefix("keychain: \"")
                .and_then(|value| value.strip_suffix('"'))
            {
                keychain = value.to_owned();
            } else if let Some(label) = line
                .strip_prefix("\"alis\"<blob>=\"")
                .and_then(|value| value.strip_suffix('"'))
            {
                values.push(CertificateInfo {
                    path: keychain.clone(),
                    subject: Some(label.to_owned()),
                    issuer: None,
                    not_after: None,
                });
            }
            if values.len() >= 64 {
                break;
            }
        }
        values
    }
    #[cfg(target_os = "linux")]
    {
        // Root CA stores contain hundreds of distribution-managed certificates and drown out the
        // certificates an operator can act on. Inventory locally managed certificate locations,
        // plus non-symlink certificate files placed directly in /etc/ssl/certs.
        let roots = [
            ("/etc/letsencrypt/live", 3, true),
            ("/etc/ssl/localcerts", 2, true),
            ("/usr/local/share/ca-certificates", 3, false),
            ("/etc/ssl/certs", 0, false),
        ];
        let mut paths = Vec::new();
        for (directory, depth, allow_symlinks) in roots {
            collect_certificate_paths(Path::new(directory), depth, allow_symlinks, &mut paths);
        }
        paths.sort();
        paths.dedup();
        paths.truncate(64);

        let openssl_available = Command::new("openssl")
            .arg("version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !openssl_available && !paths.is_empty() {
            warnings.push("openssl unavailable: certificate metadata was not inspected".into());
        }
        paths
            .into_iter()
            .map(|path| certificate_info(&path, openssl_available, warnings))
            .collect()
    }
}

#[cfg(target_os = "linux")]
fn collect_certificate_paths(
    directory: &Path,
    depth: usize,
    allow_symlinks: bool,
    paths: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() && depth > 0 {
            collect_certificate_paths(&path, depth - 1, allow_symlinks, paths);
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() && !allow_symlinks {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let extension = path.extension().and_then(|value| value.to_str());
        let is_certificate = matches!(extension, Some("crt" | "pem"))
            && name != "ca-certificates.crt"
            && (!directory.starts_with("/etc/letsencrypt") || name == "cert.pem");
        if is_certificate && (path.is_file() || file_type.is_symlink()) {
            paths.push(path);
        }
    }
}

#[cfg(target_os = "linux")]
fn certificate_info(
    path: &Path,
    openssl_available: bool,
    warnings: &mut Vec<String>,
) -> CertificateInfo {
    let mut info = CertificateInfo {
        path: path.display().to_string(),
        subject: None,
        issuer: None,
        not_after: None,
    };
    if !openssl_available {
        return info;
    }
    let path_text = path.to_string_lossy();
    let Some(metadata) = command_with_timeout(
        "openssl",
        &[
            "x509", "-noout", "-subject", "-issuer", "-enddate", "-in", &path_text,
        ],
        warnings,
        Duration::from_secs(2),
    ) else {
        return info;
    };
    for line in metadata.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("subject=") {
            info.subject = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("issuer=") {
            info.issuer = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("notAfter=") {
            info.not_after = Some(value.trim().to_owned());
        }
    }
    info
}

fn collect_accounts(path: &str, warnings: &mut Vec<String>) -> Vec<AccountInfo> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            warnings.push(format!("accounts {path} unavailable: {error}"));
            return Vec::new();
        }
    };
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() < 7 {
                return None;
            }
            Some(AccountInfo {
                name: fields[0].into(),
                uid: fields[2].parse().ok()?,
                gid: fields[3].parse().ok()?,
                home: fields[5].into(),
                shell: fields[6].into(),
            })
        })
        .collect()
}

fn collect_groups(path: &str, warnings: &mut Vec<String>) -> Vec<GroupInfo> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            warnings.push(format!("groups {path} unavailable: {error}"));
            return Vec::new();
        }
    };
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() < 4 {
                return None;
            }
            Some(GroupInfo {
                name: fields[0].into(),
                gid: fields[2].parse().ok()?,
                members: fields[3]
                    .split(',')
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
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
    let mut interfaces = if let Some(text) = command("ip", &["-brief", "address", "show"], warnings)
    {
        text.lines().filter_map(parse_interface).collect()
    } else {
        collect_linux_sysfs_interfaces(warnings)
    };
    apply_interface_counters(&mut interfaces, collect_interface_counters(warnings));
    interfaces
}

#[cfg(target_os = "linux")]
fn collect_linux_sysfs_interfaces(warnings: &mut Vec<String>) -> Vec<Interface> {
    let entries = match std::fs::read_dir("/sys/class/net") {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "network interfaces unavailable from sysfs: {error}"
            ));
            return Vec::new();
        }
    };
    let mut interfaces: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            if !entry.path().join("statistics").is_dir() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let state = std::fs::read_to_string(entry.path().join("operstate"))
                .map(|value| value.trim().to_ascii_uppercase())
                .unwrap_or_else(|_| "UNKNOWN".into());
            Some(Interface {
                name,
                state,
                addresses: Vec::new(),
                rx_bytes: None,
                tx_bytes: None,
            })
        })
        .collect();
    interfaces.sort_by(|left, right| left.name.cmp(&right.name));
    interfaces
}

#[cfg(target_os = "macos")]
fn collect_interfaces(warnings: &mut Vec<String>) -> Vec<Interface> {
    let mut interfaces = command("ifconfig", &["-a"], warnings)
        .map(|text| parse_ifconfig(&text))
        .unwrap_or_default();
    apply_interface_counters(&mut interfaces, collect_interface_counters(warnings));
    interfaces
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
                    rx_bytes: None,
                    tx_bytes: None,
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
        rx_bytes: None,
        tx_bytes: None,
    })
}

fn apply_interface_counters(interfaces: &mut [Interface], counters: Vec<(String, u64, u64)>) {
    let counters: BTreeMap<_, _> = counters
        .into_iter()
        .map(|(name, rx, tx)| (name, (rx, tx)))
        .collect();
    for interface in interfaces {
        if let Some((rx, tx)) = counters.get(&interface.name) {
            interface.rx_bytes = Some(*rx);
            interface.tx_bytes = Some(*tx);
        }
    }
}

#[cfg(target_os = "linux")]
fn collect_interface_counters(warnings: &mut Vec<String>) -> Vec<(String, u64, u64)> {
    let entries = match std::fs::read_dir("/sys/class/net") {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("network activity unavailable from sysfs: {error}"));
            return Vec::new();
        }
    };
    let mut counters: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let statistics = entry.path().join("statistics");
            let rx = read_interface_counter(&statistics.join("rx_bytes"))?;
            let tx = read_interface_counter(&statistics.join("tx_bytes"))?;
            Some((name, rx, tx))
        })
        .collect();
    counters.sort_by(|left, right| left.0.cmp(&right.0));
    counters
}

#[cfg(target_os = "linux")]
fn read_interface_counter(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn collect_interface_counters(warnings: &mut Vec<String>) -> Vec<(String, u64, u64)> {
    command("netstat", &["-ibn"], warnings)
        .map(|text| parse_netstat_counters(&text))
        .unwrap_or_default()
}

#[cfg(any(target_os = "macos", test))]
fn parse_netstat_counters(text: &str) -> Vec<(String, u64, u64)> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let columns: Vec<_> = header.split_whitespace().collect();
    let Some(rx_column) = columns.iter().position(|column| *column == "Ibytes") else {
        return Vec::new();
    };
    let Some(tx_column) = columns.iter().position(|column| *column == "Obytes") else {
        return Vec::new();
    };
    let mut counters = BTreeMap::<String, (u64, u64)>::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        let (Some(name), Some(rx), Some(tx)) = (
            fields.first(),
            fields.get(rx_column).and_then(|value| value.parse().ok()),
            fields.get(tx_column).and_then(|value| value.parse().ok()),
        ) else {
            continue;
        };
        let entry = counters.entry((*name).to_owned()).or_default();
        entry.0 = entry.0.max(rx);
        entry.1 = entry.1.max(tx);
    }
    counters
        .into_iter()
        .map(|(name, (rx, tx))| (name, rx, tx))
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_routes(warnings: &mut Vec<String>) -> Vec<Route> {
    if let Some(text) = command("ip", &["route", "show"], warnings) {
        return text
            .lines()
            .enumerate()
            .map(|(index, line)| parse_route(index, line))
            .collect();
    }
    match std::fs::read_to_string("/proc/net/route") {
        Ok(text) => text
            .lines()
            .skip(1)
            .enumerate()
            .filter_map(|(index, line)| parse_proc_route(index, line))
            .collect(),
        Err(error) => {
            warnings.push(format!("network routes unavailable from procfs: {error}"));
            Vec::new()
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_route(index: usize, line: &str) -> Option<Route> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 8 {
        return None;
    }
    let destination = proc_ipv4(fields[1])?;
    let gateway = proc_ipv4(fields[2])?;
    Some(Route {
        id: format!("route-{index}"),
        destination: if destination == "0.0.0.0" {
            "default".into()
        } else {
            destination
        },
        gateway: (gateway != "0.0.0.0").then_some(gateway),
        interface: Some(fields[0].into()),
        raw: line.into(),
    })
}

#[cfg(any(target_os = "linux", test))]
fn proc_ipv4(value: &str) -> Option<String> {
    let value = u32::from_str_radix(value, 16).ok()?;
    Some(format!(
        "{}.{}.{}.{}",
        value & 0xff,
        (value >> 8) & 0xff,
        (value >> 16) & 0xff,
        (value >> 24) & 0xff
    ))
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
    for modem in &snapshot.cellular_modems {
        let label = modem
            .model
            .as_deref()
            .or(modem.manufacturer.as_deref())
            .unwrap_or(&modem.path);
        if modem.state.eq_ignore_ascii_case("failed") {
            findings.push(SystemFinding {
                id: format!("cellular.failed.{}", modem.path),
                severity: Severity::Critical,
                title: "Cellular modem failed".into(),
                summary: format!("{label} is reported in a failed state."),
                evidence: vec![lens_model::Evidence {
                    label: "state".into(),
                    value: modem.state.clone(),
                    unit: None,
                }],
                related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
                suggested_actions: vec![
                    "Open lens-net and inspect the modem, SIM and registration state.".into(),
                    "Review ModemManager logs before reconnecting or replacing hardware.".into(),
                ],
            });
        } else if modem
            .signal_quality_percent
            .is_some_and(|quality| quality <= 15)
            && ["registered", "connected"]
                .iter()
                .any(|state| modem.state.to_ascii_lowercase().contains(state))
        {
            let quality = modem.signal_quality_percent.unwrap_or_default();
            findings.push(SystemFinding {
                id: format!("cellular.weak-signal.{}", modem.path),
                severity: Severity::Attention,
                title: "Weak cellular signal".into(),
                summary: format!("{label} reports {quality}% signal quality."),
                evidence: vec![lens_model::Evidence {
                    label: "signal quality".into(),
                    value: quality.to_string(),
                    unit: Some("percent".into()),
                }],
                related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
                suggested_actions: vec![
                    "Open lens-net and confirm access technology, operator and registration state."
                        .into(),
                    "Check antenna placement and compare signal quality over time.".into(),
                ],
            });
        }
    }
    for sensor in snapshot.temperatures.iter().filter(|sensor| {
        sensor.temperature_c
            >= sensor
                .max_c
                .unwrap_or(80.0)
                .min(sensor.critical_c.unwrap_or(90.0))
    }) {
        let critical = sensor
            .critical_c
            .is_some_and(|threshold| sensor.temperature_c >= threshold)
            || sensor.temperature_c >= 90.0;
        findings.push(SystemFinding {
            id: format!("hardware.temperature.{}", sensor.name),
            severity: if critical {
                Severity::Critical
            } else {
                Severity::Attention
            },
            title: "High hardware temperature".into(),
            summary: format!("{} is {:.1} °C.", sensor.name, sensor.temperature_c),
            evidence: vec![lens_model::Evidence {
                label: sensor.name.clone(),
                value: format!("{:.1}", sensor.temperature_c),
                unit: Some("°C".into()),
            }],
            related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
            suggested_actions: vec![
                "Open lens-hardware and inspect temperature limits and thermal status.".into(),
            ],
        });
    }
    if let Some(pi) = snapshot.hardware.raspberry_pi.as_ref() {
        if !pi.active_conditions.is_empty() {
            findings.push(SystemFinding {
                id: "hardware.raspberry-pi.active-throttling".into(),
                severity: if pi
                    .active_conditions
                    .iter()
                    .any(|condition| condition == "under-voltage" || condition == "throttled")
                {
                    Severity::Critical
                } else {
                    Severity::Attention
                },
                title: "Raspberry Pi power or thermal constraint".into(),
                summary: pi.active_conditions.join(", "),
                evidence: pi
                    .active_conditions
                    .iter()
                    .map(|condition| lens_model::Evidence {
                        label: "active".into(),
                        value: condition.clone(),
                        unit: None,
                    })
                    .collect(),
                related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
                suggested_actions: vec![
                    "Open lens-hardware and check the power supply, cooling and workload.".into(),
                ],
            });
        } else if !pi.historical_conditions.is_empty() {
            findings.push(SystemFinding {
                id: "hardware.raspberry-pi.recorded-throttling".into(),
                severity: Severity::Information,
                title: "Raspberry Pi recorded a power or thermal event".into(),
                summary: pi.historical_conditions.join(", "),
                evidence: Vec::new(),
                related_entities: vec![EntityId::Host(snapshot.host.hostname.clone())],
                suggested_actions: vec![
                    "Review Raspberry Pi firmware status in lens-hardware.".into(),
                ],
            });
        }
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

fn apply_view_filters(
    view: View,
    mut snapshot: SystemSnapshot,
    args: &ViewArgs,
) -> Result<SystemSnapshot> {
    let mode = args.r#match;
    let needle = args
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let matches = |text: &str| needle.is_none_or(|needle| mode.matches(text, needle));

    if let Some(name) = args.name.as_deref() {
        snapshot
            .services
            .retain(|item| mode.matches(&item.name, name));
    }
    if let Some(active) = args.active.as_deref() {
        snapshot
            .services
            .retain(|item| item.active.eq_ignore_ascii_case(active));
    }
    if let Some(enabled) = args.enabled {
        snapshot.services.retain(|item| {
            let loaded = item.load.eq_ignore_ascii_case("loaded");
            if enabled { loaded } else { !loaded }
        });
    }
    snapshot.services.retain(|item| {
        matches(&format!(
            "{} {} {} {}",
            item.name, item.active, item.sub, item.description
        ))
    });
    if let Some(service) = args.service.as_deref() {
        snapshot
            .services
            .retain(|item| mode.matches(&item.name, service));
        snapshot.logs.retain(|item| {
            item.unit
                .as_deref()
                .is_some_and(|unit| mode.matches(unit, service))
        });
    }
    if let Some(severity) = args.severity.as_deref() {
        snapshot.logs.retain(|item| {
            item.priority
                .as_deref()
                .is_some_and(|priority| priority.eq_ignore_ascii_case(severity))
        });
    }
    if let Some(process) = args.process.as_deref() {
        snapshot.logs.retain(|item| {
            mode.matches(
                &format!(
                    "{} {} {}",
                    item.source,
                    item.unit.as_deref().unwrap_or(""),
                    item.message
                ),
                process,
            )
        });
    }
    if let Some(unit) = args.unit.as_deref() {
        snapshot.logs.retain(|item| {
            item.unit
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(unit))
        });
    }
    if let Some(contains) = args.contains.as_deref() {
        snapshot
            .logs
            .retain(|item| mode.matches(&item.message, contains));
    }
    snapshot.logs.retain(|item| {
        matches(&format!(
            "{} {} {} {} {}",
            item.timestamp,
            item.source,
            item.unit.as_deref().unwrap_or(""),
            item.priority.as_deref().unwrap_or(""),
            item.message
        ))
    });
    if let Some(mount) = args.mount.as_deref() {
        snapshot
            .mounts
            .retain(|item| item.target.eq_ignore_ascii_case(mount));
    }
    if let Some(fstype) = args.fstype.as_deref() {
        snapshot
            .mounts
            .retain(|item| mode.matches(&item.filesystem, fstype));
        snapshot.block_devices.retain(|item| {
            item.filesystem
                .as_deref()
                .is_some_and(|value| mode.matches(value, fstype))
        });
    }
    if let Some(minimum) = args.min_used_percent {
        snapshot.mounts.retain(|item| item.used_percent >= minimum);
    }
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
    if let Some(interface) = args.interface.as_deref() {
        snapshot
            .interfaces
            .retain(|item| mode.matches(&item.name, interface));
        snapshot.routes.retain(|item| {
            item.interface
                .as_deref()
                .is_some_and(|value| mode.matches(value, interface))
        });
    }
    snapshot.interfaces.retain(|item| {
        matches(&format!(
            "{} {} {}",
            item.name,
            item.state,
            item.addresses.join(" ")
        ))
    });
    snapshot.routes.retain(|item| matches(&item.raw));
    if args.listening {
        snapshot.sockets.retain(|item| {
            item.state.eq_ignore_ascii_case("listen")
                || item.state.eq_ignore_ascii_case("listening")
        });
    }
    if let Some(port) = args.port {
        let port = port.to_string();
        snapshot.sockets.retain(|item| {
            item.local
                .rsplit_once(':')
                .is_some_and(|(_, local_port)| local_port == port)
                || item.local.ends_with(&format!(":{port}"))
        });
    }
    if let Some(proto) = args.proto.as_deref() {
        snapshot
            .sockets
            .retain(|item| item.protocol.eq_ignore_ascii_case(proto));
    }
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
        .temperatures
        .retain(|item| matches(&format!("{} {}", item.name, item.source)));
    if let Some(class) = args.class.as_deref() {
        snapshot
            .hardware_devices
            .retain(|item| mode.matches(&item.kind, class));
    }
    if let Some(serial) = args.serial.as_deref() {
        snapshot.hardware_devices.retain(|item| {
            item.serial_number
                .as_deref()
                .is_some_and(|value| mode.matches(value, serial))
        });
    }
    snapshot.hardware_devices.retain(|item| {
        matches(&format!(
            "{} {} {} {} {}",
            item.kind,
            item.name,
            item.path,
            item.manufacturer.as_deref().unwrap_or(""),
            item.serial_number.as_deref().unwrap_or("")
        ))
    });
    if let Some(section) = args.section.as_deref() {
        apply_system_section_filter(&mut snapshot, section)?;
    }
    snapshot.accounts.retain(|item| {
        matches(&format!(
            "{} {} {} {} {}",
            item.name, item.uid, item.gid, item.home, item.shell
        ))
    });
    snapshot.groups.retain(|item| {
        matches(&format!(
            "{} {} {}",
            item.name,
            item.gid,
            item.members.join(" ")
        ))
    });
    snapshot.certificates.retain(|item| {
        matches(&format!(
            "{} {} {} {}",
            item.path,
            item.subject.as_deref().unwrap_or(""),
            item.issuer.as_deref().unwrap_or(""),
            item.not_after.as_deref().unwrap_or("")
        ))
    });
    if let Some(min_severity) = args.min_severity.as_deref() {
        let minimum = parse_finding_severity(min_severity)?;
        snapshot.findings.retain(|item| item.severity >= minimum);
    }
    if let Some(id) = args.id.as_deref() {
        snapshot
            .findings
            .retain(|item| item.id.eq_ignore_ascii_case(id));
    }
    snapshot
        .findings
        .retain(|item| matches(&format!("{} {} {}", item.id, item.title, item.summary)));

    sort_snapshot_domain(view, &mut snapshot, args.sort);
    let limit = args.limit;
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
        snapshot.temperatures.truncate(limit);
        snapshot.hardware_devices.truncate(limit);
        snapshot.accounts.truncate(limit);
        snapshot.groups.truncate(limit);
        snapshot.certificates.truncate(limit);
        snapshot.findings.truncate(limit);
    }
    Ok(snapshot)
}

fn apply_system_section_filter(snapshot: &mut SystemSnapshot, section: &str) -> Result<()> {
    match section.to_ascii_lowercase().as_str() {
        "clock" | "ntp" | "clock/ntp" => {
            snapshot.dns = DnsContext::default();
            snapshot.accounts.clear();
            snapshot.groups.clear();
            snapshot.certificates.clear();
        }
        "dns" => {
            snapshot.clock = ClockContext::default();
            snapshot.accounts.clear();
            snapshot.groups.clear();
            snapshot.certificates.clear();
        }
        "users" => {
            snapshot.clock = ClockContext::default();
            snapshot.dns = DnsContext::default();
            snapshot.groups.clear();
            snapshot.certificates.clear();
        }
        "groups" => {
            snapshot.clock = ClockContext::default();
            snapshot.dns = DnsContext::default();
            snapshot.accounts.clear();
            snapshot.certificates.clear();
        }
        "certificates" | "certs" => {
            snapshot.clock = ClockContext::default();
            snapshot.dns = DnsContext::default();
            snapshot.accounts.clear();
            snapshot.groups.clear();
        }
        other => {
            return Err(usage_err(format!(
                "unknown --section '{other}'; use clock, dns, users, groups, or certificates"
            )));
        }
    }
    Ok(())
}

fn parse_finding_severity(value: &str) -> Result<lens_model::Severity> {
    match value.to_ascii_lowercase().as_str() {
        "information" | "info" => Ok(lens_model::Severity::Information),
        "attention" | "warning" | "warn" => Ok(lens_model::Severity::Attention),
        "critical" | "crit" => Ok(lens_model::Severity::Critical),
        other => Err(usage_err(format!(
            "unknown --min-severity '{other}'; use information, attention, or critical"
        ))),
    }
}

fn sort_snapshot_domain(view: View, snapshot: &mut SystemSnapshot, sort: Option<SpecialistSort>) {
    let Some(sort) = sort else {
        return;
    };
    match (view, sort) {
        (View::Services, SpecialistSort::Name) => {
            snapshot.services.sort_by(|left, right| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            });
        }
        (View::Services, SpecialistSort::Restarts) => {
            snapshot.services.sort_by(|left, right| {
                right
                    .restart_count
                    .unwrap_or(0)
                    .cmp(&left.restart_count.unwrap_or(0))
                    .then_with(|| left.name.cmp(&right.name))
            });
        }
        (View::Disk, SpecialistSort::UsedPercent) => {
            snapshot
                .mounts
                .sort_by(|left, right| right.used_percent.total_cmp(&left.used_percent));
        }
        (View::Net, SpecialistSort::Port) => {
            snapshot.sockets.sort_by(|left, right| {
                socket_port(&left.local)
                    .cmp(&socket_port(&right.local))
                    .then_with(|| left.local.cmp(&right.local))
            });
        }
        (View::Health, SpecialistSort::Severity) => {
            snapshot
                .findings
                .sort_by_key(|finding| std::cmp::Reverse(finding.severity));
        }
        _ => {}
    }
}

fn socket_port(local: &str) -> u32 {
    local
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(0)
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
                            sim.iccid
                                .as_deref()
                                .map_or_else(|| sim.path.clone(), mask_identifier),
                            sim.operator_name
                                .as_deref()
                                .map_or_else(String::new, |name| format!(" · {name}"))
                        )?;
                    }
                }
            }
        }
        View::Hardware => {
            writeln!(out, "HARDWARE")?;
            if let Some(value) = &snapshot.hardware.manufacturer {
                writeln!(out, "manufacturer: {value}")?;
            }
            if let Some(value) = &snapshot.hardware.model {
                writeln!(out, "model: {value}")?;
            }
            if let Some(value) = &snapshot.hardware.board {
                writeln!(out, "board: {value}")?;
            }
            if let Some(value) = &snapshot.hardware.board_revision {
                writeln!(out, "revision: {value}")?;
            }
            if let Some(value) = &snapshot.hardware.serial_number {
                writeln!(out, "serial: {value}")?;
            }
            if let Some(value) = &snapshot.hardware.firmware_version {
                writeln!(out, "firmware: {value}")?;
            }
            if let Some(pi) = &snapshot.hardware.raspberry_pi {
                writeln!(out, "\nRASPBERRY PI")?;
                writeln!(
                    out,
                    "active: {}",
                    if pi.throttled_raw.is_none() {
                        "unavailable".into()
                    } else if pi.active_conditions.is_empty() {
                        "none".into()
                    } else {
                        pi.active_conditions.join(", ")
                    }
                )?;
                writeln!(
                    out,
                    "recorded: {}",
                    if pi.throttled_raw.is_none() {
                        "unavailable".into()
                    } else if pi.historical_conditions.is_empty() {
                        "none".into()
                    } else {
                        pi.historical_conditions.join(", ")
                    }
                )?;
            }
            writeln!(out, "\nTEMPERATURES")?;
            for sensor in &snapshot.temperatures {
                writeln!(
                    out,
                    "{:<32} {:>6.1} °C  {}",
                    sensor.name, sensor.temperature_c, sensor.source
                )?;
            }
            writeln!(out, "\nDEVICES")?;
            for device in &snapshot.hardware_devices {
                writeln!(
                    out,
                    "{:<8} {:<32} {}",
                    device.kind, device.name, device.path
                )?;
            }
        }
        View::System => {
            writeln!(out, "CLOCK")?;
            writeln!(
                out,
                "timezone: {}",
                snapshot.clock.timezone.as_deref().unwrap_or("unavailable")
            )?;
            writeln!(
                out,
                "NTP synchronized: {}",
                snapshot
                    .clock
                    .ntp_synchronized
                    .map_or("unavailable", |value| if value { "yes" } else { "no" })
            )?;
            writeln!(
                out,
                "NTP service: {}",
                snapshot
                    .clock
                    .ntp_service
                    .as_deref()
                    .unwrap_or("unavailable")
            )?;
            writeln!(
                out,
                "\nDNS ({})",
                if snapshot.dns.source.is_empty() {
                    "unavailable"
                } else {
                    &snapshot.dns.source
                }
            )?;
            writeln!(
                out,
                "nameservers: {}",
                if snapshot.dns.nameservers.is_empty() {
                    "none".into()
                } else {
                    snapshot.dns.nameservers.join(", ")
                }
            )?;
            writeln!(
                out,
                "search domains: {}",
                if snapshot.dns.search_domains.is_empty() {
                    "none".into()
                } else {
                    snapshot.dns.search_domains.join(", ")
                }
            )?;
            writeln!(out, "\nACCOUNTS")?;
            let visible_accounts = interactive_accounts(snapshot).collect::<Vec<_>>();
            for item in &visible_accounts {
                writeln!(
                    out,
                    "{:<20} uid {:<7} gid {:<7} {:<24} {}",
                    item.name, item.uid, item.gid, item.home, item.shell
                )?;
            }
            writeln!(out, "\nGROUPS")?;
            for item in snapshot.groups.iter().filter(|group| {
                !group.members.is_empty()
                    || visible_accounts
                        .iter()
                        .any(|account| account.gid == group.gid)
            }) {
                writeln!(
                    out,
                    "{:<20} gid {:<7} {}",
                    item.name,
                    item.gid,
                    item.members.join(",")
                )?;
            }
            writeln!(out, "\nLOCAL CERTIFICATES")?;
            for item in &snapshot.certificates {
                writeln!(out, "{}", certificate_summary(item))?;
                writeln!(out, "  {}", item.path)?;
                if let Some(issuer) = &item.issuer {
                    writeln!(out, "  issuer: {issuer}")?;
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
        rx_bytes: Some(1_000_000),
        tx_bytes: Some(250_000),
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
    snapshot.clock = ClockContext {
        timezone: Some("Australia/Brisbane".into()),
        ntp_synchronized: Some(true),
        ntp_service: Some("enabled".into()),
    };
    snapshot.dns = DnsContext {
        source: "/etc/resolv.conf".into(),
        nameservers: vec!["192.0.2.53".into()],
        search_domains: vec!["device.example".into()],
    };
    snapshot.certificates = vec![CertificateInfo {
        path: "/etc/ssl/certs/device-ca.pem".into(),
        subject: None,
        issuer: None,
        not_after: None,
    }];
    snapshot.accounts = vec![AccountInfo {
        name: "mosquitto".into(),
        uid: 1883,
        gid: 1883,
        home: "/var/lib/mosquitto".into(),
        shell: "/usr/sbin/nologin".into(),
    }];
    snapshot.groups = vec![GroupInfo {
        name: "mosquitto".into(),
        gid: 1883,
        members: Vec::new(),
    }];
    snapshot.hardware = HardwareIdentity {
        manufacturer: Some("Raspberry Pi Foundation".into()),
        model: Some("Raspberry Pi 4 Model B Rev 1.5".into()),
        board: Some("BCM2711".into()),
        board_revision: Some("c03115".into()),
        serial_number: Some("10000000abcdef01".into()),
        firmware_version: Some("2026-07-22".into()),
        raspberry_pi: Some(lens_model::RaspberryPiStatus {
            throttled_raw: Some(0),
            active_conditions: Vec::new(),
            historical_conditions: Vec::new(),
        }),
    };
    snapshot.temperatures = vec![TemperatureSensor {
        name: "cpu-thermal".into(),
        source: "/sys/class/thermal/thermal_zone0".into(),
        temperature_c: 48.2,
        max_c: Some(80.0),
        critical_c: Some(85.0),
    }];
    snapshot.hardware_devices = vec![
        HardwareDevice {
            kind: "usb".into(),
            name: "Quectel LTE modem".into(),
            path: "/sys/bus/usb/devices/1-1".into(),
            manufacturer: Some("Quectel".into()),
            vendor_id: Some("2c7c".into()),
            product_id: Some("0125".into()),
            serial_number: Some("0123456789ABCDEF".into()),
        },
        HardwareDevice {
            kind: "serial".into(),
            name: "ttyUSB0".into(),
            path: "/dev/ttyUSB0".into(),
            manufacturer: None,
            vendor_id: None,
            product_id: None,
            serial_number: None,
        },
    ];
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
        let proc_route = parse_proc_route(0, "eth0 00000000 010011AC 0003 0 0 0 00000000 0 0 0")
            .expect("proc route");
        assert_eq!(proc_route.destination, "default");
        assert_eq!(proc_route.gateway.as_deref(), Some("172.17.0.1"));
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
    fn masks_cellular_identifiers_in_human_output() {
        assert_eq!(mask_identifier("8944500000000012345"), "••••2345");
        assert_eq!(mask_identifier("123"), "•••");
    }

    #[test]
    fn diagnoses_failed_and_weak_cellular_modems() {
        let mut failed = demo_snapshot();
        failed.cellular_modems[0].state = "failed".into();
        failed.findings = diagnose(&failed);
        assert!(
            failed
                .findings
                .iter()
                .any(|finding| finding.id.starts_with("cellular.failed."))
        );

        let mut weak = demo_snapshot();
        weak.cellular_modems[0].signal_quality_percent = Some(12);
        weak.findings = diagnose(&weak);
        assert!(
            weak.findings
                .iter()
                .any(|finding| finding.id.starts_with("cellular.weak-signal."))
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
    fn cockpit_does_not_report_failed_log_collection_as_zero_entries() {
        let mut snapshot = demo_snapshot();
        snapshot.logs.clear();
        snapshot
            .collection_warnings
            .push("/usr/bin/log timed out after 8.0s".into());
        assert_eq!(
            cockpit_view_summary(View::Logs, &snapshot, false),
            "log collection unavailable"
        );
    }

    #[test]
    fn cockpit_uses_a_fast_initial_log_window() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(cockpit_log_since(None), Some("1m"));
            assert_eq!(cockpit_log_since(Some("5m")), Some("5m"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(cockpit_log_since(None), None);
            assert_eq!(cockpit_log_since(Some("5m")), Some("5m"));
        }
    }

    #[test]
    fn cockpit_leads_with_host_status_while_details_load() {
        let snapshot = demo_snapshot();
        let cpu_activity = CpuActivity::default();
        let network_activity = NetworkActivity::default();
        let mut loading_output = Vec::new();
        render_cockpit(
            &snapshot,
            &cpu_activity,
            &network_activity,
            0,
            true,
            36,
            &mut loading_output,
        )
        .expect("loading cockpit");
        let loading_output = String::from_utf8(loading_output).expect("UTF-8");
        assert!(loading_output.contains("CPU"));
        assert!(loading_output.contains("Memory"));
        assert!(loading_output.contains("LIVE ACTIVITY"));
        assert!(loading_output.contains("logical CPU"));
        assert!(loading_output.contains("BUSIEST PROCESSES"));
        assert!(loading_output.contains("Checking services, logs, storage and network"));
        assert!(loading_output.contains("Processes    1 processes"));
        assert!(loading_output.contains("Services     checking"));

        let mut compact_output = Vec::new();
        render_cockpit(
            &snapshot,
            &cpu_activity,
            &network_activity,
            0,
            true,
            20,
            &mut compact_output,
        )
        .expect("compact cockpit");
        let compact_output = String::from_utf8(compact_output).expect("UTF-8");
        assert!(!compact_output.contains("BUSIEST PROCESSES"));
        assert!(compact_output.matches('\n').count() <= 20);

        let mut complete_output = Vec::new();
        render_cockpit(
            &snapshot,
            &cpu_activity,
            &network_activity,
            3,
            false,
            30,
            &mut complete_output,
        )
        .expect("complete cockpit");
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
    fn cockpit_summaries_do_not_present_collector_caps_as_inventory_totals() {
        let snapshot = demo_snapshot();
        let logs = cockpit_view_summary(View::Logs, &snapshot, false);
        let system = cockpit_view_summary(View::System, &snapshot, false);
        assert!(!logs.contains("recent entries"));
        assert!(logs.contains("errors"));
        assert!(!system.contains("certificates"));
        assert!(system.contains("DNS"));
    }

    #[test]
    fn system_view_exposes_sections_and_supports_direct_jumps() {
        let snapshot = demo_snapshot();
        let mut output = Vec::new();
        render_system_specialist(&snapshot, 0, false, 30, 124, &mut output).expect("system list");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("1 CLOCK/NTP"));
        assert!(output.contains("2 DNS"));
        assert!(output.contains("5 CERTIFICATES"));
        assert!(output.contains("Tab changes section"));

        let dns = system_section_start(&snapshot, SystemSection::Dns);
        let certificates = system_section_start(&snapshot, SystemSection::Certificates);
        assert_eq!(move_system_section(&snapshot, 0, 1), dns);
        assert_eq!(move_system_section(&snapshot, dns, 3), certificates);
        assert_eq!(
            move_system_section(&snapshot, certificates, 1),
            certificates
        );

        let mut certificate_output = Vec::new();
        render_system_specialist(
            &snapshot,
            certificates,
            false,
            24,
            80,
            &mut certificate_output,
        )
        .expect("certificate section");
        let certificate_output = String::from_utf8(certificate_output).expect("UTF-8");
        assert!(certificate_output.contains(">  CERTIFICATE"));
        assert!(certificate_output.contains("  GROUP"));

        let users = system_section_start(&snapshot, SystemSection::Users);
        let mut transition_output = Vec::new();
        render_system_specialist(&snapshot, users, false, 18, 80, &mut transition_output)
            .expect("system section transition");
        let transition_output = String::from_utf8(transition_output).expect("UTF-8");
        assert!(transition_output.contains(">  USER"));
        assert!(transition_output.contains("  DNS SEARCH"));
    }

    #[test]
    fn specialist_ancillary_options_are_explicit_and_validated() {
        let once = ViewArgs::try_parse_from(["lens-disk", "--once", "--limit", "0"]).expect("once");
        assert!(once.once);
        assert_eq!(once.limit, 0);
        assert!(validate_view_args(View::Disk, &once).is_ok());

        let invalid =
            ViewArgs::try_parse_from(["lens-disk", "--severity", "error"]).expect("shared parser");
        assert!(
            validate_view_args(View::Disk, &invalid)
                .expect_err("disk severity must fail")
                .to_string()
                .contains("lens-logs")
        );
        assert!(validate_view_args(View::Logs, &invalid).is_ok());

        let disk_help = view_command("lens-disk");
        assert!(
            disk_help
                .get_arguments()
                .find(|argument| argument.get_id() == "severity")
                .is_some_and(clap::Arg::is_hide_set)
        );
        let log_help = view_command("lens-logs");
        assert!(
            log_help
                .get_arguments()
                .find(|argument| argument.get_id() == "severity")
                .is_some_and(|argument| !argument.is_hide_set())
        );
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
            terminal.flush().expect("flush frame");
        }
        assert_eq!(
            output,
            b"\x1b[?2026hfirst\r\nsecond\r\nthird\r\n\x1b[?2026l"
        );
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
            rx_bytes: Some(0),
            tx_bytes: Some(0),
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
    fn health_reports_hot_sensors_and_active_pi_power_faults() {
        let mut snapshot = SystemSnapshot::empty("fixture");
        snapshot.temperatures.push(TemperatureSensor {
            name: "cpu".into(),
            source: "/sys/class/thermal/thermal_zone0".into(),
            temperature_c: 86.0,
            max_c: Some(80.0),
            critical_c: Some(85.0),
        });
        snapshot.hardware.raspberry_pi = Some(lens_model::RaspberryPiStatus {
            throttled_raw: Some(1),
            active_conditions: vec!["under-voltage".into()],
            historical_conditions: Vec::new(),
        });
        let findings = diagnose(&snapshot);
        assert!(findings.iter().any(|finding| {
            finding.id == "hardware.temperature.cpu" && finding.severity == Severity::Critical
        }));
        assert!(findings.iter().any(|finding| {
            finding.id == "hardware.raspberry-pi.active-throttling"
                && finding.severity == Severity::Critical
        }));
    }

    #[test]
    fn every_specialist_domain_has_list_and_detail_rendering() {
        let snapshot = demo_snapshot();
        let activity = NetworkActivity::default();
        for renderer in [View::Services, View::Disk, View::Net, View::Hardware] {
            let mut list = Vec::new();
            render_specialist(
                renderer, &snapshot, &activity, 0, false, false, "Ready", 30, &mut list,
            )
            .expect("list");
            let mut detail = Vec::new();
            render_specialist(
                renderer,
                &snapshot,
                &activity,
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
    fn specialist_frames_do_not_blank_the_terminal_before_drawing() {
        let snapshot = demo_snapshot();
        let mut output = Vec::new();
        render_specialist(
            View::Hardware,
            &snapshot,
            &NetworkActivity::default(),
            0,
            false,
            false,
            "Ready",
            30,
            &mut output,
        )
        .expect("hardware frame");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(!output.contains("\u{1b}[2J"));
        assert!(output.contains("\u{1b}[J"));
        assert!(output.matches("\u{1b}[2K").count() > 4);
    }

    #[test]
    fn anchored_footers_clear_rows_vacated_by_detail_views() {
        let snapshot = demo_snapshot();
        let mut specialist = Vec::new();
        render_specialist_content(
            View::Disk,
            &snapshot,
            &NetworkActivity::default(),
            0,
            true,
            false,
            "Ready",
            30,
            &mut specialist,
        )
        .expect("storage detail");
        let specialist = String::from_utf8(specialist).expect("UTF-8");
        assert!(specialist.contains("\u{1b}[J\u{1b}[28;1H"));

        let mut cockpit = Vec::new();
        render_cockpit_content(
            &snapshot,
            &CpuActivity::default(),
            &NetworkActivity::default(),
            0,
            false,
            30,
            &mut cockpit,
        )
        .expect("cockpit");
        let cockpit = String::from_utf8(cockpit).expect("UTF-8");
        assert!(cockpit.contains("\u{1b}[J\u{1b}[28;1H"));
    }

    #[test]
    fn specialist_lists_reflow_at_compact_width() {
        let snapshot = demo_snapshot();
        let mut output = Vec::new();
        render_service_specialist(&snapshot, 0, false, 16, 50, &mut output)
            .expect("compact services");
        render_log_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact logs");
        render_disk_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact storage");
        render_net_specialist(
            &snapshot,
            &NetworkActivity::default(),
            0,
            false,
            16,
            50,
            &mut output,
        )
        .expect("compact network");
        render_hardware_specialist(&snapshot, 0, false, 16, 50, &mut output)
            .expect("compact hardware");
        render_health_specialist(&snapshot, 0, false, 16, 50, &mut output).expect("compact health");
        assert!(!output.is_empty());
    }

    #[test]
    fn storage_detail_uses_two_columns_when_space_is_available() {
        let snapshot = demo_snapshot();
        let mut output = Vec::new();
        render_disk_specialist(&snapshot, 0, true, 20, 120, &mut output)
            .expect("wide storage detail");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(
            output
                .lines()
                .any(|line| line.contains("SOURCE") && line.contains("FILESYSTEM"))
        );
        assert!(
            output
                .lines()
                .any(|line| line.contains("USED") && line.contains("AVAILABLE"))
        );
        assert!(
            output
                .lines()
                .any(|line| line.contains("CAPACITY") && line.contains("INODES"))
        );
    }

    #[test]
    fn service_action_requires_a_separate_review_step() {
        let mut dialog = ServiceActionDialog {
            target: "nginx.service".into(),
            selection: 0,
            stage: ServiceActionStage::Choose,
            result: String::new(),
            receiver: None,
        };
        assert!(!dialog.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(dialog.stage, ServiceActionStage::Confirm);
        assert!(!dialog.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(dialog.stage, ServiceActionStage::Choose);
        assert_eq!(ServiceAction::ALL[0], ServiceAction::Restart);
    }

    #[test]
    fn network_activity_uses_counter_deltas_and_renders_charts() {
        let mut snapshot = demo_snapshot();
        let started = Instant::now();
        let mut activity = NetworkActivity::default();
        activity.observe(&snapshot.interfaces, started);
        snapshot.interfaces[0].rx_bytes = Some(1_002_048);
        snapshot.interfaces[0].tx_bytes = Some(251_024);
        activity.observe(&snapshot.interfaces, started + Duration::from_secs(1));
        assert_eq!(activity.interface_rate("eth0"), Some((2_048, 1_024)));
        assert_eq!(activity.current(), Some((2_048, 1_024)));

        let mut output = Vec::new();
        render_net_specialist(&snapshot, &activity, 0, false, 30, 120, &mut output)
            .expect("network activity");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("ACTIVITY"));
        assert!(output.contains("RX"));
        assert!(output.contains("TX"));
        assert!(output.contains('█'));
    }

    #[test]
    fn cockpit_cpu_activity_uses_aggregate_ticks_across_all_cores() {
        let mut first = demo_snapshot();
        first.host.cpu_count = 4;
        first.host.total_cpu_ticks = 1_000;
        first.host.idle_cpu_ticks = 800;
        first.processes[0].cpu_time_ticks = 10;
        let mut activity = CpuActivity::default();
        activity.observe(&mut first);

        let mut second = first.clone();
        second.host.total_cpu_ticks = 1_400;
        second.host.idle_cpu_ticks = 1_000;
        second.processes[0].cpu_time_ticks = 110;
        activity.observe(&mut second);

        assert_eq!(second.host.cpu_percent, 50.0);
        #[cfg(target_os = "linux")]
        assert_eq!(second.processes[0].cpu_percent, 100.0);
        assert_eq!(activity.history.back(), Some(&50));
    }

    #[test]
    fn cockpit_activity_renders_cpu_and_network_histories() {
        let snapshot = demo_snapshot();
        let cpu_activity = CpuActivity {
            history: VecDeque::from([0, 50, 100]),
            ..CpuActivity::default()
        };
        let started = Instant::now();
        let mut network_activity = NetworkActivity::default();
        network_activity.observe(&snapshot.interfaces, started);
        let mut later = snapshot.clone();
        later.interfaces[0].rx_bytes = later.interfaces[0].rx_bytes.map(|value| value + 2_048);
        later.interfaces[0].tx_bytes = later.interfaces[0].tx_bytes.map(|value| value + 1_024);
        network_activity.observe(&later.interfaces, started + Duration::from_secs(1));

        let mut output = Vec::new();
        render_cockpit_activity(
            &snapshot,
            &cpu_activity,
            &network_activity,
            166,
            30,
            &mut output,
        )
        .expect("cockpit activity");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("CPU PULSE · ● LIVE · 60s"));
        assert!(output.contains("NETWORK FLOW · ● LIVE · 60s"));
        assert!(output.contains("logical CPU"));
        assert!(output.contains("2.0KiB/s"));
        assert!(output.contains("1.0KiB/s"));
        assert!(output.contains("100 ┤"));
    }

    #[test]
    fn activity_charts_reserve_their_full_width_before_history_fills() {
        let snapshot = demo_snapshot();
        let short_cpu = CpuActivity {
            history: VecDeque::from([20]),
            ..CpuActivity::default()
        };
        let short_network = NetworkActivity {
            rx_history: VecDeque::from([1_024]),
            tx_history: VecDeque::from([512]),
            ..NetworkActivity::default()
        };
        let full_cpu = CpuActivity {
            history: (0..60).map(|value| value * 100 / 59).collect(),
            ..CpuActivity::default()
        };
        let full_network = NetworkActivity {
            rx_history: (1..=60).map(|value| value * 1_024).collect(),
            tx_history: (1..=60).map(|value| value * 512).collect(),
            ..NetworkActivity::default()
        };

        let mut short_output = Vec::new();
        render_cockpit_activity(
            &snapshot,
            &short_cpu,
            &short_network,
            166,
            30,
            &mut short_output,
        )
        .expect("short history");
        let mut full_output = Vec::new();
        render_cockpit_activity(
            &snapshot,
            &full_cpu,
            &full_network,
            166,
            30,
            &mut full_output,
        )
        .expect("full history");

        let widths = |output: Vec<u8>| {
            String::from_utf8(output)
                .expect("UTF-8")
                .lines()
                .map(|line| line.chars().count())
                .collect::<Vec<_>>()
        };
        assert_eq!(widths(short_output), widths(full_output));
        assert_eq!(fixed_chart_cell("▁", 32).chars().count(), 32);
        assert_eq!(fixed_chart_cell(&"█".repeat(32), 32).chars().count(), 32);
    }

    #[test]
    fn parses_macos_network_byte_counters() {
        let counters = parse_netstat_counters(
            "Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll\n\
             en0 1500 <Link#4> aa:bb:cc 12 0 4096 8 0 2048 0\n\
             en0 1500 192.0.2 192.0.2.8 12 - 4096 8 - 2048 -\n",
        );
        assert_eq!(counters, vec![("en0".into(), 4_096, 2_048)]);
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
        assert!(hardware_identity_present(&snapshot.hardware));
        assert!(!snapshot.temperatures.is_empty());
        assert!(!snapshot.hardware_devices.is_empty());
        assert!(!snapshot.findings.is_empty());
    }
}
