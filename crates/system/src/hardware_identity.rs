//! Local firmware identity only: no subprocesses, credentials or privilege escalation.

use std::io;

use lens_model::{HardwareIdentity, HardwareIdentityUnavailableReason as Reason};

#[cfg(target_os = "linux")]
pub(crate) fn collect() -> HardwareIdentity {
    use std::{fs::File, io::Read, sync::OnceLock};

    // Firmware identity is stable for this process. Access/firmware changes take effect on restart.
    static IDENTITY: OnceLock<HardwareIdentity> = OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            collect_with(|path| {
                const MAX_BYTES: u64 = 1024 * 1024;
                let mut text = String::new();
                File::open(path)?
                    .take(MAX_BYTES + 1)
                    .read_to_string(&mut text)?;
                if text.len() as u64 > MAX_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "identity input too large",
                    ));
                }
                Ok(text)
            })
        })
        .clone()
}

fn collect_with(mut read: impl FnMut(&str) -> io::Result<String>) -> HardwareIdentity {
    let mut identity = HardwareIdentity::default();
    identity.manufacturer = field(
        &mut read,
        &mut identity,
        "manufacturer",
        &["/sys/class/dmi/id/sys_vendor"],
    );
    identity.model = field(
        &mut read,
        &mut identity,
        "model",
        &[
            "/sys/firmware/devicetree/base/model",
            "/proc/device-tree/model",
            "/sys/class/dmi/id/product_name",
        ],
    );
    identity.board = field(
        &mut read,
        &mut identity,
        "board",
        &["/sys/class/dmi/id/board_name"],
    );
    identity.board_revision = field(
        &mut read,
        &mut identity,
        "board_revision",
        &["/sys/class/dmi/id/board_version", "/proc/cpuinfo"],
    );
    identity.serial_number = field(
        &mut read,
        &mut identity,
        "serial_number",
        &[
            "/sys/class/dmi/id/product_serial",
            "/sys/firmware/devicetree/base/serial-number",
            "/proc/device-tree/serial-number",
            "/sys/firmware/devicetree/base/system-id",
            "/proc/device-tree/system-id",
            "/proc/cpuinfo",
        ],
    );
    identity.chassis_serial_number = field(
        &mut read,
        &mut identity,
        "chassis_serial_number",
        &["/sys/class/dmi/id/chassis_serial"],
    );
    identity.board_serial_number = field(
        &mut read,
        &mut identity,
        "board_serial_number",
        &["/sys/class/dmi/id/board_serial"],
    );
    identity.asset_tag = field(
        &mut read,
        &mut identity,
        "asset_tag",
        &[
            "/sys/class/dmi/id/chassis_asset_tag",
            "/sys/class/dmi/id/board_asset_tag",
        ],
    );
    identity.firmware_version = field(
        &mut read,
        &mut identity,
        "firmware_version",
        &["/sys/class/dmi/id/bios_version"],
    );
    if identity.manufacturer.is_none()
        && identity
            .model
            .as_deref()
            .is_some_and(|model| model.contains("Raspberry Pi"))
    {
        identity.manufacturer = Some("Raspberry Pi Foundation".into());
        identity.unavailable_fields.remove("manufacturer");
        identity
            .identity_sources
            .insert("manufacturer".into(), "device-tree model".into());
    }
    if identity.manufacturer.as_deref().is_some_and(|vendor| {
        vendor
            .split_whitespace()
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("Dell"))
    }) {
        identity.service_tag = identity.serial_number.clone();
        if let Some(source) = identity.identity_sources.get("serial_number").cloned() {
            identity
                .identity_sources
                .insert("service_tag".into(), source);
        } else if let Some(reason) = identity.unavailable_fields.get("serial_number").copied() {
            identity
                .unavailable_fields
                .insert("service_tag".into(), reason);
        }
    }
    identity
}

fn field(
    read: &mut impl FnMut(&str) -> io::Result<String>,
    identity: &mut HardwareIdentity,
    name: &str,
    paths: &[&str],
) -> Option<String> {
    let mut unavailable = Reason::NotReported;
    for path in paths {
        let result = read(path).and_then(|text| {
            let text = if *path == "/proc/cpuinfo" {
                let key = if name == "board_revision" {
                    "Revision"
                } else {
                    "Serial"
                };
                text.lines()
                    .find_map(|line| {
                        let (label, value) = line.split_once(':')?;
                        label
                            .trim()
                            .eq_ignore_ascii_case(key)
                            .then(|| value.to_owned())
                    })
                    .unwrap_or_default()
            } else {
                text
            };
            clean(&text, name)
        });
        match result {
            Ok(value) => {
                let source = if *path == "/proc/cpuinfo" {
                    format!(
                        "{path}:{}",
                        if name == "board_revision" {
                            "Revision"
                        } else {
                            "Serial"
                        }
                    )
                } else {
                    (*path).to_owned()
                };
                identity.identity_sources.insert(name.to_owned(), source);
                return Some(value);
            }
            Err(error) => {
                let reason = match error.kind() {
                    io::ErrorKind::PermissionDenied => Reason::PermissionDenied,
                    io::ErrorKind::NotFound => Reason::NotReported,
                    io::ErrorKind::InvalidData => Reason::Invalid,
                    _ => Reason::ReadError,
                };
                if priority(reason) > priority(unavailable) {
                    unavailable = reason;
                }
            }
        }
    }
    identity
        .unavailable_fields
        .insert(name.to_owned(), unavailable);
    None
}

fn priority(reason: Reason) -> u8 {
    match reason {
        Reason::NotReported => 0,
        Reason::Invalid => 1,
        Reason::ReadError => 2,
        Reason::PermissionDenied => 3,
    }
}

fn clean(text: &str, field: &str) -> io::Result<String> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if text.is_empty() {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let placeholder = matches!(
        text.to_ascii_lowercase().as_str(),
        "unknown"
            | "none"
            | "not specified"
            | "not available"
            | "default string"
            | "to be filled by o.e.m."
            | "to be filled by o.e.m"
            | "to be filled by oem"
            | "system serial number"
    );
    if placeholder
        || ((field.contains("serial") || field == "asset_tag")
            && text
                .chars()
                .all(|c| c == '0' || c == '-' || c.is_whitespace()))
        || text.chars().any(char::is_control)
    {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(entries: &[(&str, &str)]) -> HardwareIdentity {
        collect_with(|path| {
            entries
                .iter()
                .find(|(key, _)| *key == path)
                .map(|(_, value)| (*value).to_owned())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        })
    }

    #[test]
    fn server_vendors_share_system_identity_but_keep_component_serials_separate() {
        for vendor in ["Dell Inc.", "HPE", "IBM", "Cisco Systems Inc.", "LENOVO"] {
            let identity = fixture(&[
                ("/sys/class/dmi/id/sys_vendor", vendor),
                ("/sys/class/dmi/id/product_serial", "EXAMPLE-SYSTEM\n"),
                ("/sys/class/dmi/id/chassis_serial", "EXAMPLE-CHASSIS"),
                ("/sys/class/dmi/id/board_serial", "EXAMPLE-BOARD"),
                ("/sys/class/dmi/id/chassis_asset_tag", "EXAMPLE-ASSET"),
            ]);
            assert_eq!(identity.serial_number.as_deref(), Some("EXAMPLE-SYSTEM"));
            assert_eq!(
                identity.chassis_serial_number.as_deref(),
                Some("EXAMPLE-CHASSIS")
            );
            assert_eq!(
                identity.board_serial_number.as_deref(),
                Some("EXAMPLE-BOARD")
            );
            assert_eq!(identity.asset_tag.as_deref(), Some("EXAMPLE-ASSET"));
            assert_eq!(
                identity.service_tag.as_deref(),
                (vendor == "Dell Inc.").then_some("EXAMPLE-SYSTEM")
            );
            assert_eq!(
                identity.identity_sources["serial_number"],
                "/sys/class/dmi/id/product_serial"
            );
        }
    }

    #[test]
    fn non_root_permission_errors_remain_explicit_without_component_substitution() {
        let identity = collect_with(|path| match path {
            "/sys/class/dmi/id/product_serial" => {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
            "/sys/class/dmi/id/chassis_serial" => Ok("EXAMPLE-CHASSIS".into()),
            "/sys/class/dmi/id/board_serial" => Ok("EXAMPLE-BOARD".into()),
            "/sys/class/dmi/id/sys_vendor" => Ok("Dell Inc.".into()),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        });
        assert!(identity.serial_number.is_none());
        assert!(identity.service_tag.is_none());
        assert_eq!(
            identity.unavailable_fields["serial_number"],
            Reason::PermissionDenied
        );
        assert_eq!(
            identity.unavailable_fields["service_tag"],
            Reason::PermissionDenied
        );
        assert!(identity.chassis_serial_number.is_some());
    }

    #[test]
    fn device_tree_supports_power_and_arm_without_dmidecode() {
        let power = fixture(&[("/proc/device-tree/system-id", "EXAMPLE-POWER\0")]);
        assert_eq!(power.serial_number.as_deref(), Some("EXAMPLE-POWER"));
        assert_eq!(
            power.identity_sources["serial_number"],
            "/proc/device-tree/system-id"
        );
        let arm = fixture(&[
            (
                "/sys/firmware/devicetree/base/model",
                "Raspberry Pi Example\0",
            ),
            (
                "/sys/firmware/devicetree/base/serial-number",
                "10000000abcdef01\0",
            ),
        ]);
        assert_eq!(arm.manufacturer.as_deref(), Some("Raspberry Pi Foundation"));
        assert_eq!(arm.serial_number.as_deref(), Some("10000000abcdef01"));
        let legacy = fixture(&[(
            "/proc/cpuinfo",
            "Revision : example\nSerial : 10000000abcdef02\n",
        )]);
        assert_eq!(legacy.serial_number.as_deref(), Some("10000000abcdef02"));
        assert_eq!(legacy.board_revision.as_deref(), Some("example"));
    }

    #[test]
    fn placeholders_and_empty_data_are_not_usable_identity() {
        for placeholder in [
            "To Be Filled By O.E.M.",
            "Default String",
            "Unknown",
            "0000000",
            "bad\nvalue",
        ] {
            let identity = fixture(&[("/sys/class/dmi/id/product_serial", placeholder)]);
            assert!(identity.serial_number.is_none());
            assert_eq!(
                identity.unavailable_fields["serial_number"],
                Reason::Invalid
            );
        }
        let absent = fixture(&[("/sys/class/dmi/id/product_serial", "\0\n")]);
        assert_eq!(
            absent.unavailable_fields["serial_number"],
            Reason::NotReported
        );
        let revision = fixture(&[("/sys/class/dmi/id/board_version", "0")]);
        assert_eq!(revision.board_revision.as_deref(), Some("0"));
    }

    #[test]
    fn successful_fallback_has_its_own_source_and_io_errors_are_distinct() {
        let identity = fixture(&[
            ("/sys/class/dmi/id/product_serial", "Default String"),
            ("/proc/device-tree/serial-number", "EXAMPLE-DT"),
        ]);
        assert_eq!(identity.serial_number.as_deref(), Some("EXAMPLE-DT"));
        assert!(!identity.unavailable_fields.contains_key("serial_number"));
        assert_eq!(
            identity.identity_sources["serial_number"],
            "/proc/device-tree/serial-number"
        );
        let broken = collect_with(|_| Err(io::Error::from(io::ErrorKind::Other)));
        assert_eq!(
            broken.unavailable_fields["serial_number"],
            Reason::ReadError
        );
    }
}
