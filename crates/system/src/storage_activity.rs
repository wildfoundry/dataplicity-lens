//! Kernel disk counters sampled independently of inventory; no disk probes or guest commands.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Counters {
    identity: String,
    read: u64,
    written: u64,
    active_ms: u64,
}
#[derive(Debug, Default)]
pub(super) struct StorageActivity {
    previous: BTreeMap<String, Counters>,
    pub(super) rates: BTreeMap<String, (u64, u64, f64)>,
    read: BTreeMap<String, VecDeque<u64>>,
    written: BTreeMap<String, VecDeque<u64>>,
    last: Option<Instant>,
}
impl StorageActivity {
    pub(super) fn observe(&mut self, current: BTreeMap<String, Counters>, now: Instant) {
        self.rates.clear();
        self.read.retain(|name, _| current.contains_key(name));
        self.written.retain(|name, _| current.contains_key(name));
        if let Some(previous_at) = self.last {
            let elapsed = now.saturating_duration_since(previous_at).as_secs_f64();
            if elapsed > 0.0 {
                for (name, counter) in &current {
                    if let Some(old) = self.previous.get(name).filter(|old| {
                        old.identity == counter.identity
                            && old.read <= counter.read
                            && old.written <= counter.written
                            && old.active_ms <= counter.active_ms
                    }) {
                        let read = ((counter.read - old.read) as f64 / elapsed).round() as u64;
                        let written =
                            ((counter.written - old.written) as f64 / elapsed).round() as u64;
                        let active = ((counter.active_ms - old.active_ms) as f64
                            / (elapsed * 10.0))
                            .clamp(0.0, 100.0);
                        self.rates.insert(name.clone(), (read, written, active));
                        push_bounded(
                            self.read.entry(name.clone()).or_default(),
                            read,
                            ACTIVITY_HISTORY_CAPACITY,
                        );
                        push_bounded(
                            self.written.entry(name.clone()).or_default(),
                            written,
                            ACTIVITY_HISTORY_CAPACITY,
                        );
                    } else {
                        self.read.remove(name);
                        self.written.remove(name);
                    }
                }
            }
        }
        self.previous = current;
        self.last = Some(now);
    }
    pub(super) fn gap(&mut self) {
        self.last = None;
        self.previous.clear();
        self.rates.clear();
        self.read.clear();
        self.written.clear();
    }
    pub(super) fn render(
        &self,
        name: &str,
        width: usize,
        rows: u16,
        out: &mut impl Write,
    ) -> Result<()> {
        let colour = terminal_colour_enabled();
        if let Some((read, written, active)) = self.rates.get(name) {
            writeln!(
                out,
                "\n  {}",
                ink(
                    &truncate_text(
                        &format!(
                            "LIVE I/O {name} | read {}/s | write {}/s | kernel active {:.1}%",
                            human_bytes(*read),
                            human_bytes(*written),
                            active
                        ),
                        width.saturating_sub(4)
                    ),
                    Ink::Bright,
                    colour
                )
            )?;
            if rows >= 40 && width >= 80 {
                for (label, history, tone) in [
                    ("READ", self.read.get(name), Ink::Info),
                    ("WRITE", self.written.get(name), Ink::Brand),
                ] {
                    if let Some(history) = history {
                        let graph_width = width.saturating_sub(12);
                        let peak = history
                            .iter()
                            .rev()
                            .take(graph_width)
                            .copied()
                            .max()
                            .unwrap_or(0);
                        writeln!(
                            out,
                            "  {}",
                            ink(
                                &format!("{label} peak {}/s", human_bytes(peak)),
                                tone,
                                colour
                            )
                        )?;
                        for line in block_chart_rows(history, graph_width, peak.max(1), 2) {
                            writeln!(out, "  {}", ink(&line, tone, colour))?;
                        }
                    }
                }
                writeln!(
                    out,
                    "  {}",
                    ink(
                        "Kernel active time is not a queue/latency or device-saturation measurement.",
                        Ink::Muted,
                        colour
                    )
                )?;
            }
        } else {
            writeln!(
                out,
                "\n  {}",
                ink(
                    &truncate_text(
                        &format!(
                            "LIVE I/O {name}: collecting samples or unavailable on this platform"
                        ),
                        width.saturating_sub(4)
                    ),
                    Ink::Muted,
                    colour
                )
            )?;
        }
        Ok(())
    }
}

pub(super) fn sample() -> std::result::Result<BTreeMap<String, Counters>, String> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/diskstats")
            .map_err(|e| format!("disk activity unavailable: {e}"))?;
        let mut result = parse(&text);
        for (name, counter) in &mut result {
            if let Ok(sequence) =
                std::fs::read_to_string(Path::new("/sys/class/block").join(name).join("diskseq"))
            {
                counter.identity.push(':');
                counter.identity.push_str(sequence.trim());
            }
        }
        Ok(result)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(BTreeMap::new())
    }
}
#[cfg(any(target_os = "linux", test))]
fn parse(text: &str) -> BTreeMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 14 {
                return None;
            }
            let number = |index: usize| fields[index].parse::<u64>().ok();
            Some((
                fields[2].to_owned(),
                Counters {
                    identity: format!("{}:{}", fields[0], fields[1]),
                    read: number(5)?.checked_mul(512)?,
                    written: number(9)?.checked_mul(512)?,
                    active_ms: number(12)?,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rates_use_elapsed_time_and_restart_or_hotplug_is_a_gap() {
        let first = parse("8 0 disk0 1 0 10 1 2 0 20 2 0 100 100\n");
        let now = Instant::now();
        let mut activity = StorageActivity::default();
        activity.observe(first, now);
        assert!(activity.rates.is_empty());
        let second = parse("8 0 disk0 2 0 14 2 3 0 28 3 0 200 200\n");
        activity.observe(second, now + Duration::from_secs(2));
        assert_eq!(activity.rates["disk0"], (1024, 2048, 5.0));
        let reset = parse("8 0 disk0 1 0 1 1 1 0 1 1 0 1 1\n");
        activity.observe(reset, now + Duration::from_secs(3));
        assert!(activity.rates.is_empty());
        assert!(activity.read.is_empty());
        activity.gap();
        assert!(activity.rates.is_empty());
    }
    #[test]
    fn malformed_and_short_records_do_not_become_zero() {
        assert!(parse("bad\n8 0 disk0 0\n").is_empty());
    }
}
