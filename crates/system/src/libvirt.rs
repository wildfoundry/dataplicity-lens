//! Local, read-only libvirt inventory. Never consult a user's default/remote URI.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use lens_model::{Container, ContainerResources};

pub(crate) const CONNECTION: [&str; 3] = ["--readonly", "--connect", "qemu:///system"];

pub(crate) fn collect(warnings: &mut Vec<String>) -> Result<Vec<Container>, String> {
    let started = Instant::now();
    let query = |args: &[&str]| {
        let remaining = Duration::from_secs(8).saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("libvirt collection timed out".to_owned());
        }
        super::containers::run_text("virsh", args, remaining)
    };
    let identity_args = [
        "--readonly",
        "--connect",
        "qemu:///system",
        "list",
        "--all",
        "--uuid",
        "--name",
    ];
    let before = identities(&query(&identity_args)?)?;
    let mut args = CONNECTION.to_vec();
    args.extend([
        "domstats",
        "--raw",
        "--nowait",
        "--state",
        "--cpu-total",
        "--balloon",
        "--vcpu",
        "--interface",
        "--block",
    ]);
    let rows = parse(&query(&args)?)?;
    let after = identities(&query(&identity_args)?)?;
    Ok(join_identities(rows, &before, &after, warnings))
}

fn join_identities(
    mut rows: Vec<Container>,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Vec<Container> {
    let count = rows.len();
    rows.retain_mut(|row| match (before.get(&row.name), after.get(&row.name)) {
        (Some(before), Some(after)) if before == after => {
            row.id.clone_from(before);
            true
        }
        _ => false,
    });
    if rows.len() != count {
        warnings.push("libvirt: VM identity changed during collection; unstable rows omitted until next sample".into());
    }
    rows
}

fn identities(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (uuid, name) = line
            .split_once(char::is_whitespace)
            .ok_or("invalid libvirt identity")?;
        if uuid.len() != 36
            || !uuid.chars().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
            || name.trim().is_empty()
        {
            return Err("invalid libvirt UUID or domain name".into());
        }
        if result.insert(name.trim().into(), uuid.into()).is_some() {
            return Err("duplicate libvirt identity".into());
        }
    }
    Ok(result)
}

fn parse(text: &str) -> Result<Vec<Container>, String> {
    let mut domains: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(name) = line
            .strip_prefix("Domain: '")
            .and_then(|s| s.strip_suffix('\''))
        {
            if name.is_empty() || domains.iter().any(|(existing, _)| existing == name) {
                return Err("invalid or duplicate libvirt domain name".into());
            }
            domains.push((name.into(), BTreeMap::new()));
        } else if let Some((key, value)) = line.split_once('=') {
            let (_, fields) = domains
                .last_mut()
                .ok_or("libvirt statistics missing domain")?;
            fields.insert(key.into(), value.into());
        } else {
            return Err("unrecognized libvirt statistics output".into());
        }
    }
    Ok(domains
        .into_iter()
        .map(|(name, fields)| domain(name, &fields))
        .collect())
}

fn domain(name: String, fields: &BTreeMap<String, String>) -> Container {
    let number = |key: &str| fields.get(key)?.parse::<u64>().ok();
    let (state, status) = match number("state.state") {
        Some(1) => ("running", "running"),
        Some(2) => ("running", "blocked"),
        Some(3) => ("paused", "paused"),
        Some(4) => ("stopping", "shutting down"),
        Some(5) => ("exited", "shut off"),
        Some(6) => ("dead", "crashed"),
        Some(7) => ("paused", "suspended"),
        _ => ("unknown", "unknown"),
    };
    let sum = |group: &str, counter: &str| -> Option<u64> {
        let count = number(&format!("{group}.count"))?;
        // Bound work even if a CLI emits malformed counts. Missing counters remain unknown.
        if count > 4096 {
            return None;
        }
        (0..count).try_fold(0u64, |total, index| {
            total.checked_add(number(&format!("{group}.{index}.{counter}"))?)
        })
    };
    let resources = ContainerResources {
        cpu_limit: number("vcpu.current").map(|n| format!("{n} vCPUs")),
        cpu_usage_ns: number("cpu.time"),
        memory_limit: number("balloon.maximum").map(|n| format!("{n}KiB")),
        // Host RSS, rather than pretending balloon allocation is guest RAM consumption.
        memory_usage_bytes: number("balloon.rss").and_then(|n| n.checked_mul(1024)),
        network_rx_bytes: sum("net", "rx.bytes"),
        network_tx_bytes: sum("net", "tx.bytes"),
        disk_read_bytes: sum("block", "rd.bytes"),
        disk_write_bytes: sum("block", "wr.bytes"),
        root_disk_capacity_bytes: number("block.0.capacity"),
        root_disk_usage_bytes: number("block.0.physical"),
        mounts: fields
            .iter()
            .filter(|(key, _)| key.starts_with("block.") && key.ends_with(".path"))
            .map(|(_, value)| value.clone())
            .collect(),
        ..ContainerResources::default()
    };
    Container {
        runtime: "libvirt".into(),
        id: name.clone(),
        name,
        instance_type: Some("virtual-machine".into()),
        project: None,
        resources: Some(resources),
        image: String::new(),
        state: state.into(),
        status: status.into(),
        created: String::new(),
        ports: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_vms_and_preserves_counter_units_and_missing_data() {
        let rows = parse("Domain: 'builder one'\n state.state=1\n cpu.time=2500000000\n balloon.maximum=12582912\n balloon.rss=1048576\n vcpu.current=4\n net.count=2\n net.0.rx.bytes=100\n net.1.rx.bytes=50\n net.0.tx.bytes=20\n block.count=1\n block.0.rd.bytes=400\n block.0.wr.bytes=800\n block.0.capacity=42949672960\n block.0.physical=123456\n block.0.path=/data/builder.qcow2\n\nDomain: 'offline'\n state.state=5\n").expect("valid stats");
        assert_eq!(rows[0].name, "builder one");
        let r = rows[0].resources.as_ref().expect("resources");
        assert_eq!(r.cpu_usage_ns, Some(2_500_000_000));
        assert_eq!(r.memory_usage_bytes, Some(1_073_741_824));
        assert_eq!(r.network_rx_bytes, Some(150));
        assert_eq!(r.network_tx_bytes, None);
        assert_eq!(r.disk_read_bytes, Some(400));
        assert_eq!(r.root_disk_usage_bytes, Some(123456));
        assert_eq!(rows[1].state, "exited");
        assert_eq!(
            rows[1]
                .resources
                .as_ref()
                .expect("resources")
                .memory_usage_bytes,
            None
        );
    }

    #[test]
    fn recycled_vm_names_do_not_inherit_the_previous_identity() {
        let before =
            identities("11111111-1111-1111-1111-111111111111 builder one\n").expect("before");
        let after =
            identities("22222222-2222-2222-2222-222222222222 builder one\n").expect("after");
        let rows = parse("Domain: 'builder one'\n state.state=1\n").expect("stats");
        let mut warnings = Vec::new();
        assert!(join_identities(rows.clone(), &before, &after, &mut warnings).is_empty());
        assert_eq!(warnings.len(), 1);
        let stable = join_identities(rows, &after, &after, &mut Vec::new());
        assert_eq!(stable[0].id, "22222222-2222-2222-2222-222222222222");
        assert!(identities("not-a-uuid builder\n").is_err());
        assert!(identities("11111111-1111-1111-1111-111111111111 builder\n11111111-1111-1111-1111-111111111111 builder\n").is_err());
    }

    #[test]
    fn rejects_unrecognized_output_and_duplicate_identities() {
        assert!(parse("unexpected output").is_err());
        assert!(parse("cpu.time=12").is_err());
        assert!(parse("Domain: 'same'\nDomain: 'same'\n").is_err());
        assert!(parse("").expect("empty inventory").is_empty());
        assert_eq!(CONNECTION, ["--readonly", "--connect", "qemu:///system"]);
    }
}
