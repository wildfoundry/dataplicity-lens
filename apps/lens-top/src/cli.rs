use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use lens_core::{FailOnSeverity, GroupMode, MatchMode, SortKey};
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

    /// Suppress stdout on success (errors still go to stderr).
    #[arg(long)]
    pub quiet: bool,

    /// Project JSON/JSONL to these top-level snapshot fields (comma-separated).
    #[arg(long, value_name = "LIST")]
    pub fields: Option<String>,

    /// Sample briefly, print one snapshot and exit instead of opening the TUI.
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

    /// How name/user/service filters bind (default: contains).
    #[arg(long, value_enum, default_value_t = MatchMode::Contains)]
    pub r#match: MatchMode,

    /// Show processes owned by a matching user name or UID.
    #[arg(long, value_name = "USER")]
    pub filter_user: Option<String>,

    /// Show processes whose name matches this value.
    #[arg(long, value_name = "NAME")]
    pub filter_name: Option<String>,

    /// Show processes whose name equals this value exactly.
    #[arg(long, value_name = "NAME")]
    pub exact_name: Option<String>,

    /// Show processes in a matching service or cgroup.
    #[arg(long, value_name = "SERVICE")]
    pub filter_service: Option<String>,

    /// Show processes in a matching cgroup path.
    #[arg(long, value_name = "CGROUP")]
    pub cgroup: Option<String>,

    /// Show processes in the given state, such as running, sleeping or zombie.
    #[arg(long, value_name = "STATE")]
    pub filter_state: Option<String>,

    /// Filter to one process ID (also used with --signal).
    #[arg(long, value_name = "PID")]
    pub pid: Option<u32>,

    /// Filter to children of this parent PID.
    #[arg(long, value_name = "PID")]
    pub ppid: Option<u32>,

    /// Minimum process CPU percentage.
    #[arg(long, value_name = "PERCENT")]
    pub min_cpu: Option<f64>,

    /// Minimum process memory percentage.
    #[arg(long, value_name = "PERCENT")]
    pub min_memory: Option<f64>,

    /// Maximum number of displayed processes.
    #[arg(long, value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Exit 3 when the filtered process set is empty.
    #[arg(long)]
    pub fail_if_empty: bool,

    /// Exit 3 when any filtered processes remain.
    #[arg(long)]
    pub fail_if_any: bool,

    /// Exit 3 unless the filtered process count equals N.
    #[arg(long, value_name = "N")]
    pub expect_count: Option<usize>,

    /// Exit 3 unless the filtered process count is at least N.
    #[arg(long, value_name = "N")]
    pub expect_count_min: Option<usize>,

    /// Exit 3 unless the filtered process count is at most N.
    #[arg(long, value_name = "N")]
    pub expect_count_max: Option<usize>,

    /// Exit 3 when findings reach this severity (warning or critical).
    #[arg(long, value_enum)]
    pub fail_on: Option<FailOnSeverity>,

    /// Exit 3 when collection_warnings is non-empty.
    #[arg(long)]
    pub fail_on_collection_warnings: bool,

    /// Send a named signal to one process.
    #[arg(long, value_enum)]
    pub signal: Option<SignalArg>,

    /// Expected process start time used to reject a recycled PID.
    #[arg(long, value_name = "TICKS", requires = "signal")]
    pub expect_start_ticks: Option<u64>,

    /// Expected process name re-checked immediately before signalling.
    #[arg(long, value_name = "NAME", requires = "signal")]
    pub expect_name: Option<String>,

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
    #[arg(long, hide = true)]
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
