use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    pub failures: u32,
    pub not_before_ms: u64,
    pub halted: bool,
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
}
pub const INTERVAL_HEADER: &str = "Lens-Report-Interval-Seconds";
fn default_interval() -> u64 {
    60
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            failures: 0,
            not_before_ms: 0,
            halted: false,
            interval_seconds: default_interval(),
        }
    }
}

pub fn retry_after_ms(value: Option<&str>, date: Option<&str>, now_ms: u64) -> Option<u64> {
    let text = value?.trim();
    if !text.is_empty() && text.bytes().all(|c| c.is_ascii_digit()) {
        // Overflow means an effectively indefinite hold, never an early retry.
        return Some(text.parse::<u64>().unwrap_or(u64::MAX).saturating_mul(1000));
    }
    let target = httpdate::parse_http_date(text)
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .min(u64::MAX as u128) as u64;
    let server_ms = date
        .and_then(|d| httpdate::parse_http_date(d).ok())
        .and_then(|d| d.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(now_ms);
    Some(
        target
            .saturating_sub(now_ms)
            .max(target.saturating_sub(server_ms)),
    )
}
impl Gate {
    pub fn interval_ms(&self) -> u64 {
        self.interval_seconds.clamp(60, 14_400) * 1000
    }
    pub fn observe_interval(&mut self, header: Option<&str>) {
        if let Some(seconds) = header
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| (60..=14_400).contains(seconds))
        {
            self.interval_seconds = seconds;
        }
    }
    pub fn ready(&self, now: u64) -> bool {
        !self.halted && now >= self.not_before_ms
    }
    pub fn reserve(&mut self, now: u64) {
        self.not_before_ms = now.saturating_add(
            self.interval_ms() + rand::rng().random_range(0..=(self.interval_ms() / 5).min(60_000)),
        );
    }
    pub fn reserve_attempt(&mut self, now: u64) {
        // A crash during an ambiguous request must not turn Restart= into a fast retry loop.
        self.not_before_ms = now
            .saturating_add(self.interval_ms().max(300_000) + rand::rng().random_range(0..=60_000));
    }
    pub fn success(&mut self, now: u64) {
        self.failures = 0;
        self.reserve(now);
    }
    pub fn failure(&mut self, now: u64, retry_ms: Option<u64>) {
        self.failures = self.failures.saturating_add(1);
        let ceiling = 60_000u64
            .saturating_mul(1u64 << self.failures.min(6))
            .min(3_600_000);
        let local = rand::rng().random_range(60_000..=ceiling);
        let floor = local.max(retry_ms.unwrap_or(0)).max(self.interval_ms());
        let jitter = rand::rng().random_range(0..=60_000.min(floor / 10));
        self.not_before_ms = self
            .not_before_ms
            .max(now.saturating_add(floor).saturating_add(jitter));
    }
    pub fn status(
        &mut self,
        status: u16,
        retry: Option<&str>,
        date: Option<&str>,
        now: u64,
    ) -> Outcome {
        if (200..300).contains(&status) {
            self.success(now);
            Outcome::Accepted
        } else if status == 429 || status == 408 || (500..600).contains(&status) {
            self.failure(now, retry_after_ms(retry, date, now));
            Outcome::Retry
        } else if matches!(status, 401 | 403 | 404 | 405 | 410 | 415 | 426)
            || (300..400).contains(&status)
        {
            self.halted = true;
            Outcome::Halt
        } else {
            self.success(now);
            Outcome::Discard
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Retry,
    Discard,
    Halt,
}

pub fn startup_delay() -> Duration {
    Duration::from_millis(rand::rng().random_range(1000..=60_000))
}
pub fn http_date(time: SystemTime) -> String {
    httpdate::fmt_http_date(time)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_interval_is_bounded_sticky_and_backward_compatible() {
        let mut gate: Gate =
            serde_json::from_str(r#"{"failures":0,"not_before_ms":0,"halted":false}"#)
                .expect("old state");
        assert_eq!(gate.interval_seconds, 60);
        gate.observe_interval(Some("14400"));
        assert_eq!(gate.interval_ms(), 14_400_000);
        for bad in [
            None,
            Some(""),
            Some("0"),
            Some("59"),
            Some("14401"),
            Some("99999999999999999999999"),
            Some("2m"),
            Some("120.0"),
            Some("-120"),
        ] {
            gate.observe_interval(bad);
            assert_eq!(gate.interval_seconds, 14400);
        }
        let loaded: Gate =
            serde_json::from_slice(&serde_json::to_vec(&gate).expect("persist")).expect("restart");
        assert_eq!(loaded.interval_seconds, 14400);
        gate.observe_interval(Some(" 120 "));
        assert_eq!(gate.interval_seconds, 120);
    }
    #[test]
    fn four_hour_policy_is_a_floor_for_success_crashes_and_failures() {
        for status in [201, 400, 429, 503] {
            let mut gate = Gate::default();
            gate.observe_interval(Some("14400"));
            gate.reserve_attempt(1000);
            assert!(!gate.ready(1000 + 14_400_000 - 1));
            gate.status(status, Some("60"), None, 1000);
            assert!(!gate.ready(1000 + 14_400_000 - 1));
            assert!(gate.not_before_ms <= 1000 + 14_400_000 + 60_000);
        }
        let mut gate = Gate::default();
        gate.observe_interval(Some("120"));
        gate.status(429, Some("86400"), None, 1000);
        let hold = gate.not_before_ms;
        gate.observe_interval(Some("60"));
        assert_eq!(gate.not_before_ms, hold);
        assert!(!gate.ready(86_400_999));
    }

    #[test]
    fn delta_date_and_clock_skew_retry_after() {
        assert_eq!(retry_after_ms(Some("7200"), None, 1000), Some(7_200_000));
        assert_eq!(retry_after_ms(Some("0"), None, 1000), Some(0));
        assert_eq!(retry_after_ms(Some("NaN"), None, 1000), None);
        assert_eq!(retry_after_ms(Some("inf"), None, 1000), None);
        assert_eq!(
            retry_after_ms(Some("99999999999999999999999999"), None, 1000),
            Some(u64::MAX)
        );
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let date = http_date(base);
        let target = http_date(base + Duration::from_secs(7200));
        assert_eq!(
            retry_after_ms(Some(&target), Some(&date), 1_000_000_000),
            Some(7_200_000)
        );
        assert_eq!(
            retry_after_ms(Some(&target), Some(&date), 1_000_060_000),
            Some(7_200_000)
        );
    }
    #[test]
    fn never_caps_server_hold_or_retries_before_it() {
        for _ in 0..1000 {
            let mut gate = Gate::default();
            gate.reserve(1000);
            assert_eq!(gate.status(429, Some("86400"), None, 1000), Outcome::Retry);
            assert!(!gate.ready(1000 + 86_400_000 - 1));
            assert!(gate.not_before_ms >= 86_401_000);
            assert!(gate.not_before_ms <= 86_461_000);
        }
    }
    #[test]
    fn malformed_header_still_backs_off_and_exponent_is_bounded() {
        let mut gate = Gate::default();
        let mut now = 1000;
        for _ in 0..1000 {
            gate.status(503, Some("invalid"), None, now);
            assert!(gate.not_before_ms >= now + 60_000);
            assert!(gate.not_before_ms <= now + 3_660_000);
            now = gate.not_before_ms;
        }
        assert_eq!(gate.failures, 1000);
    }
    #[test]
    fn permanent_errors_halt_without_automatic_enrolment() {
        for status in [301, 302, 307, 308, 401, 403, 404, 405, 410] {
            let mut gate = Gate::default();
            assert_eq!(gate.status(status, None, None, 1000), Outcome::Halt);
            assert!(!gate.ready(u64::MAX));
        }
    }
    #[test]
    fn poison_records_are_discarded_without_hot_loops() {
        for status in [400, 409, 413, 422] {
            let mut gate = Gate::default();
            assert_eq!(gate.status(status, None, None, 1000), Outcome::Discard);
            assert!(!gate.ready(60_999));
        }
    }
    #[test]
    fn serializing_gate_preserves_hold_and_attempts_across_restart() {
        let mut gate = Gate::default();
        gate.status(429, Some("7200"), None, 1000);
        let loaded: Gate =
            serde_json::from_slice(&serde_json::to_vec(&gate).expect("fixture serialization"))
                .expect("valid fixture");
        assert_eq!(loaded.failures, 1);
        assert_eq!(loaded.not_before_ms, gate.not_before_ms);
        assert!(!loaded.ready(7_200_999));
    }
    #[test]
    fn fleet_recovery_is_staggered_and_old_headers_do_not_create_busy_retries() {
        let mut starts = std::collections::HashSet::new();
        let mut retries = std::collections::HashSet::new();
        for _ in 0..1000 {
            starts.insert(startup_delay().as_millis() / 1000);
            let mut gate = Gate::default();
            gate.status(429, Some("60"), None, 1000);
            retries.insert(gate.not_before_ms / 1000);
            assert!(!gate.ready(60_999));
        }
        assert!(starts.len() > 40);
        assert!(retries.len() > 30);
    }
}

#[cfg(test)]
mod fleet_tests {
    use super::*;
    #[test]
    fn five_thousand_hosts_do_not_release_a_catchup_storm_after_outage() {
        let mut hosts: Vec<_> = (0..5000)
            .map(|_| Gate {
                not_before_ms: startup_delay().as_millis() as u64,
                ..Gate::default()
            })
            .collect();
        let mut counts = vec![0usize; 1800];
        for (second, count) in counts.iter_mut().enumerate().skip(1) {
            let now = second as u64 * 1000;
            for gate in &mut hosts {
                if gate.ready(now) {
                    *count += 1;
                    gate.reserve_attempt(now);
                    if second < 600 {
                        gate.status(429, Some("300"), None, now);
                    } else {
                        gate.status(201, None, None, now);
                    }
                    assert!(!gate.ready(now + 59_999));
                }
            }
        }
        assert!(counts.iter().sum::<usize>() <= 5000 * 31);
        assert!(counts.iter().copied().max().expect("simulated bins") < 250);
    }
}
