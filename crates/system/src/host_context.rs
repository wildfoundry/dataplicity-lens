//! Optional local host metadata. Never reads credentials, user-data or process environments.
use lens_model::{CpuTopology, ServerContext};
#[cfg(any(target_os = "linux", test))]
use serde_json::Value;
#[cfg(any(target_os = "linux", test))]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::{fs, path::Path};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

/// Bounded optional command, including commands such as smartctl whose exit code is a bitmask.
/// Output is capped; stdin is closed and stderr discarded so probes cannot prompt or flood Lens.
pub(super) fn probe(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(1024 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let bytes = reader.join().ok()?;
    if bytes.len() >= 1024 * 1024 {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(target_os = "linux")]
fn text(path: impl AsRef<Path>) -> Option<String> {
    let text = fs::read_to_string(path).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

pub(super) fn cpu() -> Option<CpuTopology> {
    static CPU: OnceLock<Option<CpuTopology>> = OnceLock::new();
    CPU.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            let info = text("/proc/cpuinfo")?;
            let mut cpu = parse_cpuinfo(&info);
            // ARM and some virtual machines omit physical/core IDs from cpuinfo.
            let mut sockets = BTreeSet::new();
            let mut cores = BTreeSet::new();
            let mut complete = true;
            for entry in fs::read_dir("/sys/devices/system/cpu").ok()?.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name
                    .strip_prefix("cpu")
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                {
                    continue;
                }
                if text(entry.path().join("online")).as_deref() == Some("0") {
                    continue;
                }
                let topology = entry.path().join("topology");
                match (
                    text(topology.join("physical_package_id")),
                    text(topology.join("core_id")),
                ) {
                    (Some(socket), Some(core)) if socket != "-1" && core != "-1" => {
                        sockets.insert(socket.clone());
                        cores.insert((socket, core));
                    }
                    _ => complete = false,
                }
            }
            if complete && !cores.is_empty() {
                cpu.sockets = Some(sockets.len());
                cpu.cores = Some(cores.len());
            }
            if cpu.model.is_none() {
                cpu.model = text("/sys/firmware/devicetree/base/model");
            }
            Some(cpu)
        }
        #[cfg(target_os = "macos")]
        {
            let get = |key| {
                probe("sysctl", &["-n", key], Duration::from_millis(500))
                    .map(|s| s.trim().to_owned())
            };
            Some(CpuTopology {
                model: get("machdep.cpu.brand_string"),
                sockets: get("hw.packages").and_then(|s| s.parse().ok()),
                cores: get("hw.physicalcpu").and_then(|s| s.parse().ok()),
                threads: get("hw.logicalcpu")?.parse().ok()?,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        None
    })
    .clone()
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpuinfo(info: &str) -> CpuTopology {
    let mut result = CpuTopology::default();
    let mut sockets = BTreeSet::new();
    let mut cores = BTreeSet::new();
    let mut complete = true;
    for record in info.split("\n\n") {
        let fields: BTreeMap<_, _> = record
            .lines()
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        if !fields.contains_key("processor") {
            continue;
        }
        result.threads += 1;
        if result.model.is_none() {
            result.model = fields
                .get("model name")
                .or_else(|| fields.get("Processor"))
                .map(|s| (*s).to_owned());
        }
        match (fields.get("physical id"), fields.get("core id")) {
            (Some(socket), Some(core)) => {
                sockets.insert(*socket);
                cores.insert((*socket, *core));
            }
            _ => complete = false,
        }
    }
    if complete && !cores.is_empty() {
        result.sockets = Some(sockets.len());
        result.cores = Some(cores.len());
    }
    result
}

/// Slow optional host context is refreshed at most once per minute, away from UI rendering.
pub(super) fn server() -> Option<ServerContext> {
    let reading = super::power::collect();
    let mut context =
        cached_server().or_else(|| reading.as_ref().map(|_| ServerContext::default()))?;
    context.power = reading;
    Some(context)
}

fn cached_server() -> Option<ServerContext> {
    static CACHE: Mutex<Option<(Instant, Option<ServerContext>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, value)) = &*cache {
        if at.elapsed() < Duration::from_secs(60) {
            return value.clone();
        }
    }
    let value = collect_server();
    *cache = Some((Instant::now(), value.clone()));
    value
}

#[cfg(not(target_os = "linux"))]
fn collect_server() -> Option<ServerContext> {
    None
}

#[cfg(target_os = "linux")]
fn collect_server() -> Option<ServerContext> {
    let mut context = ServerContext::default();
    #[cfg(target_os = "linux")]
    {
        let vendor = text("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
        let model = text("/sys/class/dmi/id/product_name")
            .unwrap_or_default()
            .to_lowercase();
        let has_ipmi =
            fs::read_dir("/sys/class/ipmi").is_ok_and(|mut entries| entries.next().is_some());
        let server_model = [
            "poweredge",
            "proliant",
            "system x",
            "xseries",
            "thinksystem",
            "thinkserver",
        ]
        .iter()
        .any(|name| model.contains(name));
        if has_ipmi || server_model {
            context.management = management_name(&vendor).map(str::to_owned);
        }
        if context.management.is_some() {
            context.service_tag =
                text("/sys/class/dmi/id/product_serial").filter(|s| valid_serial(s));
            if context.service_tag.is_some() {
                context.source.push("DMI system serial".into());
            }
            // Local BMC system interface only: no remote IPMI connection, password or sudo.
            if has_ipmi {
                for channel in ["1", "2", "3"] {
                    if let Some(output) = probe(
                        "ipmitool",
                        &["-I", "open", "lan", "print", channel],
                        Duration::from_millis(800),
                    ) {
                        context.management_addresses = parse_lan(&output);
                        if !context.management_addresses.is_empty() {
                            context.source.push(format!("local IPMI channel {channel}"));
                            break;
                        }
                    }
                }
            }
            // SMBIOS type 42 may advertise a Redfish service address (not a host NIC address).
            if context.management_addresses.is_empty() {
                if let Some(output) =
                    probe("dmidecode", &["--type", "42"], Duration::from_millis(500))
                {
                    context.management_addresses = parse_redfish_address(&output);
                    if !context.management_addresses.is_empty() {
                        context.source.push("SMBIOS Redfish host interface".into());
                    }
                }
            }
        }
        let execution = std::env::var("AWS_EXECUTION_ENV").ok();
        let ecs = execution
            .as_deref()
            .is_some_and(|s| s.starts_with("AWS_ECS_"));
        let endpoint = std::env::var("ECS_CONTAINER_METADATA_URI_V4")
            .ok()
            .or_else(|| std::env::var("ECS_CONTAINER_METADATA_URI").ok());
        if ecs || endpoint.as_deref().is_some_and(valid_ecs_endpoint) {
            context.platform = Some("Amazon ECS".into());
            if let Some(execution) = execution {
                context
                    .details
                    .insert("Execution environment".into(), execution);
            }
            if let Some(endpoint) = endpoint.filter(|s| valid_ecs_endpoint(s)) {
                if let Some(value) = http_json(&format!("{endpoint}/task"), None) {
                    add_ecs(&mut context, &value);
                }
            }
        }
        let ec2 = vendor.eq_ignore_ascii_case("Amazon EC2")
            || text("/sys/hypervisor/uuid").is_some_and(|s| s.to_lowercase().starts_with("ec2"));
        if ec2 {
            if context.platform.is_none() {
                context.platform = Some("Amazon EC2".into());
            }
            context.source.push("EC2 host identity".into());
        }
        // Probe the fixed link-local IMDS endpoint even if DMI is hidden in the container.
        // Fargate has no underlying EC2 instance metadata available to its task.
        if execution_is_fargate(&context) {
            return Some(context);
        }
        if let Some(token) = probe(
            "curl",
            &[
                "--disable",
                "--silent",
                "--fail",
                "--noproxy",
                "*",
                "--connect-timeout",
                "0.2",
                "--max-time",
                "0.4",
                "-X",
                "PUT",
                "-H",
                "X-aws-ec2-metadata-token-ttl-seconds: 60",
                "http://169.254.169.254/latest/api/token",
            ],
            Duration::from_millis(600),
        ) {
            let token = token.trim();
            if !token.is_empty() && token.len() <= 4096 && !token.contains(['\n', '\r']) {
                if let Some(value) = http_json(
                    "http://169.254.169.254/latest/dynamic/instance-identity/document",
                    Some(token),
                ) {
                    add_ec2(&mut context, &value);
                }
            }
        }
    }
    (context.platform.is_some() || context.management.is_some()).then_some(context)
}

#[cfg(target_os = "linux")]
fn execution_is_fargate(context: &ServerContext) -> bool {
    context
        .details
        .get("Execution environment")
        .is_some_and(|s| s == "AWS_ECS_FARGATE")
}

#[cfg(target_os = "linux")]
fn http_json(url: &str, token: Option<&str>) -> Option<Value> {
    let header = token.map(|s| format!("X-aws-ec2-metadata-token: {s}"));
    let mut args = vec![
        "--disable",
        "--silent",
        "--fail",
        "--noproxy",
        "*",
        "--connect-timeout",
        "0.2",
        "--max-time",
        "0.5",
    ];
    if let Some(header) = &header {
        args.extend(["-H", header]);
    }
    args.push(url);
    serde_json::from_str(&probe("curl", &args, Duration::from_millis(700))?).ok()
}

#[cfg(any(target_os = "linux", test))]
fn valid_ecs_endpoint(url: &str) -> bool {
    ["http://169.254.170.2/v4/", "http://169.254.170.2/v3/"]
        .iter()
        .any(|prefix| {
            url.strip_prefix(prefix).is_some_and(|tail| {
                !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        })
}
#[cfg(any(target_os = "linux", test))]
fn management_name(vendor: &str) -> Option<&'static str> {
    let vendor = vendor.to_lowercase();
    if vendor.contains("dell") {
        Some("Dell iDRAC")
    } else if vendor.contains("hewlett") || vendor == "hp" || vendor.contains("hpe") {
        Some("HPE iLO")
    } else if vendor.contains("ibm") {
        Some("IBM IMM / BMC")
    } else if vendor.contains("lenovo") {
        Some("Lenovo XClarity / IMM")
    } else {
        None
    }
}
#[cfg(any(target_os = "linux", test))]
fn valid_serial(value: &str) -> bool {
    !matches!(
        value.to_lowercase().as_str(),
        "unknown" | "none" | "default string" | "to be filled by o.e.m." | "not specified"
    )
}
#[cfg(any(target_os = "linux", test))]
fn valid_address(value: &str) -> bool {
    value
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast())
}
#[cfg(any(target_os = "linux", test))]
fn parse_lan(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (key.trim() == "IP Address" && valid_address(value)).then(|| value.to_owned())
        })
        .collect()
}
#[cfg(any(target_os = "linux", test))]
fn parse_redfish_address(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (matches!(
                key.trim(),
                "IPv4 Redfish Service Address" | "IPv6 Redfish Service Address"
            ) && valid_address(value))
            .then(|| value.to_owned())
        })
        .collect()
}
#[cfg(any(target_os = "linux", test))]
fn add_fields(context: &mut ServerContext, value: &Value, fields: &[(&str, &str)]) {
    for (key, label) in fields {
        if let Some(value) = value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            context.details.insert((*label).into(), value.into());
        }
    }
}
#[cfg(any(target_os = "linux", test))]
fn add_ec2(context: &mut ServerContext, value: &Value) {
    if value.get("instanceId").and_then(Value::as_str).is_none() {
        return;
    }
    if context.platform.is_none() {
        context.platform = Some("Amazon EC2".into());
    }
    add_fields(
        context,
        value,
        &[
            ("instanceId", "EC2 instance"),
            ("instanceType", "Instance type"),
            ("region", "Region"),
            ("availabilityZone", "Availability zone"),
            ("privateIp", "Private IP"),
            ("imageId", "AMI"),
            ("architecture", "Architecture"),
        ],
    );
    context.source.push("EC2 IMDSv2 instance identity".into());
}
#[cfg(any(target_os = "linux", test))]
fn add_ecs(context: &mut ServerContext, value: &Value) {
    add_fields(
        context,
        value,
        &[
            ("Cluster", "ECS cluster"),
            ("TaskARN", "ECS task"),
            ("Family", "Task family"),
            ("Revision", "Task revision"),
            ("LaunchType", "Launch type"),
            ("AvailabilityZone", "Availability zone"),
        ],
    );
    context.source.push("ECS task metadata".into());
}

pub(super) fn cpu_summary(cpu: &CpuTopology) -> String {
    format!(
        "{} CPUs  /  {} cores  /  {} threads",
        cpu.sockets.map_or_else(|| "?".into(), |n| n.to_string()),
        cpu.cores.map_or_else(|| "?".into(), |n| n.to_string()),
        cpu.threads
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn topology_counts_core_ids_per_socket_and_smt() {
        let info = "processor: 0\nphysical id: 0\ncore id: 0\nmodel name: Xeon\n\nprocessor: 1\nphysical id: 0\ncore id: 0\n\nprocessor: 2\nphysical id: 1\ncore id: 0\n";
        let cpu = parse_cpuinfo(info);
        assert_eq!(cpu.sockets, Some(2));
        assert_eq!(cpu.cores, Some(2));
        assert_eq!(cpu.threads, 3);
        let cpu = parse_cpuinfo("processor: 0\nmodel name: Virtual CPU\n");
        assert_eq!(cpu.cores, None);
        assert_eq!(cpu.sockets, None);
    }
    #[test]
    fn management_does_not_confuse_mac_gateway_and_host_addresses() {
        assert_eq!(
            parse_lan(
                "IP Address Source : DHCP\nIP Address : 192.0.2.10\nDefault Gateway IP : 192.0.2.1\nMAC Address : aa:bb:cc:dd:ee:ff"
            ),
            vec!["192.0.2.10"]
        );
        assert!(parse_lan("IP Address : 0.0.0.0").is_empty());
        assert_eq!(
            parse_redfish_address(
                "IPv4 Address: 192.0.2.2\nIPv4 Redfish Service Address: 192.0.2.3"
            ),
            vec!["192.0.2.3"]
        );
        assert_eq!(management_name("Dell Inc."), Some("Dell iDRAC"));
        assert_eq!(management_name("HP"), Some("HPE iLO"));
        assert!(management_name("Amazon EC2").is_none());
    }
    #[test]
    fn metadata_is_allowlisted_and_ecs_urls_are_local_only() {
        assert!(valid_ecs_endpoint("http://169.254.170.2/v4/abc-123"));
        for url in [
            "http://example.com/v4/abc",
            "http://169.254.170.2@evil/v4/a",
            "http://169.254.170.2/v4/a/../../credentials",
            "http://169.254.170.2/v4/a?query",
        ] {
            assert!(!valid_ecs_endpoint(url));
        }
        let mut context = ServerContext::default();
        add_ec2(
            &mut context,
            &serde_json::json!({"instanceId":"i-example", "instanceType":"m7i.large", "secretAccessKey":"secret", "accountId":"123"}),
        );
        assert_eq!(context.details.len(), 2);
        assert!(!format!("{context:?}").contains("secret"));
    }
}
