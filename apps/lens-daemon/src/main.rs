use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use lens_reporting::{
    buffer::Ring,
    ipc::{self, Writer},
    protocol::{MAX_REPORT_BYTES, Report},
    wait, wall_ms,
};
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Debug, Parser)]
#[command(about = "Opt-in, non-root Lens observation collector. No network or control interface.")]
struct Args {
    #[arg(long, default_value = "/run/lens-report/observations")]
    pipe: PathBuf,
    #[command(subcommand)]
    command: Option<Worker>,
}
#[derive(Debug, Subcommand)]
enum Worker {
    #[command(hide = true)]
    Snapshot {
        session: String,
        boot: String,
        sequence: u64,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    lens_reporting::require_unprivileged_linux()?;
    if let Some(Worker::Snapshot {
        session,
        boot,
        sequence,
    }) = args.command
    {
        let collected = wall_ms();
        let report = Report::from_snapshot(
            lens_system::collect_reporting_snapshot(),
            &session,
            &boot,
            sequence,
            collected,
        )?;
        std::io::stdout()
            .lock()
            .write_all(&lens_reporting::serialize_report(&report)?)?;
        return Ok(());
    }
    let parent = args.pipe.parent().context("pipe needs a parent")?;
    let _lock = ipc::singleton(&parent.join("collector.lock"))?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned();
    let session = lens_reporting::new_session();
    let stop = lens_reporting::stop_flag()?;
    let mut ring = Ring::default();
    let mut writer = Writer::default();
    let mut next = Instant::now() + lens_reporting::backoff::startup_delay();
    let mut sequence = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next {
            // Anchor to the start of the sample: collection time must not accumulate cadence drift.
            // Workers have a shorter deadline, so neither failures nor slow inventories add retries.
            next = Instant::now() + Duration::from_secs(60);
            sequence = sequence.saturating_add(1);
            match snapshot_worker(&session, &boot, sequence, &stop) {
                Ok(report) => ring.push(report, wall_ms(), Instant::now())?,
                Err(_) => eprintln!(
                    "Lens collection unavailable or exceeded its budget; waiting for next interval"
                ),
            }
        }
        ring.prune(wall_ms(), Instant::now());
        writer.flush(&args.pipe, &ring)?;
        wait(&stop, Duration::from_millis(100));
    }
    Ok(())
}

fn snapshot_worker(
    session: &str,
    boot: &str,
    sequence: u64,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Report> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(["snapshot", session, boot, &sequence.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let mut stdout = child.stdout.take().context("worker stdout missing")?;
    let flags = lens_reporting::nonblocking(&stdout);
    if let Err(error) = flags {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut bytes = Vec::new();
    let mut block = [0; 8192];
    let mut eof = false;
    let result = (|| -> Result<Report> {
        loop {
            match stdout.read(&mut block) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if bytes.len() + n > MAX_REPORT_BYTES {
                        bail!("worker output too large");
                    }
                    bytes.extend_from_slice(&block[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
            if eof {
                if let Some(status) = child.try_wait()? {
                    if !status.success() {
                        bail!("worker collection failed");
                    }
                    return lens_reporting::decode_report(&bytes, wall_ms());
                }
            }
            if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                bail!("worker collection timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        lens_reporting::kill_group(child.id());
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}
