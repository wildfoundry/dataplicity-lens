//! Optional storage health; kernel transport state is never treated as a SMART result.
#[cfg(target_os = "linux")]
use super::host_context::probe;
use lens_model::StorageHealth;
#[cfg(target_os = "linux")]
use lens_model::{BlockDevice, StorageDetail};
#[cfg(any(target_os = "linux", test))]
use lens_model::{StorageMember, Timestamp};
#[cfg(any(target_os = "linux", test))]
use serde_json::Value;
#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
pub(super) fn collect(devices: &[BlockDevice], details: &mut BTreeMap<String, StorageDetail>) {
    #[cfg(target_os = "linux")]
    {
        static CACHE: Mutex<Option<(Instant, BTreeMap<String, StorageHealth>)>> = Mutex::new(None);
        let Ok(mut cache) = CACHE.lock() else {
            return;
        };
        if cache
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(60))
        {
            let start = Instant::now();
            let mut values = BTreeMap::new();
            // No discovery scan, self-tests, writes, privilege escalation, or sleeping-drive wakeups.
            for device in devices.iter().filter(|d| d.kind == "disk").take(32) {
                if start.elapsed() > Duration::from_secs(3) {
                    break;
                }
                if !device
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    continue;
                }
                let path = format!("/dev/{}", device.name);
                if let Some(health) = probe(
                    "smartctl",
                    &["-j", "-n", "standby", "-H", "-A", &path],
                    Duration::from_millis(800),
                )
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| parse_smart(&v))
                {
                    values.insert(device.name.clone(), health);
                }
            }
            // Dell/Broadcom tools expose member states hidden behind a hardware RAID volume.
            for tool in [
                "/opt/MegaRAID/perccli/perccli64",
                "/opt/MegaRAID/storcli/storcli64",
                "/usr/sbin/storcli",
                "/usr/sbin/perccli",
            ] {
                if !Path::new(tool).is_file() {
                    continue;
                }
                if let Some(output) = probe(
                    tool,
                    &["/call/vall", "show", "all", "J"],
                    Duration::from_secs(2),
                ) {
                    if let Ok(value) = serde_json::from_str::<Value>(&output) {
                        values.extend(parse_raid(&value));
                    }
                }
                break;
            }
            *cache = Some((Instant::now(), values));
        }
        if let Some((_, values)) = &*cache {
            for (name, health) in values {
                if let Some(detail) = details.get_mut(name) {
                    detail.health = Some(health.clone());
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (devices, details);
}

#[cfg(any(target_os = "linux", test))]
fn parse_smart(value: &Value) -> Option<StorageHealth> {
    let passed = value
        .pointer("/smart_status/passed")
        .and_then(Value::as_bool);
    let nvme = value.get("nvme_smart_health_information_log");
    if passed.is_none() && nvme.is_none() {
        return None;
    }
    Some(StorageHealth {
        source: "smartctl (SMART / NVMe)".into(),
        observed_at: Some(Timestamp::now()),
        smart_passed: passed,
        temperature_c: value
            .pointer("/temperature/current")
            .and_then(Value::as_i64),
        percentage_used: nvme
            .and_then(|v| v.get("percentage_used"))
            .and_then(Value::as_u64),
        media_errors: nvme
            .and_then(|v| v.get("media_errors"))
            .and_then(Value::as_u64),
        critical_warning: nvme
            .and_then(|v| v.get("critical_warning"))
            .and_then(Value::as_u64),
        ..StorageHealth::default()
    })
}

#[cfg(any(target_os = "linux", test))]
fn state(raw: &str) -> String {
    match raw {
        "Optl" => "Optimal",
        "Onln" => "Online",
        "Dgrd" | "Pdgd" => "Degraded",
        "Offln" => "Offline",
        "Rbld" => "Rebuilding",
        "Failed" | "UBad" => "Failed",
        "UGood" => "Unconfigured good",
        "GHS" => "Global hot spare",
        "DHS" => "Dedicated hot spare",
        _ => raw,
    }
    .into()
}

#[cfg(any(target_os = "linux", test))]
fn parse_raid(value: &Value) -> BTreeMap<String, StorageHealth> {
    let mut result = BTreeMap::new();
    for controller in value
        .get("Controllers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if controller
            .pointer("/Command Status/Status")
            .and_then(Value::as_str)
            != Some("Success")
        {
            continue;
        }
        let Some(data) = controller.get("Response Data").and_then(Value::as_object) else {
            continue;
        };
        for (key, properties) in data
            .iter()
            .filter(|(key, _)| key.starts_with("VD") && key.ends_with(" Properties"))
        {
            let Some(vd) = key
                .strip_prefix("VD")
                .and_then(|s| s.strip_suffix(" Properties"))
            else {
                continue;
            };
            let Some(name) = properties
                .get("OS Drive Name")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("/dev/"))
            else {
                continue;
            };
            if name.contains('/') {
                continue;
            }
            let volume = data
                .iter()
                .find(|(k, _)| k.starts_with("/c") && k.ends_with(&format!("/v{vd}")))
                .and_then(|(_, v)| v.as_array())
                .and_then(|a| a.first());
            let device_state = volume
                .and_then(|v| v.get("State"))
                .and_then(Value::as_str)
                .map(state);
            let members = data
                .get(&format!("PDs for VD {vd}"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|member| {
                    Some(StorageMember {
                        slot: member.get("EID:Slt")?.as_str()?.into(),
                        model: member
                            .get("Model")
                            .and_then(Value::as_str)
                            .map(|s| s.trim().into()),
                        state: state(member.get("State")?.as_str()?),
                    })
                })
                .collect();
            result.insert(
                name.into(),
                StorageHealth {
                    source: "PERC / StorCLI local RAID controller".into(),
                    observed_at: Some(Timestamp::now()),
                    device_state,
                    members,
                    ..StorageHealth::default()
                },
            );
        }
    }
    result
}

pub(super) fn summary(health: &StorageHealth) -> String {
    let mut parts = Vec::new();
    if let Some(state) = &health.device_state {
        parts.push(format!("RAID {state}"));
    }
    if let Some(passed) = health.smart_passed {
        parts.push(
            if passed {
                "SMART passed"
            } else {
                "SMART FAILED"
            }
            .into(),
        );
    }
    if let Some(warning) = health.critical_warning {
        if warning != 0 {
            parts.push(format!("NVMe warning 0x{warning:x}"));
        }
    }
    if let Some(used) = health.percentage_used {
        parts.push(format!("{used}% endurance used"));
    }
    if let Some(errors) = health.media_errors {
        parts.push(format!("{errors} media errors"));
    }
    if let Some(temp) = health.temperature_c {
        parts.push(format!("{temp} C"));
    }
    if !health.members.is_empty() {
        parts.push(
            health
                .members
                .iter()
                .map(|m| format!("slot {} {}", m.slot, m.state))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    parts.join(" | ")
}

/// Missing health is unknown. Only explicit bad readings generate a finding.
pub(super) fn severity(health: &StorageHealth) -> Option<lens_model::Severity> {
    use lens_model::Severity;
    let failed = |state: &str| matches!(state, "Failed" | "Offline");
    if health.smart_passed == Some(false)
        || health.device_state.as_deref().is_some_and(failed)
        || health.members.iter().any(|member| failed(&member.state))
    {
        return Some(Severity::Critical);
    }
    if health.critical_warning.is_some_and(|n| n != 0)
        || health.percentage_used.is_some_and(|n| n >= 100)
        || health.media_errors.is_some_and(|n| n > 0)
        || health
            .device_state
            .as_deref()
            .is_some_and(|s| matches!(s, "Degraded" | "Rebuilding"))
        || health
            .members
            .iter()
            .any(|member| member.state == "Rebuilding")
    {
        return Some(Severity::Attention);
    }
    None
}

pub(super) fn findings(snapshot: &lens_model::Snapshot) -> Vec<lens_model::Finding> {
    snapshot.storage_details.iter().filter_map(|(name, detail)| {
        let health = detail.health.as_ref()?;
        let severity = severity(health)?;
        Some(lens_model::Finding {
            id: format!("disk.health.{name}"), severity,
            title: format!("Storage health needs attention: {name}"),
            summary: summary(health),
            evidence: vec![lens_model::Evidence { label: "health source".into(), value: health.source.clone(), unit: None }],
            related_entities: vec![lens_model::EntityId::BlockDevice(name.clone())],
            suggested_actions: vec!["Review the device and RAID member details in Storage; confirm backups and investigate with the appropriate local storage utility.".into()],
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_and_unknown_smart_are_not_reported_healthy() {
        assert!(parse_smart(&serde_json::json!({"smartctl":{"exit_status":2}})).is_none());
        let health = parse_smart(&serde_json::json!({"smart_status":{"passed":false},"nvme_smart_health_information_log":{"critical_warning":4,"media_errors":8,"percentage_used":101}})).expect("fixture health");
        assert_eq!(health.smart_passed, Some(false));
        assert!(summary(&health).contains("FAILED"));
        assert!(summary(&health).contains("0x4"));
        assert_eq!(health.percentage_used, Some(101));
    }
    #[test]
    fn raid_mapping_uses_os_name_and_per_volume_members() {
        let value = serde_json::json!({"Controllers":[{"Command Status":{"Status":"Success"},"Response Data":{"/c0/v0":[{"State":"Dgrd"}],"VD0 Properties":{"OS Drive Name":"/dev/sda"},"PDs for VD 0":[{"EID:Slt":"32:0","Model":"Disk ","State":"Failed"},{"EID:Slt":"32:1","State":"Onln"}]}}]});
        let map = parse_raid(&value);
        let health = &map["sda"];
        assert_eq!(health.device_state.as_deref(), Some("Degraded"));
        assert_eq!(health.members[0].state, "Failed");
        assert_eq!(health.members[1].state, "Online");
        assert!(parse_raid(&serde_json::json!({"Controllers":[{"Command Status":{"Status":"Failure"},"Response Data":value["Controllers"][0]["Response Data"]}]})).is_empty());
    }
    #[test]
    fn only_explicit_storage_health_problems_raise_findings() {
        use lens_model::Severity;
        let mut health = StorageHealth::default();
        assert_eq!(severity(&health), None);
        health.smart_passed = Some(true);
        assert_eq!(severity(&health), None);
        health.smart_passed = Some(false);
        assert_eq!(severity(&health), Some(Severity::Critical));
        health.smart_passed = None;
        health.device_state = Some("Degraded".into());
        assert_eq!(severity(&health), Some(Severity::Attention));
        health.device_state = Some("Optimal".into());
        health.members.push(StorageMember {
            slot: "32:0".into(),
            model: None,
            state: "Failed".into(),
        });
        assert_eq!(severity(&health), Some(Severity::Critical));
    }
}
