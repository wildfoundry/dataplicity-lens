#![forbid(unsafe_code)]

mod cli;
mod config;
mod demo;

use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use cli::{Args, CompletionShell};
use config::EffectiveConfig;
use demo::DemoSource;
use lens_core::{
    GroupMode, ProcessFilter, SortDirection, SortKey, parse_state, select_processes,
};
use lens_diagnostics::evaluate;
use lens_history::HistoryStore;
use lens_model::{BuildInfo, EntityId, Relationship, RelationshipKind, Snapshot};
use lens_output::{OutputFormat, PlainOptions, write_snapshot};
use lens_platform_linux::LinuxCollector;
use lens_ui::{ColorMode, UiOptions, run_tui};
use tracing_subscriber::EnvFilter;

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "lens-top: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    if args.version {
        print_version();
        return Ok(());
    }
    if args.print_default_config {
        print!("{}", config::default_config_text()?);
        return Ok(());
    }
    if let Some(path) = &args.generate_man {
        generate_man(path)?;
        return Ok(());
    }
    if let Some(shell) = args.generate_completion {
        let output = args
            .generate_output
            .as_deref()
            .context("--generate-completion requires --generate-output")?;
        generate_completion(shell, output)?;
        return Ok(());
    }

    validate_threshold("--min-cpu", args.min_cpu)?;
    validate_threshold("--min-memory", args.min_memory)?;
    let effective = EffectiveConfig::resolve(&args)?;
    let filter = process_filter(&args)?;
    let build = build_info();
    let mut sampler = Sampler::new(args.demo, effective.interval, effective.history_length, build);

    let output_format = if args.json {
        Some(OutputFormat::Json)
    } else if args.jsonl {
        Some(OutputFormat::JsonLines)
    } else if args.plain {
        Some(OutputFormat::Plain)
    } else {
        None
    };
    let terminal = io::stdout().is_terminal();
    let interactive = terminal && output_format.is_none() && !args.once;

    if interactive {
        let initial = sampler.collect()?;
        let options = UiOptions {
            interval: effective.interval,
            sort_key: effective.sort,
            sort_direction: SortDirection::Descending,
            group: effective.group,
            filter,
            limit: effective.limit,
            history_length: effective.history_length,
            color_mode: if effective.no_color {
                ColorMode::Never
            } else {
                ColorMode::Auto
            },
            ascii: effective.ascii,
        };
        run_tui(initial, move || sampler.collect(), options)?;
        return Ok(());
    }

    let mut snapshot = sampler.collect()?;
    if args.demo {
        snapshot = sampler.collect()?;
    } else {
        thread::sleep(effective.interval.min(Duration::from_millis(250)));
        snapshot = sampler.collect()?;
    }
    apply_noninteractive_query(
        &mut snapshot,
        &filter,
        effective.sort,
        effective.group,
        effective.limit,
    );
    let format = output_format.unwrap_or(OutputFormat::Plain);
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);
    let result = write_snapshot(
        &mut io::stdout().lock(),
        &snapshot,
        format,
        PlainOptions {
            width,
            limit: effective.limit,
        },
    );
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("write output"),
    }
}

#[derive(Debug)]
enum Source {
    Linux(LinuxCollector),
    Demo(DemoSource),
}

#[derive(Debug)]
struct Sampler {
    source: Source,
    history: HistoryStore,
    interval: Duration,
    previous_at: Option<Instant>,
    build: BuildInfo,
}

impl Sampler {
    fn new(demo: bool, interval: Duration, history_length: usize, build: BuildInfo) -> Self {
        let source = if demo {
            Source::Demo(DemoSource::default())
        } else {
            let mut collector = LinuxCollector::default();
            collector.set_refresh_interval(interval);
            Source::Linux(collector)
        };
        Self {
            source,
            history: HistoryStore::new(history_length),
            interval,
            previous_at: None,
            build,
        }
    }

    fn collect(&mut self) -> Result<Snapshot> {
        let now = Instant::now();
        let elapsed = self
            .previous_at
            .map_or(self.interval.as_secs_f64(), |previous| {
                now.duration_since(previous).as_secs_f64()
            });
        let mut snapshot = match &mut self.source {
            Source::Linux(collector) => collector.collect().context("collect Linux snapshot")?,
            Source::Demo(source) => source.collect(self.interval.as_millis() as u64),
        };
        self.history.apply(&mut snapshot, elapsed);
        snapshot.findings = evaluate(&snapshot, &self.history);
        attach_finding_relationships(&mut snapshot);
        snapshot.build = Some(self.build.clone());
        self.previous_at = Some(now);
        Ok(snapshot)
    }
}

fn attach_finding_relationships(snapshot: &mut Snapshot) {
    for finding in &snapshot.findings {
        for entity in &finding.related_entities {
            snapshot.relationships.push(Relationship {
                from: EntityId::Finding(finding.id.clone()),
                to: entity.clone(),
                kind: match entity {
                    EntityId::Host(_) => RelationshipKind::FindingOnHost,
                    _ => RelationshipKind::FindingConcerns,
                },
            });
        }
    }
}

fn process_filter(args: &Args) -> Result<ProcessFilter> {
    let state = match args.filter_state.as_deref() {
        Some(value) => Some(parse_state(value).with_context(|| {
            format!("invalid process state {value:?}; use running, sleeping, disk-sleep, stopped, zombie, idle or dead")
        })?),
        None => None,
    };
    Ok(ProcessFilter {
        search: None,
        user: args.filter_user.clone(),
        state,
        min_cpu: args.min_cpu,
        min_memory: args.min_memory,
        name: args.filter_name.clone(),
        service_or_cgroup: args.filter_service.clone(),
    })
}

fn apply_noninteractive_query(
    snapshot: &mut Snapshot,
    filter: &ProcessFilter,
    sort: SortKey,
    group: GroupMode,
    limit: Option<usize>,
) {
    let selected = select_processes(
        &snapshot.processes,
        filter,
        sort,
        SortDirection::Descending,
        group,
        limit,
    );
    snapshot.processes = selected
        .into_iter()
        .filter_map(|item| snapshot.processes.get(item.index).cloned())
        .collect();
}

fn validate_threshold(name: &str, value: Option<f64>) -> Result<()> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        bail!("{name} must be a non-negative finite number");
    }
    Ok(())
}

fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        commit: env!("LENS_GIT_SHA").to_owned(),
        target: env!("LENS_TARGET").to_owned(),
        built_by: env!("LENS_BUILT_BY").to_owned(),
    }
}

fn print_version() {
    let build = build_info();
    println!("lens-top {}", build.version);
    println!("commit: {}", build.commit);
    println!("target: {}", build.target);
    println!("built-by: {}", build.built_by);
}

fn generate_man(path: &Path) -> Result<()> {
    ensure_parent(path)?;
    let mut file = fs::File::create(path)
        .with_context(|| format!("create man page {}", path.display()))?;
    clap_mangen::Man::new(Args::command())
        .render(&mut file)
        .context("render man page")
}

fn generate_completion(shell: CompletionShell, path: &Path) -> Result<()> {
    ensure_parent(path)?;
    let mut file = fs::File::create(path)
        .with_context(|| format!("create completion {}", path.display()))?;
    let mut command = Args::command();
    match shell {
        CompletionShell::Bash => {
            clap_complete::generate(clap_complete::shells::Bash, &mut command, "lens-top", &mut file);
        }
        CompletionShell::Zsh => {
            clap_complete::generate(clap_complete::shells::Zsh, &mut command, "lens-top", &mut file);
        }
        CompletionShell::Fish => {
            clap_complete::generate(clap_complete::shells::Fish, &mut command, "lens-top", &mut file);
        }
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    Ok(())
}

fn init_tracing() {
    if std::env::var_os("RUST_LOG").is_none() {
        return;
    }
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}
