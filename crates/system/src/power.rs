//! Credential-free, read-only whole-host BMC power measurements.
use lens_model::{PowerReading, PowerSupplyHealth, PowerSupplyReading};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PowerSample {
    pub observed_at: lens_model::Timestamp,
    pub power: Option<PowerReading>,
    pub supplies: Vec<PowerSupplyReading>,
}
#[cfg(any(target_os = "linux", test))]
use lens_model::Timestamp;
#[cfg(target_os = "linux")]
use std::{
    fs,
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) fn collect() -> Option<PowerSample> {
    #[cfg(target_os = "linux")]
    {
        static CACHE: Mutex<Option<(Instant, Option<PowerSample>)>> = Mutex::new(None);
        if let Some(reading) = read_published() {
            return Some(reading);
        }
        let mut cache = CACHE.lock().ok()?;
        if let Some((at, reading)) = &*cache {
            if at.elapsed() < Duration::from_secs(10) {
                return reading.clone();
            }
        }
        let value =
            if fs::read_dir("/sys/class/ipmi").is_ok_and(|mut entries| entries.next().is_some()) {
                Some(sample_local())
            } else {
                None
            };
        *cache = Some((Instant::now(), value.clone()));
        return value;
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(any(target_os = "linux", test))]
fn parse_dcmi(text: &str, observed_at: Timestamp) -> Option<PowerReading> {
    let mut watts = None;
    let mut active = false;
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("Power reading state is") {
            active = value.trim().eq_ignore_ascii_case("activated");
        }
        if name
            .trim()
            .eq_ignore_ascii_case("Instantaneous power reading")
        {
            // Duplicate/ambiguous sensor values are not a whole-host reading.
            if watts.is_some() {
                return None;
            }
            let mut fields = value.split_whitespace();
            let value = fields.next()?.parse::<u32>().ok()?;
            if !fields.next()?.eq_ignore_ascii_case("Watts")
                || fields.next().is_some()
                || !(1..=50_000).contains(&value)
            {
                return None;
            }
            watts = Some(value);
        }
    }
    if !active {
        return None;
    }
    Some(PowerReading {
        watts: watts?,
        observed_at,
        source: "local_bmc_dcmi".into(),
    })
}

#[cfg(target_os = "linux")]
fn sample_local() -> PowerSample {
    let total = super::host_context::probe_success(
        "ipmitool",
        &["-I", "open", "dcmi", "power", "reading"],
        Duration::from_millis(1500),
    );
    // Entity 10 is IPMI's Power Supply entity. Discover its instances rather than
    // guessing slots from names or assuming that every server has two supplies.
    let supplies = super::host_context::probe_success(
        "ipmitool",
        &["-I", "open", "sdr", "entity", "10"],
        Duration::from_secs(3),
    );
    let observed_at = Timestamp::now();
    PowerSample {
        power: total.and_then(|text| parse_dcmi(&text, observed_at.clone())),
        supplies: supplies
            .map(|text| parse_supplies(&text, observed_at.clone()))
            .unwrap_or_default(),
        observed_at,
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_supplies(text: &str, observed_at: Timestamp) -> Vec<PowerSupplyReading> {
    use std::collections::BTreeMap;
    let mut supplies = BTreeMap::<u8, PowerSupplyReading>::new();
    for line in text.lines().take(512) {
        let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let Some(instance) = fields[3]
            .strip_prefix("10.")
            .and_then(|v| v.parse::<u8>().ok())
        else {
            continue;
        };
        if !supplies.contains_key(&instance) && supplies.len() >= 32 {
            continue;
        }
        let supply = supplies
            .entry(instance)
            .or_insert_with(|| PowerSupplyReading {
                id: format!("ipmi:10.{instance}"),
                name: format!("Power supply {instance}"),
                present: None,
                health: PowerSupplyHealth::Unknown,
                states: Vec::new(),
                watts: None,
                input_watts: None,
                output_watts: None,
                current_milliamps: None,
                voltage_millivolts: None,
                observed_at: observed_at.clone(),
                source: "local_bmc_ipmi".into(),
            });
        let name = fields[0];
        let state = fields[4];
        if state.starts_with("Logical FRU @")
            && !name.is_empty()
            && name.len() <= 64
            && !name.chars().any(char::is_control)
        {
            supply.name = name.to_owned();
            continue;
        }
        // ipmitool may print "ok" even for discrete AC-loss/failure events.
        // Interpret the actual state, never infer health from the column alone.
        for event in state.split(',').map(str::trim) {
            let (label, health) = match event.to_ascii_lowercase().as_str() {
                "present" | "device present" => {
                    supply.present = Some(true);
                    ("Present", PowerSupplyHealth::Unknown)
                }
                "presence detected" => {
                    supply.present = Some(true);
                    (
                        "Present",
                        if fields[2] == "ok" {
                            PowerSupplyHealth::Healthy
                        } else {
                            PowerSupplyHealth::Unknown
                        },
                    )
                }
                "absent" | "device absent" | "not present" => {
                    supply.present = Some(false);
                    ("Absent", PowerSupplyHealth::Absent)
                }
                "failure detected" => ("Failure detected", PowerSupplyHealth::Critical),
                "power supply ac lost" | "ac lost" => ("AC lost", PowerSupplyHealth::Critical),
                "ac lost or out-of-range" => {
                    ("AC lost or out of range", PowerSupplyHealth::Critical)
                }
                "predictive failure" => ("Predictive failure", PowerSupplyHealth::Warning),
                "ac out-of-range" => ("AC out of range", PowerSupplyHealth::Warning),
                "power supply inactive" => ("Inactive", PowerSupplyHealth::Unknown),
                _ if event.to_ascii_lowercase().starts_with("config error") => {
                    ("Configuration error", PowerSupplyHealth::Critical)
                }
                _ => continue,
            };
            merge_health(&mut supply.health, health);
            if !supply.states.iter().any(|v| v == label) && supply.states.len() < 8 {
                supply.states.push(label.into());
            }
        }
        match fields[2].to_ascii_lowercase().as_str() {
            "nc" => merge_health(&mut supply.health, PowerSupplyHealth::Warning),
            "cr" | "nr" => merge_health(&mut supply.health, PowerSupplyHealth::Critical),
            "ok" => {}
            _ => continue,
        }
        let Some((number, unit)) = state.split_once(' ') else {
            continue;
        };
        let Ok(number) = number.parse::<f64>() else {
            continue;
        };
        if !number.is_finite() || number < 0.0 {
            continue;
        }
        match unit {
            "Watts" if number <= 50_000.0 && number.fract() == 0.0 => {
                let name = name.to_ascii_lowercase();
                let field = if name.contains("input") {
                    &mut supply.input_watts
                } else if name.contains("output") {
                    &mut supply.output_watts
                } else {
                    &mut supply.watts
                };
                // Multiple different sensors must not silently overwrite each other.
                if field.is_none() {
                    *field = Some(number as u32);
                }
            }
            "Amps" if number <= 1000.0 => {
                supply.current_milliamps = Some((number * 1000.0).round() as u32)
            }
            "Volts" if number <= 1000.0 => {
                supply.voltage_millivolts = Some((number * 1000.0).round() as u32)
            }
            _ => {}
        }
    }
    supplies.into_values().collect()
}

#[cfg(any(target_os = "linux", test))]
fn merge_health(current: &mut PowerSupplyHealth, new: PowerSupplyHealth) {
    let severity = |s| match s {
        PowerSupplyHealth::Unknown => 0,
        PowerSupplyHealth::Healthy => 1,
        PowerSupplyHealth::Absent => 2,
        PowerSupplyHealth::Warning => 3,
        PowerSupplyHealth::Critical => 4,
    };
    if severity(new) > severity(*current) {
        *current = new;
    }
}

#[cfg(target_os = "linux")]
fn read_published() -> Option<PowerSample> {
    use std::os::unix::fs::MetadataExt;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let directory = std::path::Path::new("/run/lens-power");
    let info = fs::symlink_metadata(directory).ok()?;
    if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
        return None;
    }
    let path = directory.join("current.json");
    let info = fs::symlink_metadata(&path).ok()?;
    if !info.is_file() || info.uid() != 0 || info.mode() & 0o022 != 0 || info.len() > 32_768 {
        return None;
    }
    let value: PowerSample = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let timestamp = OffsetDateTime::parse(&value.observed_at.0, &Rfc3339).ok()?;
    let age = (OffsetDateTime::now_utc() - timestamp).whole_seconds();
    let mut ids = std::collections::BTreeSet::new();
    ((-5..=30).contains(&age)
        && value.power.as_ref().is_none_or(|p| {
            (1..=50_000).contains(&p.watts)
                && p.source == "local_bmc_dcmi"
                && p.observed_at == value.observed_at
        })
        && value.supplies.len() <= 32
        && value.supplies.iter().all(|p| {
            ids.insert(&p.id)
                && p.source == "local_bmc_ipmi"
                && p.observed_at == value.observed_at
                && p.id.len() <= 16
                && p.name.len() <= 64
                && !p.name.chars().any(char::is_control)
                && p.states.len() <= 8
                && p.states
                    .iter()
                    .all(|s| s.len() <= 64 && !s.chars().any(char::is_control))
                && [p.watts, p.input_watts, p.output_watts]
                    .iter()
                    .all(|v| v.is_none_or(|v| v <= 50_000))
                && p.current_milliamps.is_none_or(|v| v <= 1_000_000)
                && p.voltage_millivolts.is_none_or(|v| v <= 1_000_000)
        }))
    .then_some(value)
}

pub(super) fn write_sample() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::{
            io::Write,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let directory = std::path::Path::new("/run/lens-power");
        let info = fs::symlink_metadata(directory)?;
        anyhow::ensure!(
            info.is_dir() && info.uid() == 0 && info.mode() & 0o022 == 0,
            "power sample directory must be root-owned and protected"
        );
        let value = sample_local();
        let temporary = directory.join(format!(".sample-{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&serde_json::to_vec(&value)?)?;
            file.sync_all()?;
            fs::rename(&temporary, directory.join("current.json"))?;
            fs::File::open(directory)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(temporary);
        result
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("local BMC power sampling requires Linux")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_single_dual_and_many_supplies_without_fabricating_watts() {
        let now = Timestamp::now();
        let single = "PS1 | 00h | ns | 10.1 | Logical FRU @01h\nStatus | 62h | ok | 10.1 | Presence detected\nCurrent 1 | 6Ah | ok | 10.1 | 0.40 Amps\nVoltage 1 | 6Ch | ok | 10.1 | 236 Volts\nPwr Consumption | 77h | ok | 7.1 | 140 Watts\n";
        let supply = parse_supplies(single, now.clone());
        assert_eq!(supply.len(), 1);
        assert_eq!(supply[0].name, "PS1");
        assert_eq!(supply[0].health, PowerSupplyHealth::Healthy);
        assert_eq!(supply[0].current_milliamps, Some(400));
        assert_eq!(supply[0].voltage_millivolts, Some(236_000));
        assert_eq!(supply[0].watts, None);
        let dual = format!(
            "{single}Status | 63h | ok | 10.2 | Presence detected, Power Supply AC lost\nCurrent 2 | 6Bh | ok | 10.2 | 0 Amps\n"
        );
        let supplies = parse_supplies(&dual, now.clone());
        assert_eq!(supplies.len(), 2);
        assert_eq!(supplies[1].health, PowerSupplyHealth::Critical);
        assert_eq!(supplies[1].current_milliamps, Some(0));
        let many = (1..=40).map(|n| format!("Status | 62h | ok | 10.{n} | Presence detected\nInput | 63h | ok | 10.{n} | 100 Watts\n")).collect::<String>();
        let supplies = parse_supplies(&many, now);
        assert_eq!(supplies.len(), 32);
        assert!(supplies.iter().all(|s| s.input_watts == Some(100)));
    }

    #[test]
    fn absent_unknown_and_invalid_sensors_do_not_become_healthy_or_zero() {
        let supply = parse_supplies(
            "Presence | 42h | ok | 10.1 | Absent\nCurrent | 6Ah | ns | 10.1 | No Reading\nVoltage | 6Ch | ok | 10.1 | NaN Volts\nPower | 6Dh | ok | 10.1 | -1 Watts\n",
            Timestamp::now(),
        );
        assert_eq!(supply[0].health, PowerSupplyHealth::Absent);
        assert_eq!(supply[0].present, Some(false));
        assert_eq!(supply[0].watts, None);
        assert_eq!(supply[0].current_milliamps, None);
        assert_eq!(supply[0].voltage_millivolts, None);
        let supply = parse_supplies(
            "PS4 | 00h | ns | 10.4 | Logical FRU @04h\n",
            Timestamp::now(),
        );
        assert_eq!(supply[0].health, PowerSupplyHealth::Unknown);
        assert_eq!(supply[0].present, None);
    }
    #[test]
    fn only_an_active_whole_host_reading_is_retained() {
        let now = Timestamp("2026-11-11T00:00:00Z".into());
        let input = "Instantaneous power reading: 182 Watts\nMinimum during sampling period: 120 Watts\nAverage power reading: 150 Watts\nPower reading state is: activated\n";
        let value = parse_dcmi(input, now.clone()).expect("active DCMI reading");
        assert_eq!(value.watts, 182);
        assert_eq!(value.observed_at, now);
        assert_eq!(value.source, "local_bmc_dcmi");
        assert!(parse_dcmi(&input.replace("activated", "deactivated"), now.clone()).is_none());
        for invalid in [
            "na",
            "-1",
            "NaN",
            "65535",
            "0",
            "182 volts",
            "182 Watts extra",
        ] {
            assert!(parse_dcmi(&input.replace("182 Watts", invalid), now.clone()).is_none());
        }
        assert!(
            parse_dcmi(
                "Average power reading: 150 Watts\nPower reading state is: activated",
                now
            )
            .is_none()
        );
    }
}
