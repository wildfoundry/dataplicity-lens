//! Read-only optional kernel metadata. No probes, privileges, daemon or guest commands.
use super::*;
use lens_model::{NetworkDetail, StorageDetail};

#[cfg(any(target_os = "linux", test))]
fn text(path: impl AsRef<Path>) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}
#[cfg(any(target_os = "linux", test))]
fn number(path: impl AsRef<Path>) -> Option<u64> {
    text(path)?.parse().ok()
}
#[cfg(any(target_os = "linux", test))]
fn flag(path: impl AsRef<Path>) -> Option<bool> {
    match number(path)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}
#[cfg(any(target_os = "linux", test))]
fn names(path: impl AsRef<Path>) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|item| item.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

pub(super) fn storage(devices: &[BlockDevice]) -> BTreeMap<String, StorageDetail> {
    #[cfg(target_os = "linux")]
    return devices
        .iter()
        .filter_map(|device| {
            let path = Path::new("/sys/class/block").join(&device.name);
            path.is_dir()
                .then(|| (device.name.clone(), storage_at(&path)))
        })
        .collect();
    #[cfg(not(target_os = "linux"))]
    {
        let _ = devices;
        BTreeMap::new()
    }
}

#[cfg(any(target_os = "linux", test))]
fn storage_at(path: &Path) -> StorageDetail {
    let stats: Vec<u64> = text(path.join("stat"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_default();
    StorageDetail {
        model: text(path.join("device/model")),
        parent: path
            .join("partition")
            .exists()
            .then(|| {
                std::fs::canonicalize(path)
                    .ok()?
                    .parent()?
                    .file_name()?
                    .to_str()
                    .map(str::to_owned)
            })
            .flatten(),
        rotational: flag(path.join("queue/rotational")),
        read_only: flag(path.join("ro")),
        // Kernel block stats always report 512-byte sectors, independent of logical sector size.
        read_bytes: stats.get(2).and_then(|sectors| sectors.checked_mul(512)),
        write_bytes: stats.get(6).and_then(|sectors| sectors.checked_mul(512)),
        io_active_ms: stats.get(9).copied(),
    }
}

pub(super) fn network(interfaces: &[Interface]) -> BTreeMap<String, NetworkDetail> {
    #[cfg(target_os = "linux")]
    return interfaces
        .iter()
        .filter_map(|interface| {
            let path = Path::new("/sys/class/net").join(&interface.name);
            path.is_dir()
                .then(|| (interface.name.clone(), network_at(&path)))
        })
        .collect();
    #[cfg(not(target_os = "linux"))]
    {
        let _ = interfaces;
        BTreeMap::new()
    }
}

#[cfg(any(target_os = "linux", test))]
fn network_at(path: &Path) -> NetworkDetail {
    let kind = if path.join("bridge").is_dir() {
        "bridge"
    } else if path.join("device").exists() {
        "physical"
    } else if number(path.join("type")) == Some(772) {
        "loopback"
    } else {
        "virtual"
    };
    NetworkDetail {
        kind: kind.into(),
        mac_address: text(path.join("address")),
        mtu: number(path.join("mtu")),
        carrier: flag(path.join("carrier")),
        speed_mbps: number(path.join("speed")).filter(|n| *n > 0 && *n < u32::MAX as u64),
        duplex: text(path.join("duplex")).filter(|value| value == "full" || value == "half"),
        master: std::fs::read_link(path.join("master"))
            .ok()
            .and_then(|target| target.file_name()?.to_str().map(str::to_owned)),
        ports: names(path.join("brif")),
        rx_errors: number(path.join("statistics/rx_errors")),
        tx_errors: number(path.join("statistics/tx_errors")),
        rx_dropped: number(path.join("statistics/rx_dropped")),
        tx_dropped: number(path.join("statistics/tx_dropped")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_metadata_is_unknown_and_counters_are_not_invented() {
        let root = std::env::temp_dir().join(format!("lens-native-{}", std::process::id()));
        std::fs::create_dir_all(root.join("statistics")).expect("fixture");
        std::fs::write(root.join("speed"), "-1").expect("fixture");
        std::fs::write(root.join("carrier"), "0").expect("fixture");
        std::fs::write(root.join("stat"), "10 0 4 5 11 0 8 9 0 20 25").expect("fixture");
        let link = network_at(&root);
        assert_eq!(link.speed_mbps, None);
        assert_eq!(link.carrier, Some(false));
        assert_eq!(link.rx_errors, None);
        let disk = storage_at(&root);
        assert_eq!(disk.read_bytes, Some(2048));
        assert_eq!(disk.write_bytes, Some(4096));
        assert_eq!(disk.io_active_ms, Some(20));
        assert_eq!(disk.model, None);
        std::fs::write(root.join("stat"), "broken").expect("fixture");
        assert_eq!(storage_at(&root).read_bytes, None);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
