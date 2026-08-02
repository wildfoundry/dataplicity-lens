use std::{
    io::{self, IsTerminal},
    time::Duration,
};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use lens_core::{select_processes, SnapshotSource, ViewOptions};
use lens_model::SortKey;
use lens_output::{write_snapshot, OutputFormat};
use lens_platform_linux::LinuxSource;

#[derive(Debug, Parser)]
#[command(
    name = "lens-top",
    version,
    about = "A fast, humane Linux process and system monitor"
)]
struct Cli {
    /// Render one snapshot and exit.
    #[arg(long)]
    once: bool,

    /// Output surface. TUI falls back to table when stdout is not a terminal.
    #[arg(long, value_enum, default_value_t = FormatArg::Tui)]
    format: FormatArg,

    /// Process sort field.
    #[arg(long, value_enum, default_value_t = SortArg::Cpu)]
    sort: SortArg,

    /// Sort from low to high instead of high to low.
    #[arg(long)]
    ascending: bool,

    /// Keep processes whose PID, name, or command contains this text.
    #[arg(long)]
    filter: Option<String>,

    /// Maximum number of processes to display.
    #[arg(long, default_value_t = 100)]
    limit: usize,

    /// Refresh interval in seconds for the interactive display.
    #[arg(long, default_value_t = 2.0)]
    interval: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FormatArg {
    Tui,
    Table,
    Json,
    Ndjson,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SortArg {
    Cpu,
    Memory,
    Pid,
    Name,
    Runtime,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let options = ViewOptions {
        sort_key: cli.sort.into(),
        descending: !cli.ascending,
        filter: cli.filter,
        limit: cli.limit.max(1),
    };
    let interval = if cli.interval.is_finite() {
        cli.interval.max(0.1)
    } else {
        2.0
    };
    let refresh_interval = Duration::from_secs_f64(interval);
    let stdout_is_terminal = io::stdout().is_terminal();

    if cli.format == FormatArg::Tui && stdout_is_terminal && !cli.once {
        return lens_ui::run(LinuxSource::new(), options, refresh_interval);
    }

    let mut source = LinuxSource::new();
    let snapshot = source.refresh()?;
    let processes = select_processes(&snapshot, &options);
    let format = match cli.format {
        FormatArg::Tui | FormatArg::Table => OutputFormat::Table,
        FormatArg::Json => OutputFormat::Json,
        FormatArg::Ndjson => OutputFormat::Ndjson,
    };
    write_snapshot(io::stdout().lock(), &snapshot, &processes, format)?;
    Ok(())
}

impl From<SortArg> for SortKey {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Cpu => Self::Cpu,
            SortArg::Memory => Self::Memory,
            SortArg::Pid => Self::Pid,
            SortArg::Name => Self::Name,
            SortArg::Runtime => Self::Runtime,
        }
    }
}
