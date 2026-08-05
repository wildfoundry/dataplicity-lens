use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use lens_core::{GroupMode, SortKey};
use serde::Serialize;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SortArg {
    Cpu,
    Memory,
    Pid,
    Name,
    User,
    Runtime,
    ReadRate,
    WriteRate,
    Threads,
}

impl From<SortArg> for SortKey {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Cpu => Self::Cpu,
            SortArg::Memory => Self::Memory,
            SortArg::Pid => Self::Pid,
            SortArg::Name => Self::Name,
            SortArg::User => Self::User,
            SortArg::Runtime => Self::Runtime,
            SortArg::ReadRate => Self::ReadRate,
            SortArg::WriteRate => Self::WriteRate,
            SortArg::Threads => Self::Threads,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GroupArg {
    None,
    Tree,
    User,
    Service,
}

impl From<GroupArg> for GroupMode {
    fn from(value: GroupArg) -> Self {
        match value {
            GroupArg::None => Self::None,
            GroupArg::Tree => Self::Tree,
            GroupArg::User => Self::User,
            GroupArg::Service => Self::Service,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ThemeArg {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalArg {
    Term,
    Kill,
    Hup,
    Int,
    Stop,
    Cont,
}

impl SignalArg {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
            Self::Hup => "HUP",
            Self::Int => "INT",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "lens-top",
    about = "A coherent, modern Linux and macOS process explorer",
    long_about = "Dataplicity Lens makes Linux and macOS process and system activity easier to understand. It provides an interactive terminal UI plus stable plain, JSON and JSON Lines output.",
    disable_version_flag = true
)]
pub struct Args {
    /// Write stable human-readable output and exit.
    #[arg(long, conflicts_with_all = ["json", "jsonl"])]
    pub plain: bool,

    /// Write one JSON document and exit.
    #[arg(long, conflicts_with_all = ["plain", "jsonl"])]
    pub json: bool,

    /// Write one JSON object per line and exit.
    #[arg(long, conflicts_with_all = ["plain", "json"])]
    pub jsonl: bool,

    /// Collect a snapshot once and exit instead of opening the TUI.
    #[arg(long)]
    pub once: bool,

    /// Refresh interval, for example 500ms, 1s or 2s.
    #[arg(long, value_name = "DURATION")]
    pub interval: Option<String>,

    /// Process sort key.
    #[arg(long, value_enum)]
    pub sort: Option<SortArg>,

    /// Process grouping mode.
    #[arg(long, value_enum)]
    pub group: Option<GroupArg>,

    /// Show processes owned by a matching user name or UID.
    #[arg(long, value_name = "USER")]
    pub filter_user: Option<String>,

    /// Show processes whose name contains this value.
    #[arg(long, value_name = "NAME")]
    pub filter_name: Option<String>,

    /// Show processes in a matching service or cgroup.
    #[arg(long, value_name = "SERVICE")]
    pub filter_service: Option<String>,

    /// Show processes in the given state, such as running, sleeping or zombie.
    #[arg(long, value_name = "STATE")]
    pub filter_state: Option<String>,

    /// Minimum process CPU percentage.
    #[arg(long, value_name = "PERCENT")]
    pub min_cpu: Option<f64>,

    /// Minimum process memory percentage.
    #[arg(long, value_name = "PERCENT")]
    pub min_memory: Option<f64>,

    /// Maximum number of displayed processes.
    #[arg(long, value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Send a named signal to one process.
    #[arg(long, value_enum, requires = "pid")]
    pub signal: Option<SignalArg>,

    /// Exact process ID targeted by --signal.
    #[arg(long, requires = "signal")]
    pub pid: Option<u32>,

    /// Expected process start time used to reject a recycled PID.
    #[arg(long, hide = true, requires = "signal")]
    pub expect_start_ticks: Option<u64>,

    /// Confirm a requested process signal for non-interactive use.
    #[arg(long, requires = "signal")]
    pub yes: bool,

    /// Print the planned process signal without sending it.
    #[arg(long, requires = "signal")]
    pub dry_run: bool,

    /// Disable colour even when the terminal supports it.
    #[arg(long)]
    pub no_color: bool,

    /// Choose colours for an auto-detected, dark or light terminal background.
    #[arg(long, value_enum)]
    pub theme: Option<ThemeArg>,

    /// Force ASCII rendering instead of Unicode line and graph characters.
    #[arg(long)]
    pub ascii: bool,

    /// Run against deterministic committed demo data.
    #[arg(long)]
    pub demo: bool,

    /// Load configuration from this path instead of the XDG default.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print the default configuration and exit.
    #[arg(long)]
    pub print_default_config: bool,

    /// Print version and build information.
    #[arg(long)]
    pub version: bool,

    #[arg(long, hide = true, value_name = "PATH")]
    pub generate_man: Option<PathBuf>,

    #[arg(long, hide = true, value_enum)]
    pub generate_completion: Option<CompletionShell>,

    #[arg(long, hide = true, value_name = "PATH")]
    pub generate_output: Option<PathBuf>,
}
