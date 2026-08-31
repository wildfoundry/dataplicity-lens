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
use cli::{Args, CompletionShell, SignalArg};
use config::EffectiveConfig;
use demo::DemoSource;
use lens_core::{
    AssertionError, AssertionPolicy, GroupMode, PrimaryDomain, ProcessFilter, SortDirection,
    SortKey, UsageError, exit_code_from_error, parse_fields_list, parse_state,
    project_snapshot_value, select_processes,
};
use lens_diagnostics::evaluate;
use lens_history::HistoryStore;
use lens_model::{BuildInfo, EntityId, Relationship, RelationshipKind, Snapshot};
use lens_output::{
    OutputFormat, PlainOptions, jsonl_record_types_for_fields, write_json_lines_filtered,
    write_json_value, write_snapshot,
};
#[cfg(target_os = "linux")]
use lens_platform_linux::LinuxCollector;
#[cfg(target_os = "macos")]
use lens_platform_macos::MacOsCollector;
use lens_ui::{ColorMode, UiOptions, run_tui};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

fn main() {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "lens-top: {error:#}");
        std::process::exit(exit_code_from_error(error.as_ref()));
    }
}

fn usage_err(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UsageError::new(message))
}

fn assertion_err(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(AssertionError::new(message))
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
    let policy = assertion_policy_from_args(&args);
    policy
        .validate()
        .map_err(|error| usage_err(error.message))?;
    if args.fields.is_some() && !args.json && !args.jsonl {
        return Err(usage_err("--fields requires --json or --jsonl"));
    }
    let effective = EffectiveConfig::resolve(&args)?;
    let filter = process_filter(&args)?;
    let build = build_info();
    let mut sampler = Sampler::new(
        args.demo,
        effective.interval,
        effective.history_length,
        build,
    );
    if args.signal.is_some() {
        return run_process_action(&args, &filter, &mut sampler);
    }

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
    let interactive = terminal
        && output_format.is_none()
        && !args.once
        && !args.quiet
        && args.fields.is_none()
        && !policy.is_active();

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
            theme_mode: effective.theme,
            glyphs: effective.glyphs,
        };
        run_tui(initial, move || sampler.collect(), options)?;
        return Ok(());
    }

    let _initial = sampler.collect()?;
    if !args.demo {
        // A requested interval is also the measurement window for a one-shot process sample.
        // Keep the implicit default fast so plain output remains useful in shell pipelines.
        let sampling_window = one_shot_sampling_window(args.interval.is_some(), effective.interval);
        thread::sleep(sampling_window);
    }
    let mut snapshot = sampler.collect()?;
    apply_noninteractive_query(
        &mut snapshot,
        &filter,
        effective.sort,
        effective.group,
        effective.limit,
    );
    emit_top_output(&args, &snapshot, effective.limit)?;
    match policy.evaluate(&snapshot, PrimaryDomain::Processes) {
        Ok(()) => Ok(()),
        Err(error) => Err(assertion_err(error.message)),
    }
}

fn assertion_policy_from_args(args: &Args) -> AssertionPolicy {
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

fn emit_top_output(args: &Args, snapshot: &Snapshot, limit: Option<usize>) -> Result<()> {
    if args.quiet {
        return Ok(());
    }
    let fields = match &args.fields {
        Some(raw) => Some(parse_fields_list(raw).map_err(|error| usage_err(error.message))?),
        None => None,
    };
    let result = if args.json {
        if let Some(fields) = fields {
            let value = project_snapshot_value(snapshot, &fields).context("project JSON fields")?;
            write_json_value(&mut io::stdout().lock(), &value)
        } else {
            write_snapshot(
                &mut io::stdout().lock(),
                snapshot,
                OutputFormat::Json,
                PlainOptions::default(),
            )
        }
    } else if args.jsonl {
        let record_types = if let Some(fields) = fields.as_ref() {
            jsonl_record_types_for_fields(fields)
        } else {
            vec!["host", "process", "finding"]
        };
        write_json_lines_filtered(&mut io::stdout().lock(), snapshot, &record_types)
    } else {
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120);
        write_snapshot(
            &mut io::stdout().lock(),
            snapshot,
            OutputFormat::Plain,
            PlainOptions { width, limit },
        )
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("write output"),
    }
}

fn one_shot_sampling_window(explicit_interval: bool, interval: Duration) -> Duration {
    if explicit_interval {
        interval
    } else {
        interval.min(Duration::from_millis(250))
    }
}

#[derive(Debug, Serialize)]
struct ProcessActionOutcome {
    signal: SignalArg,
    pid: u32,
    process: String,
    status: &'static str,
    verified: String,
}

fn run_process_action(args: &Args, filter: &ProcessFilter, sampler: &mut Sampler) -> Result<()> {
    let signal = args.signal.ok_or_else(|| usage_err("missing --signal"))?;
    if !args.dry_run && !args.yes {
        return Err(usage_err(
            "process signals require --yes; use --dry-run to inspect the plan safely",
        ));
    }
    if args.demo && !args.dry_run {
        return Err(usage_err("--demo only supports --dry-run process actions"));
    }
    let snapshot = sampler.collect()?;
    let mut candidates: Vec<_> = snapshot
        .processes
        .iter()
        .filter(|process| filter.matches(process))
        .collect();
    if let Some(pid) = args.pid {
        candidates.retain(|process| process.pid.0 == pid);
    }
    let target = match candidates.as_slice() {
        [process] => *process,
        [] => {
            return Err(usage_err(
                "process action selector matched no processes; refuse to act",
            ));
        }
        _ => {
            return Err(usage_err(format!(
                "process action selector matched {} processes; refuse to act without a unique target",
                candidates.len()
            )));
        }
    };
    let pid = target.pid.0;
    if matches!(pid, 0 | 1) || pid == std::process::id() {
        bail!("refusing to signal PID {pid}");
    }
    let identity = target.identity();
    let process = target.name.clone();
    if args
        .expect_start_ticks
        .is_some_and(|expected| expected != identity.1)
    {
        bail!("PID {pid} changed or exited before confirmation; no signal was sent");
    }
    if let Some(expected_name) = args.expect_name.as_deref()
        && !process.eq_ignore_ascii_case(expected_name)
    {
        return Err(usage_err(format!(
            "PID {pid} name is '{process}', expected '{expected_name}'; no signal was sent"
        )));
    }
    if args.dry_run {
        return write_process_action(
            args,
            &ProcessActionOutcome {
                signal,
                pid,
                process,
                status: "planned",
                verified: "not executed".into(),
            },
        );
    }
    let current = sampler.collect()?;
    let current_match = current
        .processes
        .iter()
        .find(|candidate| candidate.identity() == identity);
    let Some(current_match) = current_match else {
        bail!("PID {pid} changed or exited before confirmation; no signal was sent");
    };
    if let Some(expected_name) = args.expect_name.as_deref()
        && !current_match.name.eq_ignore_ascii_case(expected_name)
    {
        return Err(usage_err(format!(
            "PID {pid} name changed before confirmation; no signal was sent"
        )));
    }
    let status = std::process::Command::new("/bin/kill")
        .args([format!("-{}", signal.name()), pid.to_string()])
        .status()
        .context("send process signal")?;
    if !status.success() {
        bail!(
            "the operating system rejected {} for PID {pid}",
            signal.name()
        );
    }
    thread::sleep(Duration::from_millis(50));
    let refreshed = sampler.collect()?;
    let verified = refreshed
        .processes
        .iter()
        .find(|candidate| candidate.pid.0 == pid && candidate.start_time_ticks == identity.1);
    write_process_action(
        args,
        &ProcessActionOutcome {
            signal,
            pid,
            process,
            status: "completed",
            verified: verified.map_or_else(
                || "process exited".into(),
                |process| format!("process remains {}", process.state.label()),
            ),
        },
    )
}

fn write_process_action(args: &Args, outcome: &ProcessActionOutcome) -> Result<()> {
    if args.quiet {
        return Ok(());
    }
    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), outcome)?;
        println!();
    } else {
        println!(
            "{} PID {} ({}): {}",
            outcome.signal.name(),
            outcome.pid,
            outcome.process,
            outcome.status
        );
        println!("Verification: {}", outcome.verified);
    }
    Ok(())
}

#[derive(Debug)]
enum Source {
    #[cfg(target_os = "linux")]
    Linux(LinuxCollector),
    #[cfg(target_os = "macos")]
    MacOs(MacOsCollector),
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
            #[cfg(target_os = "linux")]
            {
                let mut collector = LinuxCollector::default();
                collector.set_refresh_interval(interval);
                Source::Linux(collector)
            }
            #[cfg(target_os = "macos")]
            {
                let mut collector = MacOsCollector::default();
                collector.set_refresh_interval(interval);
                Source::MacOs(collector)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            compile_error!("lens-top currently supports Linux and macOS");
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
            #[cfg(target_os = "linux")]
            Source::Linux(collector) => collector.collect().context("collect Linux snapshot")?,
            #[cfg(target_os = "macos")]
            Source::MacOs(collector) => collector.collect().context("collect macOS snapshot")?,
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
        Some(value) => Some(parse_state(value).ok_or_else(|| {
            usage_err(format!(
                "invalid process state {value:?}; use running, sleeping, disk-sleep, stopped, zombie, idle or dead"
            ))
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
        exact_name: args.exact_name.clone(),
        service_or_cgroup: args.filter_service.clone(),
        cgroup: args.cgroup.clone(),
        pid: args.pid.filter(|_| args.signal.is_none()),
        ppid: args.ppid,
        match_mode: args.r#match,
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
    let mut file =
        fs::File::create(path).with_context(|| format!("create man page {}", path.display()))?;
    clap_mangen::Man::new(Args::command())
        .render(&mut file)
        .context("render man page")
}

fn generate_completion(shell: CompletionShell, path: &Path) -> Result<()> {
    ensure_parent(path)?;
    let mut file =
        fs::File::create(path).with_context(|| format!("create completion {}", path.display()))?;
    let mut command = Args::command();
    match shell {
        CompletionShell::Bash => {
            clap_complete::generate(
                clap_complete::shells::Bash,
                &mut command,
                "lens-top",
                &mut file,
            );
        }
        CompletionShell::Zsh => {
            clap_complete::generate(
                clap_complete::shells::Zsh,
                &mut command,
                "lens-top",
                &mut file,
            );
        }
        CompletionShell::Fish => {
            clap_complete::generate(
                clap_complete::shells::Fish,
                &mut command,
                "lens-top",
                &mut file,
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_one_shot_interval_is_honoured() {
        assert_eq!(
            one_shot_sampling_window(true, Duration::from_secs(2)),
            Duration::from_secs(2)
        );
        assert_eq!(
            one_shot_sampling_window(false, Duration::from_secs(2)),
            Duration::from_millis(250)
        );
    }
}
