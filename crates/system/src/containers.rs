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
    for runtime in ["docker", "podman", "nerdctl", "lxd"] {
        if !binary_on_path(runtime_program(runtime)) {
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
                            warnings.push(permission_warning(runtime));
                        }
                        ProbeClass::NotLive => {
                            warnings.push(format!("{runtime} inventory unavailable: {message}"))
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
    binary_on_path(runtime_program(runtime)) && probe_runtime(runtime) == ProbeClass::Ok
}

fn runtime_program(runtime: &str) -> &str {
    if runtime == "lxd" { "lxc" } else { runtime }
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
    let args: &[&str] = if runtime == "lxd" {
        &["info", "local:"]
    } else {
        &["info"]
    };
    match run_text(runtime_program(runtime), args, Duration::from_secs(5)) {
        Ok(_) => ProbeClass::Ok,
        Err(message) => classify_runtime_error(&message),
    }
}

fn list_containers(runtime: &str) -> Result<Vec<Container>, String> {
    if runtime == "lxd" {
        let text = run_text(
            "lxc",
            &["list", "local:", "--all-projects", "--format=json"],
            Duration::from_secs(8),
        )?;
        return parse_lxd_list(&text);
    }
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
        "{runtime} is installed but not usable by the current user (socket or group access denied)"
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
        instance_type: None,
        project: None,
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

// Pin every operation to the local daemon, irrespective of the user's default remote.
// Project-qualified identities prevent acting on a same-named instance in another project.
fn parse_lxd_list(text: &str) -> Result<Vec<Container>, String> {
    let values: Vec<Value> =
        serde_json::from_str(text).map_err(|error| format!("invalid LXD inventory: {error}"))?;
    values
        .iter()
        .map(|value| {
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("LXD instance missing name")?;
            let project = value
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("container");
            if !matches!(kind, "container" | "virtual-machine") {
                return Err(format!("unknown LXD instance type: {kind}"));
            }
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let config = &value["config"];
            Ok(Container {
                runtime: "lxd".into(),
                id: format!("{project}/{name}"),
                name: name.into(),
                instance_type: Some(kind.into()),
                project: Some(project.into()),
                image: config["image.description"]
                    .as_str()
                    .or_else(|| config["volatile.base_image"].as_str())
                    .unwrap_or("")
                    .into(),
                status: status.into(),
                state: normalize_state(status),
                created: value["created_at"].as_str().unwrap_or("").into(),
                ports: String::new(),
            })
        })
        .collect()
}

fn action_command(
    runtime: &str,
    action: &str,
    target: &str,
) -> Result<(String, Vec<String>), String> {
    if !matches!(action, "start" | "stop" | "restart") {
        return Err("unsupported instance action".into());
    }
    if runtime == "lxd" {
        let (project, name) = target
            .split_once('/')
            .ok_or("LXD target must be project/name")?;
        if [project, name].iter().any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        }) {
            return Err("invalid local LXD project/name".into());
        }
        Ok((
            "lxc".into(),
            vec![
                action.into(),
                "--project".into(),
                project.into(),
                "--".into(),
                format!("local:{name}"),
            ],
        ))
    } else if matches!(runtime, "docker" | "podman" | "nerdctl") {
        Ok((
            runtime.into(),
            vec![action.into(), "--".into(), target.into()],
        ))
    } else {
        Err("unsupported runtime".into())
    }
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
    if lower.contains("paused") || lower == "frozen" {
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

fn read_bounded(pipe: impl Read) -> Result<String, String> {
    const MAX: u64 = 8 * 1024 * 1024;
    let mut data = Vec::new();
    pipe.take(MAX + 1)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() as u64 > MAX {
        return Err("runtime output exceeded 8 MiB".into());
    }
    String::from_utf8(data).map_err(|error| error.to_string())
}

fn run_text(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    // Drain both pipes while the child runs: LXD JSON can exceed a pipe buffer.
    let stdout = child.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.stderr.take().ok_or("missing stderr pipe")?;
    let out_reader = thread::spawn(move || read_bounded(stdout));
    let err_reader = thread::spawn(move || read_bounded(stderr));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = out_reader.join().map_err(|_| "stdout reader failed")??;
                let stderr = err_reader.join().map_err(|_| "stderr reader failed")??;
                if status.success() {
                    return Ok(stdout);
                }
                return Err(if stderr.trim().is_empty() {
                    format!("exit {status}")
                } else {
                    stderr.trim().to_owned()
                });
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {timeout:?}"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

pub(crate) fn run_container_cli(
    runtime: &str,
    action: &str,
    target: &str,
    timeout: Duration,
) -> Result<(), String> {
    let (program, args) = action_command(runtime, action, target)?;
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    run_text(&program, &args, timeout).map(|_| ())
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

#[cfg(test)]
mod lxd_tests {
    use super::*;

    #[test]
    fn inventory_preserves_kind_project_and_duplicate_names() {
        let rows = parse_lxd_list(r#"[
            {"name":"runner","project":"default","type":"virtual-machine","status":"Running","config":{"image.description":"Ubuntu"}},
            {"name":"runner","project":"test","type":"container","status":"Frozen","config":{}}
        ]"#).unwrap();
        assert_eq!(rows[0].id, "default/runner");
        assert_eq!(rows[0].instance_type.as_deref(), Some("virtual-machine"));
        assert_eq!(rows[1].id, "test/runner");
        assert_eq!(rows[1].state, "paused");
        assert!(parse_lxd_list("{}").is_err());
        assert!(parse_lxd_list("[{\"status\":\"Running\"}]").is_err());
    }

    #[test]
    fn actions_are_local_project_scoped_and_never_forced() {
        let (program, args) = action_command("lxd", "stop", "build/runner").unwrap();
        assert_eq!(program, "lxc");
        assert_eq!(args, ["stop", "--project", "build", "--", "local:runner"]);
        for target in [
            "runner",
            "default/remote:runner",
            "default/-all",
            "default/a/b",
            "/runner",
        ] {
            assert!(action_command("lxd", "restart", target).is_err());
        }
        assert!(action_command("lxd", "delete", "default/runner").is_err());
    }

    #[test]
    fn large_cli_output_is_drained_without_deadlock() {
        let output = run_text(
            "sh",
            &["-c", "head -c 200000 /dev/zero | tr '\\000' x"],
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.len(), 200000);
    }
}
