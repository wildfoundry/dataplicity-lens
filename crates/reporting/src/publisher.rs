use crate::{
    backoff::{Gate, INTERVAL_HEADER, Outcome, startup_delay},
    buffer::{Ring, encode_batch},
    ipc, wait, wall_ms,
};
use anyhow::{Context, Result, bail};
use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
struct State {
    server_url: String,
    registration_id: String,
    device_hash: Option<String>,
    device_secret: Option<String>,
    gate: Gate,
}
impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("credentials", &"[redacted]")
            .field("gate", &self.gate)
            .finish()
    }
}
fn private_dir(path: &Path) -> Result<()> {
    let meta = path.symlink_metadata()?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        bail!("publisher state directory must be owned by this account with mode 0700");
    }
    Ok(())
}
fn read_state(dir: &Path) -> Result<State> {
    private_dir(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(dir.join("state.json"))?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.len() > 16384
    {
        bail!("invalid or insecure publisher state file");
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    let state: State = serde_json::from_slice(&bytes)
        .context("publisher state is corrupt; refusing network access")?;
    validate_server(&state.server_url)?;
    uuid::Uuid::parse_str(&state.registration_id)?;
    Ok(state)
}
fn write_state(dir: &Path, state: &State) -> Result<()> {
    private_dir(dir)?;
    let path = dir.join("state.next");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(&path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec(state)?)?;
    file.sync_all()?;
    fs::rename(path, dir.join("state.json"))?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}
fn validate_server(base: &str) -> Result<()> {
    let uri: ureq::http::Uri = base.parse().context("invalid server URL")?;
    if uri.scheme_str() != Some("https")
        || uri.host().is_none()
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        || uri.query().is_some()
        || !["", "/"].contains(&uri.path())
    {
        bail!("server URL must be an HTTPS origin, without credentials, path or query");
    }
    Ok(())
}
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .max_response_header_size(16384)
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .build()
        .into()
}
fn deadline(gate: &Gate) -> Instant {
    Instant::now()
        .checked_add(remaining(gate))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(100 * 365 * 86400))
}
fn remaining(gate: &Gate) -> Duration {
    Duration::from_millis(gate.not_before_ms.saturating_sub(wall_ms()))
}

/// Explicit one-attempt registration. The organisation code is never written to state.
pub fn register(dir: &Path, base: &str, name: &str, code: &str, refresh: bool) -> Result<()> {
    private_dir(dir)?;
    validate_server(base)?;
    let _lock = ipc::singleton(&dir.join("publisher.lock"))?;
    let state_path = dir.join("state.json");
    let mut state = if state_path.symlink_metadata().is_ok() {
        read_state(dir)?
    } else {
        State {
            server_url: base.trim_end_matches('/').into(),
            registration_id: uuid::Uuid::new_v4().to_string(),
            device_hash: None,
            device_secret: None,
            gate: Gate::default(),
        }
    };
    if state.device_secret.is_some() && !refresh {
        bail!("already registered; existing credentials retained");
    }
    if state.server_url != base.trim_end_matches('/') {
        bail!("pending registration belongs to another origin");
    }
    if name.is_empty() || name.len() > 255 || code.trim().is_empty() || code.len() > 4096 {
        bail!("invalid registration input");
    }
    if state.gate.not_before_ms > wall_ms() {
        bail!(
            "registration cooldown active; retry in {} seconds",
            remaining(&state.gate).as_secs()
        );
    }
    state.gate.halted = false;
    // Store enrolment identity and reserve a slot BEFORE making a potentially ambiguous request.
    state.gate.reserve_attempt(wall_ms());
    write_state(dir, &state)?;
    let body = serde_json::to_vec(
        &serde_json::json!({"provisioning_key":code.trim(),"name":name,"registration_id":state.registration_id,"expected_device_hash":state.device_hash}),
    )?;
    let result = agent()
        .post(format!(
            "{}/device-gateway/lens/provision/",
            state.server_url
        ))
        .header("Content-Type", "application/json")
        .send(&body);
    let mut response = match result {
        Ok(response) => response,
        Err(_) => {
            state.gate.failure(wall_ms(), None);
            write_state(dir, &state)?;
            bail!(
                "registration transport failed; credentials and code were not logged; retry only after cooldown"
            );
        }
    };
    let status = response.status().as_u16();
    let outcome = state.gate.status(
        status,
        response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok()),
        response.headers().get("Date").and_then(|v| v.to_str().ok()),
        wall_ms(),
    );
    write_state(dir, &state)?;
    if outcome != Outcome::Accepted {
        bail!("registration rejected (HTTP {status}); cooldown recorded, no automatic retry");
    }
    let response_body = response
        .body_mut()
        .with_config()
        .limit(16384)
        .read_to_vec()?;
    let json: serde_json::Value = serde_json::from_slice(&response_body)?;
    if json.get("registration_id").and_then(|v| v.as_str()) != Some(state.registration_id.as_str())
    {
        state.gate.halted = true;
        write_state(dir, &state)?;
        bail!(
            "server does not confirm idempotent Lens registration; upgrade the backend before retrying"
        );
    }
    let hash = json
        .get("hash_id")
        .and_then(|v| v.as_str())
        .context("registration omitted host identity")?;
    let secret = json
        .get("device_secret")
        .and_then(|v| v.as_str())
        .context("registration omitted credentials")?;
    if hash.len() != 64
        || !hash.bytes().all(|c| c.is_ascii_hexdigit())
        || secret.len() < 32
        || secret.len() > 512
        || !secret
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        bail!("invalid registration response");
    }
    if state
        .device_hash
        .as_deref()
        .is_some_and(|expected| expected != hash)
    {
        state.gate.halted = true;
        write_state(dir, &state)?;
        bail!("registration changed existing host identity; refusing replacement");
    }
    state.device_hash = Some(hash.into());
    state.device_secret = Some(secret.into());
    write_state(dir, &state)?;
    println!("Lens registered. Reporting services remain stopped until explicitly enabled.");
    Ok(())
}

fn send_upload(
    agent: &ureq::Agent,
    url: &str,
    secret: &str,
    body: &[u8],
    gate: &mut Gate,
) -> Outcome {
    let response = agent
        .post(url)
        .header("Authorization", format!("ApiKey {secret}"))
        .header("Content-Type", "application/json")
        .send(body);
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            gate.observe_interval(
                response
                    .headers()
                    .get(INTERVAL_HEADER)
                    .and_then(|v| v.to_str().ok()),
            );
            let outcome = gate.status(
                status,
                response
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok()),
                response.headers().get("Date").and_then(|v| v.to_str().ok()),
                wall_ms(),
            );
            if outcome != Outcome::Accepted {
                eprintln!("Lens upload HTTP {status}; policy {outcome:?}");
            }
            outcome
        }
        Err(_) => {
            gate.failure(wall_ms(), None);
            eprintln!("Lens transport failure; backing off");
            Outcome::Retry
        }
    }
}

pub fn run(dir: PathBuf, pipe: PathBuf) -> Result<()> {
    private_dir(&dir)?;
    let _lock = ipc::singleton(&dir.join("publisher.lock"))?;
    let mut state = read_state(&dir)?;
    let hash = state
        .device_hash
        .as_deref()
        .context("register Lens before starting the publisher")?
        .to_owned();
    let secret = state
        .device_secret
        .as_deref()
        .context("registration credentials missing")?
        .to_owned();
    let stop = crate::stop_flag()?;
    let queue = Arc::new(Mutex::new(Ring::default()));
    let reader_stop = stop.clone();
    let reader_queue = queue.clone();
    let reader = std::thread::spawn(move || -> Result<()> {
        let mut reader = ipc::Reader::open(&pipe)?;
        while !reader_stop.load(Ordering::Relaxed) {
            let now = wall_ms();
            let reports = reader.read(now)?;
            let mut queue = reader_queue
                .lock()
                .map_err(|_| anyhow::anyhow!("report queue poisoned"))?;
            queue.prune(now, Instant::now());
            for report in reports {
                queue.push(report, now, Instant::now())?;
            }
            drop(queue);
            wait(&reader_stop, Duration::from_millis(50));
        }
        Ok(())
    });
    let result = (|| -> Result<()> {
        let agent = agent();
        wait(&stop, startup_delay());
        let mut mono_gate = deadline(&state.gate);
        while !stop.load(Ordering::Relaxed) {
            if reader.is_finished() {
                bail!("report reader stopped; network publishing stopped");
            }
            if state.gate.halted {
                wait(&stop, Duration::from_secs(1));
                continue;
            }
            if !state.gate.ready(wall_ms()) || Instant::now() < mono_gate {
                wait(&stop, Duration::from_millis(200));
                continue;
            }
            let batch = {
                let mut queue = queue
                    .lock()
                    .map_err(|_| anyhow::anyhow!("report queue poisoned"))?;
                if state.gate.interval_ms() > 60_000 {
                    // A server slowdown reduces both request count and sample volume.
                    queue.latest(wall_ms(), Instant::now())
                } else {
                    queue.batch(wall_ms(), Instant::now())
                }
            };
            if batch.is_empty() {
                wait(&stop, Duration::from_millis(200));
                continue;
            }
            state.gate.reserve_attempt(wall_ms());
            write_state(&dir, &state)?;
            let body = encode_batch(&batch);
            let outcome = send_upload(
                &agent,
                &format!("{}/device-gateway/lens/{hash}/reports/", state.server_url),
                &secret,
                &body,
                &mut state.gate,
            );
            // Failure to persist a gate ends publishing, rather than permitting a retry storm.
            write_state(&dir, &state)?;
            mono_gate = deadline(&state.gate);
            if matches!(outcome, Outcome::Accepted | Outcome::Discard) {
                queue
                    .lock()
                    .map_err(|_| anyhow::anyhow!("report queue poisoned"))?
                    .remove(&batch);
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    let reader_result = reader
        .join()
        .map_err(|_| anyhow::anyhow!("reader thread panicked"))?;
    result.and(reader_result)
}

pub fn status(dir: &Path) -> Result<()> {
    let state = read_state(dir)?;
    if state.device_hash.is_none() || state.device_secret.is_none() {
        bail!("registration pending");
    }
    if state.gate.halted {
        bail!("publisher halted; explicit credential/backend repair required");
    }
    println!(
        "Registered. Minimum upload interval {} seconds. Next network attempt no earlier than {}.",
        state.gate.interval_ms() / 1000,
        state.gate.not_before_ms
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("lens-publisher-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).expect("create fixture directory");
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .expect("private fixture directory");
            Self(dir)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn state() -> State {
        State {
            server_url: "https://example.invalid".into(),
            registration_id: uuid::Uuid::new_v4().to_string(),
            device_hash: Some("a".repeat(64)),
            device_secret: Some("secret-fixture".repeat(4)),
            gate: Gate::default(),
        }
    }
    #[test]
    fn cooldown_and_halt_survive_disk_roundtrip_and_secrets_are_redacted() {
        let dir = Directory::new();
        let mut s = state();
        s.gate.reserve_attempt(1000);
        s.gate.status(429, Some("7200"), None, 1000);
        write_state(&dir.0, &s).expect("persist fixture");
        let loaded = read_state(&dir.0).expect("load fixture");
        assert_eq!(loaded.gate.not_before_ms, s.gate.not_before_ms);
        assert!(!loaded.gate.ready(7_200_999));
        assert!(!format!("{loaded:?}").contains("secret-fixture"));
        s.gate.halted = true;
        write_state(&dir.0, &s).expect("persist halt");
        assert!(read_state(&dir.0).expect("load halt").gate.halted);
        assert_eq!(
            fs::metadata(dir.0.join("state.json"))
                .expect("state metadata")
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn insecure_corrupt_or_symlinked_state_fails_closed() {
        let dir = Directory::new();
        write_state(&dir.0, &state()).expect("persist fixture");
        fs::set_permissions(dir.0.join("state.json"), fs::Permissions::from_mode(0o644))
            .expect("change fixture permissions");
        assert!(read_state(&dir.0).is_err());
        fs::remove_file(dir.0.join("state.json")).expect("remove fixture");
        std::os::unix::fs::symlink("nonexistent", dir.0.join("state.json"))
            .expect("fixture symlink");
        assert!(read_state(&dir.0).is_err());
    }
    #[test]
    fn only_https_origins_are_accepted_and_redirects_are_disabled() {
        for url in [
            "http://localhost",
            "https://user:password@example.com",
            "https://example.com/path",
            "https://example.com/?token=secret",
            "file:///etc/passwd",
        ] {
            assert!(validate_server(url).is_err());
        }
        assert!(validate_server("https://example.com").is_ok());
        assert_eq!(agent().config().max_redirects(), 0);
        assert!(agent().config().https_only());
        assert_eq!(agent().config().max_response_header_size(), 16384);
    }
    fn mock_server(response: &'static str) -> (String, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let address = listener.local_addr().expect("test address");
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("test read timeout");
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            while !bytes.windows(4).any(|s| s == b"\r\n\r\n") {
                let n = stream.read(&mut buf).expect("request headers");
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            let header_end = bytes
                .windows(4)
                .position(|s| s == b"\r\n\r\n")
                .expect("headers")
                + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().expect("length"))
                })
                .unwrap_or(0);
            while bytes.len() < header_end + length {
                let n = stream.read(&mut buf).expect("request body");
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            stream.write_all(response.as_bytes()).expect("respond once");
            1
        });
        (format!("http://{address}/reports"), thread)
    }
    #[test]
    fn actual_success_announces_four_hour_interval_without_reading_a_control_body() {
        let (url, server) = mock_server(
            "HTTP/1.1 201 Created\r\nLens-Report-Interval-Seconds: 14400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let local: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .into();
        let mut gate = Gate::default();
        let before = wall_ms();
        gate.reserve_attempt(before);
        assert_eq!(
            send_upload(&local, &url, "fixture", b"{}", &mut gate),
            Outcome::Accepted
        );
        assert_eq!(gate.interval_seconds, 14400);
        assert!(!gate.ready(before + 14_400_000 - 1));
        assert_eq!(server.join().expect("server"), 1);
    }
    #[test]
    fn actual_429_is_one_request_and_long_retry_after_is_not_capped() {
        let (url, server) = mock_server(
            "HTTP/1.1 429 Too Many Requests\r\nLens-Report-Interval-Seconds: 120\r\nRetry-After: 7200\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        // Cleartext is confined to this local transport test. Production always uses agent().
        let local: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .into();
        let mut gate = Gate::default();
        let before = wall_ms();
        gate.reserve_attempt(before);
        assert_eq!(
            send_upload(&local, &url, "fixture", b"{}", &mut gate),
            Outcome::Retry
        );
        assert!(gate.not_before_ms >= before + 7_200_000);
        assert_eq!(gate.interval_seconds, 120);
        for tick in 0..7200 {
            assert!(!gate.ready(before + tick * 1000));
        }
        assert_eq!(server.join().expect("test server"), 1);
    }
    #[test]
    fn actual_redirect_does_not_forward_credentials_or_follow_location() {
        let (url, server) = mock_server(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/never\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let local: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .into();
        let mut gate = Gate::default();
        assert_eq!(
            send_upload(&local, &url, "fixture", b"{}", &mut gate),
            Outcome::Halt
        );
        assert!(gate.halted);
        assert_eq!(server.join().expect("test server"), 1);
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    #[test]
    fn enormous_retry_after_survives_restart_without_instant_overflow_or_early_retry() {
        let gate = Gate {
            not_before_ms: u64::MAX,
            ..Gate::default()
        };
        assert!(deadline(&gate) > Instant::now());
        assert!(!gate.ready(wall_ms()));
    }
}
