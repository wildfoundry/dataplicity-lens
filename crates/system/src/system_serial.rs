//! Private, observational system-serial fallback. Never an authority or host pin.
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
pub(super) const SAVED_SOURCE: &str = "local cached DMI system serial";
#[cfg(target_os = "linux")]
pub(super) const DMI_PATH: &str = "/sys/class/dmi/id/product_serial";
#[cfg(target_os = "linux")]
const DIRECTORY: &str = "/var/lib/lens-collector";
const FILE: &str = "system-serial.json";
const MAX_BYTES: u64 = 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema_version: u8,
    machine_id: String,
    product_serial: String,
}

struct Cache {
    directory: PathBuf,
    owner: u32,
    machine_id: String,
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

pub(super) fn clean(value: &str) -> io::Result<String> {
    let value = super::hardware_identity::clean(value, "serial_number")?;
    if value.len() > 256 {
        return Err(invalid());
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
fn read_text(path: &Path) -> io::Result<String> {
    let mut value = String::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut value)?;
    if value.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    Ok(value)
}

fn valid_machine_id(value: &str) -> bool {
    value.len() == 32
        && value.bytes().all(|b| b.is_ascii_hexdigit())
        && value.bytes().any(|b| b != b'0')
}

impl Cache {
    #[cfg(target_os = "linux")]
    fn local() -> io::Result<Self> {
        let machine_id = read_text(Path::new("/etc/machine-id"))?.trim().to_owned();
        if !valid_machine_id(&machine_id) {
            return Err(invalid());
        }
        Ok(Self {
            directory: PathBuf::from(DIRECTORY),
            owner: fs::metadata("/proc/self")?.uid(),
            machine_id,
        })
    }

    fn directory(&self) -> io::Result<()> {
        let info = fs::symlink_metadata(&self.directory)?;
        if !info.is_dir() || info.uid() != self.owner || info.mode() & 0o777 != 0o700 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }

    fn bytes(&self) -> io::Result<Vec<u8>> {
        self.directory()?;
        let path = self.directory.join(FILE);
        let info = fs::symlink_metadata(&path)?;
        if !info.is_file()
            || info.uid() != self.owner
            || info.mode() & 0o777 != 0o600
            || info.len() > MAX_BYTES
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let file = fs::File::open(path)?;
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != (info.dev(), info.ino()) {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        Ok(bytes)
    }

    fn load(&self) -> io::Result<String> {
        let saved: Saved = serde_json::from_slice(&self.bytes()?).map_err(|_| invalid())?;
        if saved.schema_version != 1 || saved.machine_id != self.machine_id {
            return Err(invalid());
        }
        clean(&saved.product_serial)
    }

    fn store(&self, value: &str) -> io::Result<bool> {
        self.directory()?;
        let bytes = serde_json::to_vec(&Saved {
            schema_version: 1,
            machine_id: self.machine_id.clone(),
            product_serial: clean(value)?,
        })
        .map_err(|_| invalid())?;
        match self.bytes() {
            Ok(previous) if previous == bytes => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Only this unprivileged account can traverse/write the directory.
        // create_new refuses a pre-existing file or symlink; rename replaces
        // only the fixed cache entry, never a symlink's target.
        let temporary = self
            .directory
            .join(format!(".system-serial-{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join(FILE))?;
            fs::File::open(&self.directory)?.sync_all()?;
            Ok(true)
        })();
        let _ = fs::remove_file(temporary);
        result
    }
}

fn resolve(direct: io::Result<String>, cache: Option<&Cache>) -> io::Result<String> {
    match direct.and_then(|value| clean(&value)) {
        Ok(value) => {
            if let Some(cache) = cache {
                let _ = cache.store(&value);
            }
            Ok(value)
        }
        Err(error) => cache.and_then(|cache| cache.load().ok()).ok_or(error),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn read() -> io::Result<(String, bool)> {
    let direct = read_text(Path::new(DMI_PATH)).and_then(|value| clean(&value));
    let cached = direct.is_err();
    resolve(direct, Cache::local().ok().as_ref()).map(|value| (value, cached))
}

#[cfg(target_os = "linux")]
pub(super) fn remember() -> anyhow::Result<()> {
    let mut value = String::new();
    io::stdin()
        .lock()
        .take(MAX_BYTES + 1)
        .read_to_string(&mut value)?;
    anyhow::ensure!(
        value.len() as u64 <= MAX_BYTES,
        "system serial input is too large"
    );
    Cache::local()?.store(&clean(&value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

    struct Fixture(Cache);
    impl std::ops::Deref for Fixture {
        type Target = Cache;
        fn deref(&self) -> &Cache {
            &self.0
        }
    }
    impl std::ops::DerefMut for Fixture {
        fn deref_mut(&mut self) -> &mut Cache {
            &mut self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn fixture() -> Fixture {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "lens-serial-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .expect("unique fixture directory");
        Fixture(Cache {
            owner: fs::metadata(&directory).expect("fixture metadata").uid(),
            directory,
            machine_id: "1234567890abcdef1234567890abcdef".into(),
        })
    }

    #[test]
    fn direct_read_wins_and_identical_bytes_preserve_inode_and_mtime() {
        let cache = fixture();
        assert_eq!(
            resolve(Ok("EXAMPLE-ONE\n".into()), Some(&cache)).expect("valid direct value"),
            "EXAMPLE-ONE"
        );
        let before = fs::metadata(cache.directory.join(FILE)).expect("saved fixture");
        let bytes = cache.bytes().expect("saved bytes");
        assert!(!cache.store("EXAMPLE-ONE").expect("unchanged write"));
        assert_eq!(
            resolve(Ok("EXAMPLE-ONE".into()), Some(&cache)).expect("repeat direct value"),
            "EXAMPLE-ONE"
        );
        let after = fs::metadata(cache.directory.join(FILE)).expect("saved fixture");
        assert_eq!(
            (before.ino(), before.mtime(), before.mtime_nsec()),
            (after.ino(), after.mtime(), after.mtime_nsec())
        );
        assert_eq!(cache.bytes().expect("unchanged bytes"), bytes);
        assert_eq!(
            resolve(Ok("EXAMPLE-TWO".into()), Some(&cache)).expect("changed direct value"),
            "EXAMPLE-TWO"
        );
        assert_eq!(cache.load().expect("changed saved value"), "EXAMPLE-TWO");
        assert_ne!(
            fs::metadata(cache.directory.join(FILE))
                .expect("changed fixture")
                .ino(),
            before.ino()
        );
        assert!(
            !cache
                .store("EXAMPLE-TWO")
                .expect("changed value written only once")
        );
    }

    #[test]
    fn missing_invalid_or_denied_direct_reads_preserve_valid_same_host_fallback() {
        let cache = fixture();
        cache.store("EXAMPLE-ONE").expect("valid fixture");
        let before = cache.bytes().expect("saved bytes");
        for direct in [
            Err(io::ErrorKind::PermissionDenied.into()),
            Err(io::ErrorKind::NotFound.into()),
            Ok("unknown".into()),
            Ok("bad\nserial".into()),
            Ok("0".repeat(20)),
            Ok("x".repeat(257)),
        ] {
            assert_eq!(
                resolve(direct, Some(&cache)).expect("retained same-host value"),
                "EXAMPLE-ONE"
            );
            assert_eq!(cache.bytes().expect("retained bytes"), before);
        }
    }

    #[test]
    fn a_late_cache_is_retried_and_copied_or_invalid_cache_is_not_identity() {
        let mut cache = fixture();
        assert!(resolve(Err(io::ErrorKind::PermissionDenied.into()), Some(&cache)).is_err());
        cache.store("EXAMPLE-ONE").expect("later setup capture");
        assert_eq!(
            resolve(Err(io::ErrorKind::PermissionDenied.into()), Some(&cache))
                .expect("later cache"),
            "EXAMPLE-ONE"
        );
        cache.machine_id = "abcdef1234567890abcdef1234567890".into();
        assert!(resolve(Err(io::ErrorKind::PermissionDenied.into()), Some(&cache)).is_err());
        assert!(!valid_machine_id(""));
        assert!(!valid_machine_id(&"0".repeat(32)));
        assert!(valid_machine_id(&cache.machine_id));
        fs::write(cache.directory.join(FILE), b"invalid").expect("corrupt fixture");
        assert!(cache.load().is_err());
    }

    #[test]
    fn writable_public_symlink_and_wrong_owner_cache_are_rejected() {
        let mut cache = fixture();
        cache.store("EXAMPLE-ONE").expect("valid fixture");
        let path = cache.directory.join(FILE);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("public fixture");
        assert!(cache.load().is_err());
        assert!(cache.store("EXAMPLE-TWO").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore fixture");
        cache.owner = cache.owner.wrapping_add(1);
        assert!(cache.load().is_err());
        cache.owner = fs::metadata(&cache.directory)
            .expect("fixture metadata")
            .uid();
        let target = cache.directory.join("retained");
        fs::rename(&path, &target).expect("retained fixture");
        symlink(&target, &path).expect("symlink fixture");
        assert!(cache.load().is_err());
        assert!(cache.store("EXAMPLE-TWO").is_err());
        assert!(
            fs::read_to_string(target)
                .expect("retained target")
                .contains("EXAMPLE-ONE")
        );
    }
}
