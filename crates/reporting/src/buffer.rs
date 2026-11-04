use crate::protocol::{MAX_BATCH_BYTES, MAX_BATCH_REPORTS, MAX_REPORT_BYTES, Report, TTL_MS};
use anyhow::Result;
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

pub const BUFFER_BYTES: usize = 4 * 1024 * 1024;
pub const BUFFER_RECORDS: usize = 32;
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub expires_at_ms: u64,
    pub bytes: Arc<Vec<u8>>,
    deadline: Instant,
}
#[derive(Debug, Default)]
pub struct Ring {
    entries: VecDeque<Entry>,
    bytes: usize,
    pub dropped: u64,
}
impl Ring {
    pub fn push(&mut self, report: Report, now: u64, mono: Instant) -> Result<()> {
        report.validate(now)?;
        self.prune(now, mono);
        let bytes = serde_json::to_vec(&report)?;
        if bytes.len() > MAX_REPORT_BYTES {
            anyhow::bail!("report too large");
        }
        let id = report.id();
        if self.entries.iter().any(|e| e.id == id) {
            return Ok(());
        }
        self.bytes += bytes.len();
        // FIFO reconnection replays newest first: arrival order is not observation order.
        let position = self
            .entries
            .partition_point(|entry| entry.expires_at_ms <= report.expires_at_ms);
        self.entries.insert(
            position,
            Entry {
                id,
                expires_at_ms: report.expires_at_ms,
                bytes: Arc::new(bytes),
                deadline: mono + Duration::from_millis((report.expires_at_ms - now).min(TTL_MS)),
            },
        );
        while self.bytes > BUFFER_BYTES || self.entries.len() > BUFFER_RECORDS {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.bytes.len();
                self.dropped = self.dropped.saturating_add(1);
            } else {
                break;
            }
        }
        Ok(())
    }
    pub fn prune(&mut self, now: u64, mono: Instant) {
        self.entries
            .retain(|entry| entry.expires_at_ms > now && entry.deadline > mono);
        self.bytes = self.entries.iter().map(|entry| entry.bytes.len()).sum();
    }
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.entries.iter()
    }
    pub fn latest(&mut self, now: u64, mono: Instant) -> Vec<Entry> {
        self.prune(now, mono);
        self.entries.back().cloned().into_iter().collect()
    }
    pub fn batch(&mut self, now: u64, mono: Instant) -> Vec<Entry> {
        self.prune(now, mono);
        let mut batch = Vec::new();
        let mut bytes = 64;
        // Current state first, then oldest retained reports. At most one bounded batch per interval.
        for entry in self.entries.back().into_iter().chain(self.entries.iter()) {
            if batch.iter().any(|e: &Entry| e.id == entry.id) {
                continue;
            }
            if batch.len() == MAX_BATCH_REPORTS || bytes + entry.bytes.len() + 1 > MAX_BATCH_BYTES {
                break;
            }
            bytes += entry.bytes.len() + 1;
            batch.push(entry.clone());
        }
        batch
    }
    pub fn remove(&mut self, batch: &[Entry]) {
        self.entries.retain(|e| !batch.iter().any(|b| b.id == e.id));
        self.bytes = self.entries.iter().map(|entry| entry.bytes.len()).sum();
    }
}

pub fn encode_batch(entries: &[Entry]) -> Vec<u8> {
    let mut bytes = b"{\"protocol_version\":1,\"reports\":[".to_vec();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            bytes.push(b',');
        }
        bytes.extend_from_slice(&entry.bytes);
    }
    bytes.extend_from_slice(b"]}");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::fixture;
    #[test]
    fn newest_first_replay_never_replaces_current_state_with_older_arrivals() {
        let mono = Instant::now();
        let mut ring = Ring::default();
        for seq in (1..=32).rev() {
            ring.push(fixture(seq, 1000 + seq), 1000, mono)
                .expect("replay");
        }
        ring.push(fixture(100, 999), 1000, mono)
            .expect("older replay under pressure");
        assert_eq!(ring.entries().count(), 32);
        assert!(ring.latest(1000, mono)[0].id.ends_with(":32"));
        let batch = ring.batch(1000, mono);
        assert!(batch[0].id.ends_with(":32"));
        assert!(batch[1].id.ends_with(":1"));
    }
    #[test]
    fn slowed_publishing_sends_only_current_state_without_extending_expiry() {
        let mut ring = Ring::default();
        let mono = Instant::now();
        for seq in 1..=30 {
            ring.push(fixture(seq, 1000), 1000, mono).expect("sample");
        }
        let batch = ring.latest(1000, mono);
        assert_eq!(batch.len(), 1);
        assert!(batch[0].id.ends_with(":30"));
        assert!(
            ring.latest(
                1000 + 4 * 60 * 60 * 1000,
                mono + Duration::from_secs(4 * 60 * 60)
            )
            .is_empty()
        );
    }
    #[test]
    fn ring_wraps_and_never_keeps_expired_history_after_clock_rollback() {
        let mut ring = Ring::default();
        let mono = Instant::now();
        for seq in 1..=100 {
            ring.push(fixture(seq, 1000), 1000, mono).expect("fixture");
        }
        assert_eq!(ring.entries().count(), BUFFER_RECORDS);
        assert_eq!(ring.dropped, 68);
        ring.prune(0, mono + Duration::from_millis(TTL_MS));
        assert_eq!(ring.entries().count(), 0);
        assert_eq!(ring.bytes, 0);
    }
    #[test]
    fn latest_first_recovery_remains_bounded_and_acknowledges_only_sent_records() {
        let mut ring = Ring::default();
        let mono = Instant::now();
        for seq in 1..=30 {
            ring.push(fixture(seq, 1000), 1000, mono).expect("fixture");
        }
        let batch = ring.batch(1000, mono);
        assert_eq!(batch.len(), 4);
        assert!(batch[0].id.ends_with(":30"));
        assert!(batch[1].id.ends_with(":1"));
        let bytes = encode_batch(&batch);
        assert!(bytes.len() <= MAX_BATCH_BYTES);
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("batch JSON");
        assert_eq!(json["reports"].as_array().expect("reports").len(), 4);
        ring.remove(&batch);
        assert_eq!(ring.entries().count(), 26);
    }
    #[test]
    fn duplicates_do_not_refresh_deadlines_or_grow_memory() {
        let mut ring = Ring::default();
        let mono = Instant::now();
        ring.push(fixture(1, 1000), 1000, mono).expect("fixture");
        ring.push(fixture(1, 1000), 2000, mono + Duration::from_secs(1))
            .expect("duplicate");
        assert_eq!(ring.entries().count(), 1);
        ring.prune(1000 + TTL_MS, mono);
        assert_eq!(ring.entries().count(), 0);
    }
    #[test]
    fn byte_limit_wins_over_record_count() {
        let mut ring = Ring::default();
        let mono = Instant::now();
        for seq in 1..=100 {
            let mut report = fixture(seq, 1000);
            report.snapshot["large"] =
                serde_json::Value::String("x".repeat(MAX_REPORT_BYTES - 10000));
            ring.push(report, 1000, mono).expect("bounded fixture");
            assert!(ring.bytes <= BUFFER_BYTES);
            assert!(ring.entries().count() <= BUFFER_RECORDS);
            if seq > 32 {
                assert!(ring.entries().count() < BUFFER_RECORDS);
            }
        }
    }
}
