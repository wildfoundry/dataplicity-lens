//! Credential-free, read-only whole-host BMC power measurements.
use lens_model::PowerReading;
#[cfg(any(target_os = "linux", test))]
use lens_model::Timestamp;
#[cfg(target_os = "linux")]
use std::{
    fs,
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) fn collect() -> Option<PowerReading> {
    #[cfg(target_os = "linux")]
    {
        static CACHE: Mutex<Option<(Instant, Option<PowerReading>)>> = Mutex::new(None);
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
                super::host_context::probe_success(
                    "ipmitool",
                    &["-I", "open", "dcmi", "power", "reading"],
                    Duration::from_millis(1500),
                )
                .and_then(|text| parse_dcmi(&text, Timestamp::now()))
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
fn read_published() -> Option<PowerReading> {
    use std::os::unix::fs::MetadataExt;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    let directory = std::path::Path::new("/run/lens-power");
    let info = fs::symlink_metadata(directory).ok()?;
    if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
        return None;
    }
    let path = directory.join("current.json");
    let info = fs::symlink_metadata(&path).ok()?;
    if !info.is_file() || info.uid() != 0 || info.mode() & 0o022 != 0 || info.len() > 4096 {
        return None;
    }
    let value: PowerReading = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let timestamp = OffsetDateTime::parse(&value.observed_at.0, &Rfc3339).ok()?;
    let age = (OffsetDateTime::now_utc() - timestamp).whole_seconds();
    ((-5..=30).contains(&age)
        && (1..=50_000).contains(&value.watts)
        && value.source == "local_bmc_dcmi")
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
        let value = super::host_context::probe_success(
            "ipmitool",
            &["-I", "open", "dcmi", "power", "reading"],
            Duration::from_millis(1500),
        )
        .and_then(|text| parse_dcmi(&text, Timestamp::now()));
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
