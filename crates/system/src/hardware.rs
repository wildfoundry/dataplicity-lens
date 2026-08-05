use std::{fs, process::Command};

#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use lens_model::RaspberryPiStatus;
use lens_model::{HardwareDevice, HardwareIdentity, TemperatureSensor};

#[derive(Default)]
pub(crate) struct HardwareSnapshot {
    pub identity: HardwareIdentity,
    pub temperatures: Vec<TemperatureSensor>,
    pub devices: Vec<HardwareDevice>,
}

pub(crate) fn collect() -> HardwareSnapshot {
    #[cfg(target_os = "linux")]
    return collect_linux();
    #[cfg(target_os = "macos")]
    return collect_macos();
    #[allow(unreachable_code)]
    HardwareSnapshot::default()
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim_matches(['\0', '\n', '\r', ' ']).to_owned();
    (!value.is_empty()).then_some(value)
}

fn command_text(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(target_os = "linux")]
fn millidegrees(path: impl AsRef<Path>) -> Option<f64> {
    read_trimmed(path)?
        .parse::<f64>()
        .ok()
        .map(|value| value / 1000.0)
}

#[cfg(target_os = "linux")]
fn collect_linux() -> HardwareSnapshot {
    let model = read_trimmed("/sys/firmware/devicetree/base/model")
        .or_else(|| read_trimmed("/proc/device-tree/model"))
        .or_else(|| read_trimmed("/sys/class/dmi/id/product_name"));
    let cpuinfo = read_trimmed("/proc/cpuinfo").unwrap_or_default();
    let cpuinfo_value = |key: &str| {
        cpuinfo.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| value.trim().to_owned())
        })
    };
    let is_pi = model
        .as_deref()
        .is_some_and(|value| value.contains("Raspberry Pi"));
    let mut identity = HardwareIdentity {
        manufacturer: read_trimmed("/sys/class/dmi/id/sys_vendor")
            .or_else(|| is_pi.then(|| "Raspberry Pi Foundation".to_owned())),
        model,
        board: read_trimmed("/sys/class/dmi/id/board_name"),
        board_revision: read_trimmed("/sys/class/dmi/id/board_version")
            .or_else(|| cpuinfo_value("Revision")),
        serial_number: read_trimmed("/sys/class/dmi/id/product_serial")
            .or_else(|| cpuinfo_value("Serial")),
        firmware_version: read_trimmed("/sys/class/dmi/id/bios_version"),
        raspberry_pi: None,
    };
    let mut temperatures = linux_temperatures();
    if is_pi {
        identity.firmware_version =
            command_text("vcgencmd", &["version"]).or(identity.firmware_version);
        identity.raspberry_pi = Some(raspberry_pi_status());
        if let Some(value) = command_text("vcgencmd", &["measure_temp"])
            .and_then(|text| text.split('=').nth(1).map(str::to_owned))
            .and_then(|text| text.trim_end_matches("'C").parse::<f64>().ok())
        {
            if !temperatures.iter().any(|sensor| sensor.name == "SoC") {
                temperatures.push(TemperatureSensor {
                    name: "SoC".into(),
                    source: "vcgencmd".into(),
                    temperature_c: value,
                    max_c: Some(80.0),
                    critical_c: Some(85.0),
                });
            }
        }
    }
    temperatures.sort_by(|left, right| left.name.cmp(&right.name));
    HardwareSnapshot {
        identity,
        temperatures,
        devices: linux_devices(),
    }
}

#[cfg(target_os = "linux")]
fn raspberry_pi_status() -> RaspberryPiStatus {
    let raw = command_text("vcgencmd", &["get_throttled"])
        .and_then(|text| text.split('=').nth(1).map(str::to_owned))
        .and_then(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16).ok());
    let (active_conditions, historical_conditions) =
        raw.map_or_else(|| (Vec::new(), Vec::new()), decode_throttled);
    RaspberryPiStatus {
        throttled_raw: raw,
        active_conditions,
        historical_conditions,
    }
}

#[cfg(target_os = "linux")]
fn decode_throttled(raw: u32) -> (Vec<String>, Vec<String>) {
    const CONDITIONS: [(u32, &str); 4] = [
        (0, "under-voltage"),
        (1, "frequency capped"),
        (2, "throttled"),
        (3, "soft temperature limit"),
    ];
    let active = CONDITIONS
        .iter()
        .filter(|(bit, _)| raw & (1 << bit) != 0)
        .map(|(_, label)| (*label).to_owned())
        .collect();
    let historical = CONDITIONS
        .iter()
        .filter(|(bit, _)| raw & (1 << (bit + 16)) != 0)
        .map(|(_, label)| format!("{label} occurred"))
        .collect();
    (active, historical)
}

#[cfg(target_os = "linux")]
fn linux_temperatures() -> Vec<TemperatureSensor> {
    let mut sensors = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("thermal_zone")
            {
                continue;
            }
            let Some(temperature_c) = millidegrees(path.join("temp")) else {
                continue;
            };
            sensors.push(TemperatureSensor {
                name: read_trimmed(path.join("type")).unwrap_or_else(|| "thermal zone".into()),
                source: path.display().to_string(),
                temperature_c,
                max_c: None,
                critical_c: None,
            });
        }
    }
    if let Ok(chips) = fs::read_dir("/sys/class/hwmon") {
        for chip in chips.flatten() {
            let chip_path = chip.path();
            let chip_name = read_trimmed(chip_path.join("name")).unwrap_or_else(|| "hwmon".into());
            let Ok(entries) = fs::read_dir(&chip_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().into_owned();
                let Some(stem) = filename.strip_suffix("_input") else {
                    continue;
                };
                if !stem.starts_with("temp") {
                    continue;
                }
                let Some(temperature_c) = millidegrees(entry.path()) else {
                    continue;
                };
                let label = read_trimmed(chip_path.join(format!("{stem}_label")))
                    .unwrap_or_else(|| stem.to_owned());
                sensors.push(TemperatureSensor {
                    name: format!("{chip_name} {label}"),
                    source: entry.path().display().to_string(),
                    temperature_c,
                    max_c: millidegrees(chip_path.join(format!("{stem}_max"))),
                    critical_c: millidegrees(chip_path.join(format!("{stem}_crit"))),
                });
            }
        }
    }
    sensors
}

#[cfg(target_os = "linux")]
fn linux_devices() -> Vec<HardwareDevice> {
    let mut devices = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") {
        for entry in entries.flatten() {
            let path = entry.path();
            let (Some(vendor_id), Some(product_id)) = (
                read_trimmed(path.join("idVendor")),
                read_trimmed(path.join("idProduct")),
            ) else {
                continue;
            };
            let manufacturer = read_trimmed(path.join("manufacturer"));
            let product = read_trimmed(path.join("product"));
            devices.push(HardwareDevice {
                kind: "usb".into(),
                name: product
                    .clone()
                    .unwrap_or_else(|| format!("{vendor_id}:{product_id}")),
                path: path.display().to_string(),
                manufacturer,
                vendor_id: Some(vendor_id),
                product_id: Some(product_id),
                serial_number: read_trimmed(path.join("serial")),
            });
        }
    }
    let mut paths = HashSet::new();
    if let Ok(entries) = fs::read_dir("/dev/serial/by-id") {
        for entry in entries.flatten() {
            let path = entry.path();
            paths.insert(path.display().to_string());
        }
    }
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ["ttyUSB", "ttyACM", "ttyAMA", "rfcomm"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                paths.insert(entry.path().display().to_string());
            }
        }
    }
    devices.extend(paths.into_iter().map(|path| {
        HardwareDevice {
            kind: "serial".into(),
            name: Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
            path,
            manufacturer: None,
            vendor_id: None,
            product_id: None,
            serial_number: None,
        }
    }));
    devices.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.name.cmp(&right.name)));
    devices
}

#[cfg(target_os = "macos")]
fn collect_macos() -> HardwareSnapshot {
    let hardware = system_profiler("SPHardwareDataType");
    let identity = HardwareIdentity {
        manufacturer: Some("Apple".into()),
        model: json_string(&hardware, &["machine_name", "machine_model"]),
        board: json_string(&hardware, &["chip_type", "cpu_type"]),
        board_revision: None,
        serial_number: json_string(&hardware, &["serial_number"]),
        firmware_version: json_string(&hardware, &["system_firmware_version", "boot_rom_version"]),
        raspberry_pi: None,
    };
    let mut devices = Vec::new();
    if let Some(usb) = system_profiler("SPUSBDataType") {
        collect_macos_usb(&usb, &mut devices);
    }
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("cu.") {
                devices.push(HardwareDevice {
                    kind: "serial".into(),
                    name,
                    path: entry.path().display().to_string(),
                    manufacturer: None,
                    vendor_id: None,
                    product_id: None,
                    serial_number: None,
                });
            }
        }
    }
    devices.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.name.cmp(&right.name)));
    HardwareSnapshot {
        identity,
        temperatures: Vec::new(),
        devices,
    }
}

#[cfg(target_os = "macos")]
fn system_profiler(data_type: &str) -> Option<serde_json::Value> {
    command_text("system_profiler", &[data_type, "-json"])
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[cfg(target_os = "macos")]
fn json_string(value: &Option<serde_json::Value>, keys: &[&str]) -> Option<String> {
    fn find(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
        match value {
            serde_json::Value::Object(object) => keys
                .iter()
                .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .or_else(|| object.values().find_map(|value| find(value, keys))),
            serde_json::Value::Array(values) => values.iter().find_map(|value| find(value, keys)),
            _ => None,
        }
    }
    value.as_ref().and_then(|value| find(value, keys))
}

#[cfg(target_os = "macos")]
fn collect_macos_usb(value: &serde_json::Value, devices: &mut Vec<HardwareDevice>) {
    match value {
        serde_json::Value::Object(object) => {
            if let (Some(name), Some(vendor_id), Some(product_id)) = (
                object.get("_name").and_then(serde_json::Value::as_str),
                object.get("vendor_id").and_then(serde_json::Value::as_str),
                object.get("product_id").and_then(serde_json::Value::as_str),
            ) {
                devices.push(HardwareDevice {
                    kind: "usb".into(),
                    name: name.to_owned(),
                    path: object
                        .get("location_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(name)
                        .to_owned(),
                    manufacturer: object
                        .get("manufacturer")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    vendor_id: Some(vendor_id.to_owned()),
                    product_id: Some(product_id.to_owned()),
                    serial_number: object
                        .get("serial_num")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
            for child in object.values() {
                collect_macos_usb(child, devices);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_macos_usb(child, devices);
            }
        }
        _ => {}
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::decode_throttled;

    #[test]
    fn decodes_current_and_historical_pi_flags() {
        let (active, historical) = decode_throttled((1 << 0) | (1 << 18));
        assert_eq!(active, ["under-voltage"]);
        assert_eq!(historical, ["throttled occurred"]);
    }
}
