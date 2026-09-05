use std::{
    env,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use lens_model::Container;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeClass {
    Ok,
    Permission,
    NotLive,
}

pub(crate) fn collect_containers(warnings: &mut Vec<String>) -> (Vec<Container>, bool) {
    let mut containers = Vec::new();
    let mut runtime_live = false;
    for runtime in ["docker", "podman", "nerdctl"] {
        if !binary_on_path(runtime) {
            continue;
        }
        match probe_runtime(runtime) {
            ProbeClass::NotLive => {}
            ProbeClass::Permission => {
                warnings.push(permission_warning(runtime));
            }
            ProbeClass::Ok => {
                runtime_live = true;
                match list_containers(runtime) {
                    Ok(mut rows) => containers.append(&mut rows),
                    Err(message) => match classify_runtime_error(&message) {
                        ProbeClass::Permission => {
                            runtime_live = false;
                            warnings.push(permission_warning(runtime));
                        }
                        ProbeClass::NotLive => {
                            runtime_live = false;
                        }
                        ProbeClass::Ok => {
                            warnings.push(format!("{runtime} unavailable: {message}"))
                        }
                    },
                }
            }
        }
    }
    containers.sort_by(|left, right| {
        left.runtime
            .cmp(&right.runtime)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    containers.dedup_by(|left, right| left.runtime == right.runtime && left.id == right.id);
    (containers, runtime_live)
}

pub(crate) fn runtime_is_usable(runtime: &str) -> bool {
    binary_on_path(runtime) && probe_runtime(runtime) == ProbeClass::Ok
}

fn binary_on_path(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|path| {
            let candidate = path.join(program);
            candidate.is_file()
        })
    })
}

fn probe_runtime(runtime: &str) -> ProbeClass {
    match run_text(runtime, &["info"], Duration::from_secs(5)) {
        Ok(_) => ProbeClass::Ok,
        Err(message) => classify_runtime_error(&message),
    }
}

fn list_containers(runtime: &str) -> Result<Vec<Container>, String> {
    let text = run_text(
        runtime,
        &["ps", "-a", "--format", "{{json .}}"],
        Duration::from_secs(8),
    )?;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(container) = parse_ps_json_line(runtime, line) {
            rows.push(container);
        }
    }
    Ok(rows)
}

pub(crate) fn classify_runtime_error(message: &str) -> ProbeClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("operation not permitted")
        || (lower.contains("are you in the") && lower.contains("group"))
        || (lower.contains("dial unix") && lower.contains("permission"))
    {
        return ProbeClass::Permission;
    }
    if lower.contains("cannot connect")
        || lower.contains("is the docker daemon running")
        || lower.contains("connection refused")
        || lower.contains("no such file or directory")
        || lower.contains("error: unable to connect")
        || lower.contains("podman machine") && lower.contains("not running")
        || lower.contains("the system has no container")
        || lower.contains("rootless containerd not running")
        || lower.contains("containerd sock") && lower.contains("no such")
    {
        return ProbeClass::NotLive;
    }
    // Unknown CLI failure while binary exists: treat as not-live to avoid noisy warnings
    // unless the message clearly indicates auth/socket permission.
    if lower.contains("denied") || lower.contains("unauthorized") || lower.contains("forbidden") {
        return ProbeClass::Permission;
    }
    ProbeClass::NotLive
}

fn permission_warning(runtime: &str) -> String {
    format!(
        "{runtime} is installed but not usable by the current user (socket or {runtime} group access denied)"
    )
}

fn parse_ps_json_line(runtime: &str, line: &str) -> Option<Container> {
    let value: Value = serde_json::from_str(line).ok()?;
    let id = first_string(&value, &["ID", "Id", "Id"])?;
    let name = first_string(&value, &["Names", "Name", "Names"])
        .map(normalize_name)
        .unwrap_or_else(|| id.chars().take(12).collect());
    let image = first_string(&value, &["Image"]).unwrap_or_default();
    let status = first_string(&value, &["Status"]).unwrap_or_default();
    let state = normalize_state(
        &first_string(&value, &["State", "Status"]).unwrap_or_else(|| status.clone()),
    );
    let created = first_string(&value, &["CreatedAt", "Created", "created"]).unwrap_or_default();
    let ports = ports_from_value(&value);
    Some(Container {
        runtime: runtime.to_owned(),
        id,
        name,
        image,
        status,
        state,
        created,
        ports,
    })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            Some(Value::Number(number)) => return Some(number.to_string()),
            Some(Value::Array(items)) => {
                let joined = items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
            _ => {}
        }
    }
    None
}

fn ports_from_value(value: &Value) -> String {
    if let Some(text) = first_string(value, &["Ports", "ports"]) {
        return text;
    }
    String::new()
}

fn normalize_name(name: String) -> String {
    name.trim_start_matches('/')
        .split(',')
        .next()
        .unwrap_or(&name)
        .trim()
        .to_owned()
}

pub(crate) fn normalize_state(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("up") || lower == "running" {
        return "running".into();
    }
    if lower.contains("paused") {
        return "paused".into();
    }
    if lower.contains("restarting") {
        return "restarting".into();
    }
    if lower.contains("created") {
        return "created".into();
    }
    if lower.contains("dead") {
        return "dead".into();
    }
    if lower.contains("exited") || lower.starts_with("exit") {
        return "exited".into();
    }
    if lower.contains("removing") {
        return "removing".into();
    }
    if lower.is_empty() {
        return "unknown".into();
    }
    lower
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_owned()
}

fn run_text(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                if status.success() {
                    return Ok(stdout);
                }
                let detail = stderr.trim();
                if detail.is_empty() {
                    return Err(format!("exit {status}"));
                }
                return Err(detail.to_owned());
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {timeout:?}"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(crate) fn run_container_cli(
    runtime: &str,
    action: &str,
    target: &str,
    timeout: Duration,
) -> Result<(), String> {
    let mut child = Command::new(runtime)
        .args([action, "--", target])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let detail = stderr.trim();
                if detail.is_empty() {
                    return Err(format!("{runtime} {action} failed: {status}"));
                }
                return Err(format!("{runtime} {action} failed: {detail}"));
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{runtime} {action} timed out after {timeout:?}"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_permission_vs_not_live() {
        assert_eq!(
            classify_runtime_error(
                "Got permission denied while trying to connect to the Docker daemon socket"
            ),
            ProbeClass::Permission
        );
        assert_eq!(
            classify_runtime_error(
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?"
            ),
            ProbeClass::NotLive
        );
        assert_eq!(
            classify_runtime_error("Error: unable to connect to Podman"),
            ProbeClass::NotLive
        );
    }

    #[test]
    fn parses_docker_ps_json_line() {
        let line = r#"{"ID":"abc123def456","Names":"/web","Image":"nginx:latest","Status":"Up 2 hours","State":"running","CreatedAt":"2026-08-01 10:00:00 +0000 UTC","Ports":"0.0.0.0:8080->80/tcp"}"#;
        let container = parse_ps_json_line("docker", line).expect("parse");
        assert_eq!(container.runtime, "docker");
        assert_eq!(container.id, "abc123def456");
        assert_eq!(container.name, "web");
        assert_eq!(container.image, "nginx:latest");
        assert_eq!(container.status, "Up 2 hours");
        assert_eq!(container.state, "running");
        assert!(container.created.contains("2026-08-01"));
        assert!(container.ports.contains("8080"));
    }

    #[test]
    fn parses_nerdctl_ps_json_line() {
        let line = r#"{"Command":"\"python -u /app/main…\"","CreatedAt":"2026-09-05T14:03:32Z","ID":"31a89e811214","Image":"localhost/grain-silo-sim:0.1.1","Names":"dp-grain-silo-sim","Ports":"","Status":"Up"}"#;
        let container = parse_ps_json_line("nerdctl", line).expect("parse");
        assert_eq!(container.runtime, "nerdctl");
        assert_eq!(container.id, "31a89e811214");
        assert_eq!(container.name, "dp-grain-silo-sim");
        assert_eq!(container.image, "localhost/grain-silo-sim:0.1.1");
        assert_eq!(container.status, "Up");
        assert_eq!(container.state, "running");
    }

    #[test]
    fn classifies_nerdctl_rootless_as_not_live() {
        assert_eq!(
            classify_runtime_error(
                "rootless containerd not running? (hint: use `containerd-rootless-setuptool.sh install`)"
            ),
            ProbeClass::NotLive
        );
    }

    #[test]
    fn normalize_state_from_status_strings() {
        assert_eq!(normalize_state("Up 3 minutes"), "running");
        assert_eq!(normalize_state("Exited (0) 2 days ago"), "exited");
        assert_eq!(normalize_state("Created"), "created");
        assert_eq!(
            normalize_state("Restarting (1) 5 seconds ago"),
            "restarting"
        );
    }
}
