use crate::{
    buffer::{Entry, Ring},
    protocol::{Decoder, Report},
};
use anyhow::{Context, Result, bail};
use rustix::fs::{FlockOperation, OFlags};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

fn open_fifo(path: &Path, write: bool) -> Result<File> {
    let parent = path.parent().context("FIFO needs parent directory")?;
    let parent_meta = parent.symlink_metadata()?;
    let before = path.symlink_metadata()?;
    if !parent_meta.is_dir()
        || parent_meta.mode() & 0o022 != 0
        || !before.file_type().is_fifo()
        || before.mode() & 0o777 != 0o240
        || parent_meta.uid() != before.uid()
    {
        bail!("FIFO must have a trusted owner, non-writable parent and mode 0240");
    }
    if write && before.uid() != rustix::process::geteuid().as_raw() {
        bail!("collector must own FIFO");
    }
    let file = OpenOptions::new()
        .read(!write)
        .write(write)
        .custom_flags((OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
        .open(path)?;
    let after = file.metadata()?;
    if before.ino() != after.ino() || before.dev() != after.dev() {
        bail!("FIFO changed while opening");
    }
    Ok(file)
}

pub fn singleton(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
        .open(path)?;
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive)
        .context("another reporting process already holds the lock")?;
    Ok(file)
}

#[derive(Debug, Default)]
pub struct Writer {
    file: Option<File>,
    pending: Option<(Entry, usize)>,
    sent: HashSet<String>,
    cut: bool,
}
impl Writer {
    pub fn flush(&mut self, path: &Path, ring: &Ring) -> Result<()> {
        self.sent.retain(|id| ring.entries().any(|e| &e.id == id));
        if self
            .pending
            .as_ref()
            .is_some_and(|(entry, _)| !ring.entries().any(|e| e.id == entry.id))
        {
            self.pending = None;
            self.cut = true;
        }
        if self.file.is_none() {
            match open_fifo(path, true) {
                Ok(file) => {
                    self.file = Some(file);
                    self.sent.clear();
                    self.cut = true;
                }
                Err(error) => {
                    if error.downcast_ref::<io::Error>().is_some_and(|e| {
                        matches!(
                            e.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::WouldBlock
                        ) || e.raw_os_error() == Some(6)
                    }) {
                        return Ok(());
                    }
                    return Err(error);
                }
            }
        }
        if self.cut {
            match self.file.as_mut().context("FIFO disappeared")?.write(b"\n") {
                Ok(1) => self.cut = false,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    self.file = None;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
                _ => return Ok(()),
            }
        }
        // Limit work per tick; EAGAIN never blocks the collector.
        for _ in 0..128 {
            if self.pending.is_none() {
                let next = ring
                    .entries()
                    .rev()
                    .find(|e| !self.sent.contains(&e.id))
                    .cloned();
                let Some(next) = next else {
                    break;
                };
                self.pending = Some((next, 0));
            }
            let Some((entry, offset)) = self.pending.as_mut() else {
                break;
            };
            let bytes = if *offset < entry.bytes.len() {
                &entry.bytes[*offset..(*offset + 4096).min(entry.bytes.len())]
            } else {
                b"\n"
            };
            let result = self.file.as_mut().context("FIFO disappeared")?.write(bytes);
            match result {
                Ok(0) => break,
                Ok(n) => {
                    *offset += n;
                    if *offset > entry.bytes.len() {
                        self.sent.insert(entry.id.clone());
                        self.pending = None;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    self.file = None;
                    self.pending = None;
                    self.sent.clear();
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Reader {
    file: File,
    decoder: Decoder,
}
impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            file: open_fifo(path, false)?,
            decoder: Decoder::default(),
        })
    }
    pub fn read(&mut self, now: u64) -> Result<Vec<Report>> {
        let mut bytes = [0; 8192];
        match self.file.read(&mut bytes) {
            Ok(0) => {
                self.decoder.reset();
                Ok(Vec::new())
            }
            Ok(n) => Ok(self.decoder.feed(&bytes[..n], now)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TTL_MS, fixture};
    use std::time::Instant;
    #[test]
    fn backpressure_keeps_collection_unblocked_and_frames_survive_partial_writes() {
        let (read, write) = rustix::pipe::pipe().expect("test pipe");
        let mut read = File::from(read);
        crate::nonblocking(&read).expect("nonblocking test reader");
        crate::nonblocking(&write).expect("nonblocking test writer");
        let mut writer = Writer {
            file: Some(File::from(write)),
            ..Writer::default()
        };
        let mut ring = Ring::default();
        for seq in 1..=2 {
            let mut report = fixture(seq, 1000);
            report.snapshot["large"] = serde_json::Value::String("x".repeat(200_000));
            ring.push(report, 1000, Instant::now())
                .expect("test report");
        }
        writer
            .flush(Path::new("/not-used"), &ring)
            .expect("nonblocking write");
        assert!(writer.pending.is_some());
        assert_eq!(ring.entries().count(), 2);
        let mut decoder = Decoder::default();
        let mut received = Vec::new();
        for _ in 0..1000 {
            let mut bytes = [0; 8192];
            match read.read(&mut bytes) {
                Ok(n) => received.extend(decoder.feed(&bytes[..n], 1000)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("test read failed: {e}"),
            }
            writer
                .flush(Path::new("/not-used"), &ring)
                .expect("resume partial write");
            if received.len() == 2 {
                break;
            }
        }
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].sequence, 2);
        assert_eq!(received[1].sequence, 1);
    }
    #[test]
    fn expiry_discards_partial_frames_instead_of_publishing_old_data() {
        let (read, write) = rustix::pipe::pipe().expect("test pipe");
        let mut read = File::from(read);
        crate::nonblocking(&read).expect("nonblocking test reader");
        crate::nonblocking(&write).expect("nonblocking test writer");
        let mut writer = Writer {
            file: Some(File::from(write)),
            ..Writer::default()
        };
        let mono = Instant::now();
        let mut ring = Ring::default();
        let mut report = fixture(1, 1000);
        report.snapshot["large"] = serde_json::Value::String("x".repeat(200_000));
        ring.push(report, 1000, mono).expect("test report");
        writer
            .flush(Path::new("/not-used"), &ring)
            .expect("partial write");
        assert!(writer.pending.is_some());
        ring.prune(1000 + TTL_MS, mono);
        let mut decoder = Decoder::default();
        for _ in 0..100 {
            let mut bytes = [0; 8192];
            match read.read(&mut bytes) {
                Ok(n) => assert!(decoder.feed(&bytes[..n], 1000 + TTL_MS).is_empty()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("test read failed: {e}"),
            }
            writer
                .flush(Path::new("/not-used"), &ring)
                .expect("expiry boundary");
        }
        assert!(writer.pending.is_none());
        assert!(!writer.cut);
    }
    #[test]
    fn publisher_lock_is_exclusive_and_normal_files_cannot_replace_fifo() {
        let dir = std::env::temp_dir().join(format!("lens-ipc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).expect("fixture directory");
        let path = dir.join("lock");
        let lock = singleton(&path).expect("first lock");
        assert!(singleton(&path).is_err());
        assert!(open_fifo(&path, true).is_err());
        drop(lock);
        std::fs::remove_dir_all(&dir).expect("fixture cleanup");
    }
}
