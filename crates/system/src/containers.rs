use std::{
    env,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use lens_model::{Container, ContainerResources};
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
                    Ok(mut rows) => {
                        if runtime != "lxd" {
                            enrich_oci_resources(runtime, &mut rows, warnings);
                        }
                        containers.append(&mut rows);
                    }
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
        resources: None,
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
                resources: Some(lxd_resources(value)),
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

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn lxd_resources(value: &Value) -> ContainerResources {
    // Expanded values include inherited profile allocations, not just local overrides.
    let config = value
        .get("expanded_config")
        .filter(|v| v.is_object())
        .unwrap_or(&value["config"]);
    let devices = value
        .get("expanded_devices")
        .filter(|v| v.is_object())
        .unwrap_or(&value["devices"]);
    let root = devices.as_object().and_then(|devices| {
        devices
            .iter()
            .find(|(_, device)| device["type"] == "disk" && device["path"] == "/")
    });
    let mut r = ContainerResources {
        cpu_limit: text(&config["limits.cpu"]),
        memory_limit: text(&config["limits.memory"]),
        cpu_usage_ns: value["state"]["cpu"]["usage"].as_u64(),
        memory_usage_bytes: value["state"]["memory"]["usage"].as_u64(),
        ..Default::default()
    };
    if let Some((name, root)) = root {
        r.root_disk_limit = text(&root["size"]);
        r.storage_pool = text(&root["pool"]);
        r.root_disk_capacity_bytes = value["state"]["disk"][name]["total"]
            .as_u64()
            .filter(|n| *n > 0);
        // LXD dir-backed VMs may report a zero placeholder without guest disk usage.
        r.root_disk_usage_bytes = value["state"]["disk"][name]["usage"]
            .as_u64()
            .filter(|n| *n > 0 || value["type"] != "virtual-machine");
    }
    if let Some(devices) = devices.as_object() {
        r.mounts = devices
            .values()
            .filter(|device| device["type"] == "disk" && device["path"] != "/")
            .filter_map(|device| {
                text(&device["path"]).map(|path| {
                    format!(
                        "{} -> {}{}",
                        device["source"].as_str().unwrap_or("volume"),
                        path,
                        if device["readonly"] == "true" {
                            " (read-only)"
                        } else {
                            ""
                        }
                    )
                })
            })
            .collect();
    }
    r
}

fn json_rows(text: &str) -> Result<Vec<Value>, String> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return match value {
            Value::Array(rows) => Ok(rows),
            Value::Object(_) => Ok(vec![value]),
            _ => Err("expected JSON object or array".into()),
        };
    }
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
        .collect()
}

fn oci_resources(value: &Value) -> ContainerResources {
    let mut r = ContainerResources::default();
    let host = &value["HostConfig"];
    if host.is_object() {
        let cpus = host["NanoCpus"]
            .as_f64()
            .filter(|n| *n > 0.0)
            .map(|n| n / 1e9)
            .or_else(|| {
                let quota = host["CpuQuota"].as_f64()?;
                let period = host["CpuPeriod"]
                    .as_f64()
                    .filter(|n| *n > 0.0)
                    .unwrap_or(100000.0);
                (quota > 0.0).then_some(quota / period)
            });
        let affinity = host["CpusetCpus"].as_str().filter(|s| !s.is_empty());
        r.cpu_limit = Some(match (cpus, affinity) {
            (Some(cpus), Some(set)) => format!("{cpus:.2} CPUs; set {set}"),
            (Some(cpus), None) => format!("{cpus:.2} CPUs"),
            (None, Some(set)) => format!("CPU set {set}"),
            _ => "Shared (no configured limit)".into(),
        });
        r.memory_limit = Some(
            host["Memory"]
                .as_u64()
                .filter(|n| *n > 0)
                .map_or_else(|| "Shared (no configured limit)".into(), super::human_bytes),
        );
        r.root_disk_limit = text(&host["StorageOpt"]["size"]);
    }
    // SizeRw is writable-layer usage only, not image layers or mounted volumes.
    r.root_disk_usage_bytes = value["SizeRw"].as_u64();
    if let Some(mounts) = value["Mounts"].as_array() {
        r.mounts = mounts
            .iter()
            .filter_map(|mount| {
                text(&mount["Destination"]).map(|destination| {
                    format!(
                        "{} -> {}{}",
                        mount["Name"]
                            .as_str()
                            .or_else(|| mount["Source"].as_str())
                            .unwrap_or("volume"),
                        destination,
                        if mount["RW"] == false {
                            " (read-only)"
                        } else {
                            ""
                        }
                    )
                })
            })
            .collect();
    }
    r
}

fn parse_usage_bytes(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let end = raw
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(raw.len());
    let number: f64 = raw[..end].parse().ok()?;
    let multiplier = match raw[end..].trim().to_ascii_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1048576.0,
        "gib" => 1073741824.0,
        "tib" => 1099511627776.0,
        _ => return None,
    };
    (number.is_finite() && number >= 0.0 && number * multiplier <= u64::MAX as f64)
        .then_some((number * multiplier) as u64)
}

fn enrich_oci_resources(runtime: &str, rows: &mut [Container], warnings: &mut Vec<String>) {
    // Batches avoid a subprocess per instance, keep argv bounded and preserve inventory on failures.
    for batch in rows.chunks_mut(32) {
        let mut args = vec!["inspect", "--type", "container", "--"];
        args.extend(batch.iter().map(|row| row.id.as_str()));
        match run_text(runtime, &args, Duration::from_secs(8)).and_then(|text| json_rows(&text)) {
            Ok(values) => {
                for row in batch.iter_mut() {
                    if let Some(value) = values.iter().find(|value| {
                        value["Id"]
                            .as_str()
                            .or_else(|| value["ID"].as_str())
                            .is_some_and(|id| id == row.id || id.starts_with(&row.id))
                    }) {
                        row.resources = Some(oci_resources(value));
                    }
                }
            }
            Err(error) => warnings.push(format!("{runtime} allocations unavailable: {error}")),
        }
        let running: Vec<_> = batch
            .iter()
            .filter(|row| row.state == "running")
            .map(|row| row.id.as_str())
            .collect();
        if running.is_empty() {
            continue;
        }
        let mut args = vec!["stats", "--no-stream", "--format", "{{json .}}", "--"];
        args.extend(running);
        match run_text(runtime, &args, Duration::from_secs(8)).and_then(|text| json_rows(&text)) {
            Ok(values) => {
                for row in batch.iter_mut() {
                    let Some(value) = values.iter().find(|value| {
                        ["ID", "Container", "ContainerID"].iter().any(|key| {
                            value[key].as_str().is_some_and(|id| {
                                !id.is_empty()
                                    && (id == row.id
                                        || id.starts_with(&row.id)
                                        || row.id.starts_with(id))
                            })
                        })
                    }) else {
                        continue;
                    };
                    let r = row.resources.get_or_insert_default();
                    r.cpu_usage_percent = text(&value["CPUPerc"])
                        .or_else(|| text(&value["CPU"]))
                        .or_else(|| text(&value["CPUPercent"]));
                    r.memory_usage_bytes = value["MemUsage"]
                        .as_str()
                        .and_then(|s| parse_usage_bytes(s.split('/').next().unwrap_or("")));
                }
            }
            Err(error) => warnings.push(format!("{runtime} live usage unavailable: {error}")),
        }
    }
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

#[cfg(test)]
mod resource_tests {
    use super::*;

    #[test]
    fn lxd_inherited_limits_and_zero_vm_disk_placeholder() {
        let value = serde_json::json!({"type":"virtual-machine", "config":{}, "expanded_config":{"limits.cpu":"4", "limits.memory":"12GiB"}, "expanded_devices":{"root":{"type":"disk", "path":"/", "size":"40GiB", "pool":"runner-pool"}}, "state":{"memory":{"usage":12345}, "cpu":{"usage":5000000000_u64}, "disk":{"root":{"usage":0, "total":42949672960_u64}}}});
        let r = lxd_resources(&value);
        assert_eq!(r.cpu_limit.as_deref(), Some("4"));
        assert_eq!(r.memory_limit.as_deref(), Some("12GiB"));
        assert_eq!(r.root_disk_limit.as_deref(), Some("40GiB"));
        assert_eq!(r.storage_pool.as_deref(), Some("runner-pool"));
        assert_eq!(r.memory_usage_bytes, Some(12345));
        assert_eq!(r.cpu_usage_ns, Some(5000000000));
        assert_eq!(r.root_disk_capacity_bytes, Some(42949672960));
        assert_eq!(r.root_disk_usage_bytes, None);
        let stopped =
            lxd_resources(&serde_json::json!({"type":"container", "config":{}, "state":null}));
        assert_eq!(stopped.memory_usage_bytes, None);
    }

    #[test]
    fn docker_podman_and_nerdctl_compatible_allocations() {
        let docker = oci_resources(
            &serde_json::json!({"HostConfig":{"NanoCpus":2000000000_u64,"CpusetCpus":"0-3","Memory":1073741824_u64,"StorageOpt":{"size":"10G"}},"SizeRw":1234,"Mounts":[{"Name":"data","Destination":"/data","RW":false}]}),
        );
        assert_eq!(docker.cpu_limit.as_deref(), Some("2.00 CPUs; set 0-3"));
        assert_eq!(docker.memory_limit.as_deref(), Some("1.0GiB"));
        assert_eq!(docker.root_disk_limit.as_deref(), Some("10G"));
        assert_eq!(docker.root_disk_usage_bytes, Some(1234));
        assert_eq!(docker.mounts, ["data -> /data (read-only)"]);
        let quota = oci_resources(
            &serde_json::json!({"HostConfig":{"CpuQuota":150000,"CpuPeriod":100000,"Memory":0}}),
        );
        assert_eq!(quota.cpu_limit.as_deref(), Some("1.50 CPUs"));
        assert!(quota.memory_limit.unwrap().contains("no configured limit"));
        let shared = oci_resources(&serde_json::json!({"HostConfig":{"CpuQuota":-1,"Memory":0}}));
        assert!(shared.cpu_limit.unwrap().contains("no configured limit"));
        let unknown = oci_resources(&serde_json::json!({}));
        assert_eq!(unknown.cpu_limit, None);
        assert_eq!(unknown.memory_limit, None);
    }

    #[test]
    fn runtime_usage_units_and_json_formats() {
        assert_eq!(parse_usage_bytes("1.5GiB"), Some(1610612736));
        assert_eq!(parse_usage_bytes("12.5 MB"), Some(12500000));
        assert_eq!(parse_usage_bytes("0B"), Some(0));
        assert_eq!(parse_usage_bytes("--"), None);
        assert_eq!(json_rows("[{\"ID\":\"a\"}]").unwrap().len(), 1);
        assert_eq!(
            json_rows("{\"ID\":\"a\"}\n{\"ID\":\"b\"}").unwrap().len(),
            2
        );
        assert!(json_rows("not json").is_err());
    }
}
