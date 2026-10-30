#![forbid(unsafe_code)]

pub mod backoff;
pub mod buffer;
pub mod ipc;
pub mod protocol;
pub mod publisher;

use anyhow::{Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub fn require_unprivileged_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("Lens reporting services require Linux");
    }
    if rustix::process::geteuid().is_root() {
        bail!("run this service as its dedicated non-root account");
    }
    Ok(())
}

pub fn stop_flag() -> Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, flag.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())?;
    Ok(flag)
}

pub fn wait(stop: &AtomicBool, duration: Duration) {
    let until = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < until {
        std::thread::sleep(
            until
                .saturating_duration_since(std::time::Instant::now())
                .min(Duration::from_millis(200)),
        );
    }
}

pub fn new_session() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn serialize_report(report: &protocol::Report) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(report)?)
}
pub fn decode_report(bytes: &[u8], now: u64) -> Result<protocol::Report> {
    if bytes.len() > protocol::MAX_REPORT_BYTES {
        bail!("report exceeds limit");
    }
    let report: protocol::Report = serde_json::from_slice(bytes)?;
    report.validate(now)?;
    Ok(report)
}
pub fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}
pub fn kill_group(id: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(id as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}
