//! Session-only presentation state; observations and machine-readable output remain unchanged.
use super::*;
use std::collections::BTreeSet;

pub(super) fn level(log: &LogEntry) -> Option<&'static str> {
    match log
        .priority
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "emerg" | "emergency" | "alert" | "crit" | "critical" | "err" | "error" => Some("error"),
        "warn" | "warning" => Some("warning"),
        "notice" => Some("notice"),
        "info" => Some("info"),
        "debug" => Some("debug"),
        _ => match log_priority(&log.message).as_deref() {
            Some("error") => Some("error"),
            Some("warning") => Some("warning"),
            _ => None,
        },
    }
}
pub(super) fn flagged(log: &LogEntry) -> bool {
    matches!(level(log), Some("error" | "warning"))
}

pub(super) fn readable_limit(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "not reported".into();
    };
    for (unit, scale) in [
        ("KiB", 1024u64),
        ("MiB", 1024u64.pow(2)),
        ("GiB", 1024u64.pow(3)),
    ] {
        if let Some(n) = value
            .strip_suffix(unit)
            .and_then(|n| n.trim().parse::<u64>().ok())
        {
            return n
                .checked_mul(scale)
                .map(human_bytes)
                .unwrap_or_else(|| value.into());
        }
    }
    value.into()
}

pub(super) fn resolver_label(value: &str) -> String {
    if value
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
    {
        format!("{value} (local stub; upstream not reported)")
    } else {
        value.into()
    }
}

pub(super) fn sensor_label(snapshot: &SystemSnapshot, sensor: &TemperatureSensor) -> String {
    let parent = Path::new(&sensor.source).parent();
    let package = snapshot
        .temperatures
        .iter()
        .find(|s| s.name.contains("Package id ") && Path::new(&s.source).parent() == parent);
    if let Some(package) = package.and_then(|s| s.name.split_once("Package id ").map(|(_, id)| id))
    {
        let name = sensor
            .name
            .strip_prefix("coretemp ")
            .unwrap_or(&sensor.name);
        return format!("CPU {package} / {name}");
    }
    let source = parent
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or(&sensor.source);
    format!("{} [{source}]", sensor.name)
}

pub(super) fn gauge(label: &str, percent: f64, width: usize) -> String {
    let percent = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let bar_width = width.saturating_sub(label.chars().count() + 11).min(24);
    let filled = (percent / 100.0 * bar_width as f64).round() as usize;
    format!(
        "{label} [{fill}{rest}] {percent:.1}%",
        fill = glyphs().blocks[8].to_string().repeat(filled),
        rest = " ".repeat(bar_width - filled)
    )
}

type NetworkCounters = (Option<u64>, Option<u64>, Option<u64>, Option<u64>);

#[derive(Debug, Default)]
pub(super) struct SessionView {
    pub mode: usize,
    pub expanded: bool,
    pub paused: bool,
    pub last_sample: Option<Instant>,
    first_sample: Option<Instant>,
    service_cpu: BTreeMap<String, VecDeque<u64>>,
    service_rss: BTreeMap<String, VecDeque<u64>>,
    restart_base: BTreeMap<String, u64>,
    restart_latest: BTreeMap<String, u64>,
    thermal: BTreeMap<String, VecDeque<u64>>,
    power: VecDeque<u64>,
    power_observed_at: Option<Timestamp>,
    log_activity: VecDeque<u64>,
    seen_logs: BTreeSet<String>,
    net_baseline: BTreeMap<String, NetworkCounters>,
    net_changes: BTreeMap<String, String>,
    last_log_sample: Option<Instant>,
}
impl SessionView {
    pub fn for_args(args: &ViewArgs) -> Self {
        Self {
            expanded: args.mount.is_some()
                || args.fstype.is_some()
                || args.class.is_some()
                || args.filter.is_some()
                || args.serial.is_some(),
            ..Self::default()
        }
    }
    pub fn cycle(&mut self, view: View) {
        self.mode = (self.mode + 1)
            % match view {
                View::Services => 4,
                View::Logs => 3,
                _ => 1,
            };
    }
    pub fn label(&self, view: View) -> &str {
        match view {
            View::Services => ["all", "running", "failed", "restarted"][self.mode % 4],
            View::Logs => ["all levels", "warning+", "error+"][self.mode % 3],
            View::Disk | View::Hardware => {
                if self.expanded {
                    "all inventory"
                } else {
                    "summary inventory"
                }
            }
            _ => "",
        }
    }
    pub fn observe(&mut self, snapshot: &SystemSnapshot, now: Instant) {
        self.first_sample.get_or_insert(now);
        let has_previous = self.last_sample.is_some();
        self.last_sample = Some(now);
        self.service_cpu
            .retain(|name, _| snapshot.services.iter().any(|s| &s.name == name));
        self.service_rss
            .retain(|name, _| snapshot.services.iter().any(|s| &s.name == name));
        self.restart_base
            .retain(|name, _| snapshot.services.iter().any(|s| &s.name == name));
        self.restart_latest.clear();
        for service in &snapshot.services {
            if let Some(count) = service.restart_count {
                let base = self
                    .restart_base
                    .entry(service.name.clone())
                    .or_insert(count);
                // Counter reset starts a new baseline; never underflow or report a negative delta.
                *base = (*base).min(count);
                self.restart_latest
                    .insert(service.name.clone(), count.saturating_sub(*base));
            }
            let processes: Vec<_> = snapshot
                .processes
                .iter()
                .filter(|p| p.service.as_ref().is_some_and(|s| s.name == service.name))
                .collect();
            if !processes.is_empty() {
                let cpu = processes
                    .iter()
                    .map(|p| p.cpu_percent.max(0.0))
                    .sum::<f64>();
                let rss = processes
                    .iter()
                    .fold(0u64, |sum, p| sum.saturating_add(p.rss_bytes));
                if has_previous {
                    push_bounded(
                        self.service_cpu.entry(service.name.clone()).or_default(),
                        (cpu * 100.0).round() as u64,
                        120,
                    );
                }
                push_bounded(
                    self.service_rss.entry(service.name.clone()).or_default(),
                    rss,
                    120,
                );
            } else {
                self.service_cpu.remove(&service.name);
                self.service_rss.remove(&service.name);
            }
        }
        if let Some(reading) = snapshot
            .hardware
            .server
            .as_ref()
            .and_then(|server| server.power.as_ref())
        {
            if self.power_observed_at.as_ref() != Some(&reading.observed_at) {
                push_bounded(&mut self.power, u64::from(reading.watts), 120);
                self.power_observed_at = Some(reading.observed_at.clone());
            }
        } else {
            // Do not draw a line across a missing measurement or invent a zero.
            self.power.clear();
            self.power_observed_at = None;
        }
        self.thermal
            .retain(|source, _| snapshot.temperatures.iter().any(|s| &s.source == source));
        for sensor in &snapshot.temperatures {
            push_bounded(
                self.thermal.entry(sensor.source.clone()).or_default(),
                (sensor.temperature_c.max(0.0) * 10.0).round() as u64,
                120,
            );
        }
        self.net_baseline
            .retain(|name, _| snapshot.network_details.contains_key(name));
        self.net_changes.clear();
        for (name, meta) in &snapshot.network_details {
            let now = (
                meta.rx_errors,
                meta.tx_errors,
                meta.rx_dropped,
                meta.tx_dropped,
            );
            let base = self.net_baseline.entry(name.clone()).or_insert(now);
            let delta = |value: Option<u64>, old: &mut Option<u64>| -> String {
                match (value, *old) {
                    (Some(value), Some(previous)) if value >= previous => {
                        format!("+{}", value - previous)
                    }
                    (Some(value), _) => {
                        *old = Some(value);
                        "reset/baseline".into()
                    }
                    _ => "unavailable".into(),
                }
            };
            let text = format!(
                "Session errors RX {} / TX {} | drops RX {} / TX {}",
                delta(now.0, &mut base.0),
                delta(now.1, &mut base.1),
                delta(now.2, &mut base.2),
                delta(now.3, &mut base.3)
            );
            self.net_changes.insert(name.clone(), text);
        }
        // Counts describe new observations per completed collection, not an inferred event rate.
        if !snapshot.log_sources.is_empty() || !snapshot.logs.is_empty() {
            let keys: BTreeSet<_> = snapshot.logs.iter().rev().take(2000).map(log_key).collect();
            let new = if self.last_log_sample.is_some() {
                snapshot
                    .logs
                    .iter()
                    .rev()
                    .take(2000)
                    .filter(|l| flagged(l) && !self.seen_logs.contains(&log_key(l)))
                    .count() as u64
            } else {
                0
            };
            if self.last_log_sample.is_some() {
                push_bounded(&mut self.log_activity, new, 120);
            }
            self.seen_logs = keys;
            self.last_log_sample = Some(now);
        }
    }
    pub fn apply(&self, view: View, snapshot: &mut SystemSnapshot) {
        match view {
            View::Services => snapshot.services.retain(|s| match self.mode % 4 {
                1 => s.active == "active" && s.sub == "running",
                2 => s.active == "failed",
                3 => s.restart_count.unwrap_or(0) > 0,
                _ => true,
            }),
            View::Logs => {
                snapshot.logs.retain(|log| match self.mode % 3 {
                    1 => flagged(log),
                    2 => level(log) == Some("error"),
                    _ => true,
                });
            }
            View::Disk if !self.expanded => {
                snapshot.mounts.retain(investigate::persistent_mount);
                snapshot
                    .block_devices
                    .retain(|d| !matches!(d.kind.as_str(), "loop" | "rom"));
            }
            View::Hardware => {
                if !self.expanded {
                    snapshot.temperatures.retain(|s| !s.name.contains(" Core "));
                }
                snapshot.temperatures.sort_by(|a, b| {
                    Path::new(&a.source)
                        .parent()
                        .cmp(&Path::new(&b.source).parent())
                        .then_with(|| a.name.contains(" Core ").cmp(&b.name.contains(" Core ")))
                        .then_with(|| b.temperature_c.total_cmp(&a.temperature_c))
                });
            }
            View::Net => {
                let original = snapshot.interfaces.clone();
                let mut sorted = Vec::new();
                for root in original.iter().filter(|i| {
                    snapshot
                        .network_details
                        .get(&i.name)
                        .is_none_or(|m| m.master.is_none())
                }) {
                    sorted.push(root.clone());
                    sorted.extend(
                        original
                            .iter()
                            .filter(|child| {
                                snapshot
                                    .network_details
                                    .get(&child.name)
                                    .and_then(|m| m.master.as_deref())
                                    == Some(&root.name)
                            })
                            .cloned(),
                    );
                }
                for item in original {
                    if !sorted.iter().any(|i| i.name == item.name) {
                        sorted.push(item);
                    }
                }
                snapshot.interfaces = sorted;
            }
            _ => {}
        }
    }
    pub fn render(
        &self,
        view: View,
        snapshot: &SystemSnapshot,
        selected: usize,
        width: usize,
        budget: usize,
        out: &mut impl Write,
    ) -> Result<()> {
        if budget < 5 {
            return Ok(());
        }
        let mut lines = Vec::new();
        if view == View::Services {
            if let Some(service) = snapshot.services.get(selected) {
                if let Some(delta) = self.restart_latest.get(&service.name) {
                    lines.push(format!(
                        "Restarts +{delta} since session baseline (cumulative {})",
                        service.restart_count.unwrap_or(0)
                    ));
                }
                for (label, history, divisor) in [
                    ("CPU %", self.service_cpu.get(&service.name), 100.0),
                    (
                        "Host RSS MiB",
                        self.service_rss.get(&service.name),
                        1024.0 * 1024.0,
                    ),
                ] {
                    if let Some(history) = history {
                        lines.extend(trend_lines(
                            label,
                            history,
                            width.saturating_sub(8),
                            divisor,
                            1,
                        ));
                    }
                }
            }
        } else if view == View::Net {
            if let Some(interface) = snapshot.interfaces.get(selected) {
                if let Some(delta) = self.net_changes.get(&interface.name) {
                    lines.push(delta.clone());
                }
            }
        } else if view == View::Hardware {
            if !self.power.is_empty() {
                lines.extend(trend_lines(
                    "Host power W",
                    &self.power,
                    width.saturating_sub(8),
                    1.0,
                    if budget >= 12 { 3 } else { 1 },
                ));
                let mean = self.power.iter().sum::<u64>() as f64 / self.power.len() as f64;
                lines.push(format!(
                    "Mean {mean:.1} W / {} measured BMC samples",
                    self.power.len()
                ));
            }
            for sensor in snapshot
                .temperatures
                .iter()
                .filter(|s| !s.name.contains(" Core "))
                .take(4)
            {
                if let Some(history) = self.thermal.get(&sensor.source) {
                    lines.extend(trend_lines(
                        &sensor_label(snapshot, sensor),
                        history,
                        width.saturating_sub(8),
                        10.0,
                        1,
                    ));
                }
            }
        } else if view == View::Logs && !self.log_activity.is_empty() {
            lines.extend(trend_lines(
                "New flagged / refresh (latest 2000)",
                &self.log_activity,
                width.saturating_sub(8),
                1.0,
                1,
            ));
        }
        if !lines.is_empty() {
            let elapsed = self.first_sample.map_or(0, |t| t.elapsed().as_secs());
            let panel = overview::OverviewPanel::new(
                &format!("TRENDS ~10s/sample / {elapsed}s observed / min..max scale"),
                lines,
                Ink::Info,
            );
            let height = panel.lines.len().min(budget - 3);
            overview::draw_panels(&[panel], width.saturating_sub(4), height, out)?;
        }
        Ok(())
    }
}
fn log_key(log: &LogEntry) -> String {
    format!(
        "{}\0{}\0{}\0{}",
        log.timestamp,
        log.source,
        log.unit.as_deref().unwrap_or(""),
        log.message
    )
}
/// Labels never compete with the graph for horizontal space. Sparse observations
/// occupy equal time slots; stretching their display does not invent new samples.
fn trend_lines(
    label: &str,
    history: &VecDeque<u64>,
    width: usize,
    divisor: f64,
    height: usize,
) -> Vec<String> {
    if history.is_empty() || width == 0 {
        return Vec::new();
    }
    let minimum = history.iter().copied().min().unwrap_or(0);
    let maximum = history.iter().copied().max().unwrap_or(0);
    let current = history.back().copied().unwrap_or(0) as f64 / divisor;
    let mut lines = vec![truncate_text(
        &format!(
            "{label}: {current:.1} / min {:.1} / peak {:.1} / {} samples",
            minimum as f64 / divisor,
            maximum as f64 / divisor,
            history.len()
        ),
        width,
    )];
    // One observation is a value, not a trend. Keep the history visibly pending.
    if history.len() == 1 {
        lines.push(truncate_text("Collecting history...", width));
        return lines;
    }
    let height = height.max(1);
    let bars = glyphs().bars;
    for row in (0..height).rev() {
        let graph: String = (0..width)
            .map(|column| {
                // Compress dense histories by retaining each bucket's peak.
                let start = column * history.len() / width;
                let end = ((column + 1) * history.len() / width).max(start + 1);
                let value = history
                    .range(start..end.min(history.len()))
                    .copied()
                    .max()
                    .unwrap_or(minimum);
                let level = if minimum == maximum {
                    height * 4
                } else {
                    1 + ((value - minimum) as u128 * (height * 8 - 1) as u128
                        / (maximum - minimum) as u128) as usize
                };
                let fill = level.saturating_sub(row * 8).min(8);
                if fill == 0 {
                    ' '
                } else {
                    bars[fill.saturating_sub(1).min(7)]
                }
            })
            .collect();
        lines.push(graph);
    }
    lines
}

// All escapes here are Lens-authored SGR sequences. Untrusted fields are sanitized upstream.
fn cell_width(line: &str) -> usize {
    let mut escape = false;
    let plain: String = line
        .chars()
        .filter(|c| {
            if *c == '\x1b' {
                escape = true;
                return false;
            }
            if escape {
                if *c == 'm' {
                    escape = false;
                }
                return false;
            }
            true
        })
        .collect();
    unicode_width::UnicodeWidthStr::width(plain.as_str())
}
pub(super) fn columns(
    left: &[u8],
    right: &[u8],
    left_width: usize,
    right_width: usize,
    height: usize,
    out: &mut impl Write,
) -> Result<()> {
    let left = String::from_utf8_lossy(left);
    let right = String::from_utf8_lossy(right);
    let left: Vec<_> = left.lines().collect();
    let right: Vec<_> = right.lines().collect();
    for row in 0..left.len().max(right.len()).min(height) {
        let a = clip_sgr(left.get(row).copied().unwrap_or(""), left_width);
        let b = if row + 1 == height && right.len() > height {
            "  Enter: full detail".into()
        } else {
            clip_sgr(right.get(row).copied().unwrap_or(""), right_width)
        };
        writeln!(
            out,
            "{a}{} {} {b}",
            " ".repeat(left_width.saturating_sub(cell_width(&a))),
            ink(
                &glyphs().frame_vertical.to_string(),
                Ink::Border,
                terminal_colour_enabled()
            )
        )?;
    }
    Ok(())
}

pub(super) fn log_window(logs: &[LogEntry]) -> String {
    let mut timestamps = logs.iter().filter_map(|l| {
        time::OffsetDateTime::parse(&l.timestamp, &time::format_description::well_known::Rfc3339)
            .ok()
    });
    let Some(first) = timestamps.next() else {
        return "window timestamps unavailable".into();
    };
    let (min, max) = timestamps.fold((first, first), |(min, max), t| (min.min(t), max.max(t)));
    let fmt = time::macros::format_description!("[month]-[day] [hour]:[minute]");
    format!(
        "{}..{} UTC",
        min.to_offset(time::UtcOffset::UTC)
            .format(fmt)
            .unwrap_or_default(),
        max.to_offset(time::UtcOffset::UTC)
            .format(fmt)
            .unwrap_or_default()
    )
}

fn clip_sgr(line: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut chars = line.chars().peekable();
    let mut used = 0;
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c == 'm' {
                    break;
                }
            }
            continue;
        }
        let next = c.width().unwrap_or(0);
        if used + next > width {
            break;
        }
        used += next;
        out.push(c);
    }
    if line.contains('\x1b') {
        out.push_str("\x1b[0m");
    }
    out
}

pub(super) fn expiry_status(value: &str, now: OffsetDateTime) -> String {
    let expiry = OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .or_else(|| {
            let parts: Vec<_> = value.split_whitespace().collect();
            if parts.len() != 5 || parts[4] != "GMT" {
                return None;
            }
            let day = parts[1].parse::<u8>().ok()?;
            let normalized = format!("{} {day:02} {} {} GMT", parts[0], parts[2], parts[3]);
            time::PrimitiveDateTime::parse(
                &normalized,
                time::macros::format_description!(
                    "[month repr:short] [day] [hour]:[minute]:[second] [year] GMT"
                ),
            )
            .ok()
            .map(|t| t.assume_utc())
        });
    let Some(expiry) = expiry else {
        return "expiry date not understood".into();
    };
    let delta = expiry - now;
    if delta.is_negative() {
        format!("EXPIRED {}d ago", (-delta).whole_days())
    } else if delta.whole_days() < 1 {
        format!("expires in {}h", delta.whole_hours())
    } else {
        format!("expires in {}d", delta.whole_days())
    }
}

pub(super) fn bound_body(out: &mut Vec<u8>, start: usize, width: usize, height: usize) {
    let body = String::from_utf8_lossy(&out[start..]);
    let lines: Vec<_> = body.lines().collect();
    let mut bounded = String::new();
    for (index, line) in lines.iter().take(height).enumerate() {
        if index + 1 == height && lines.len() > height {
            bounded.push_str(&truncate_text(
                "  More detail: enlarge terminal or use full screen",
                width,
            ));
        } else {
            bounded.push_str(&clip_sgr(line, width));
        }
        bounded.push('\n');
    }
    out.truncate(start);
    out.extend_from_slice(bounded.as_bytes());
}

pub(super) fn log_clock(timestamp: &str) -> String {
    OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|t| {
            t.to_offset(time::UtcOffset::UTC)
                .format(time::macros::format_description!(
                    "[hour]:[minute]:[second]"
                ))
                .ok()
        })
        .unwrap_or_else(|| timestamp.get(11..19).unwrap_or(timestamp).into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn charts_keep_labels_separate_and_fill_available_width() {
        let history = VecDeque::from([130, 140, 132, 128]);
        for width in [1, 28, 72, 192] {
            let lines = trend_lines("Host power W", &history, width, 1.0, 3);
            assert_eq!(lines.len(), 4);
            assert!(lines.iter().all(|line| cell_width(line) <= width));
            assert!(lines[1..].iter().all(|line| cell_width(line) == width));
        }
        let lines = trend_lines("Power W", &history, 72, 1.0, 3);
        assert!(lines[0].contains("128.0 / min 128.0 / peak 140.0 / 4 samples"));
        assert_ne!(lines[1], lines[3]);
        assert!(trend_lines("Power W", &VecDeque::new(), 72, 1.0, 3).is_empty());
        assert_eq!(
            trend_lines("Power W", &VecDeque::from([130]), 72, 1.0, 3)[1],
            "Collecting history..."
        );
        assert!(
            trend_lines("Power W", &VecDeque::from([130, 130]), 72, 1.0, 3)
                .iter()
                .all(|line| cell_width(line) <= 72)
        );
        let dense = (0..120).collect::<VecDeque<u64>>();
        assert_eq!(
            trend_lines("dense", &dense, 4, 1.0, 1)[1].chars().count(),
            4
        );
    }

    #[test]
    fn shared_trends_respect_row_and_column_budgets() {
        let snapshot = demo_snapshot();
        let mut ui = SessionView::default();
        ui.observe(&snapshot, Instant::now());
        ui.power = VecDeque::from([130, 140, 132, 128]);
        for view in [View::Hardware, View::Services, View::Logs] {
            for width in [36, 80, 200] {
                for budget in [5, 8, 18] {
                    let mut out = Vec::new();
                    ui.render(view, &snapshot, 0, width, budget, &mut out)
                        .unwrap();
                    let text = String::from_utf8(out).unwrap();
                    assert!(text.lines().count() <= budget);
                    assert!(text.lines().all(|line| cell_width(line) <= width));
                }
            }
        }
    }

    #[test]
    fn power_history_deduplicates_cached_samples_and_clears_unavailable_gaps() {
        let mut snapshot = SystemSnapshot::empty("power-fixture");
        snapshot.hardware.server = Some(lens_model::ServerContext {
            power: Some(lens_model::PowerReading {
                watts: 182,
                observed_at: Timestamp("2026-11-11T00:00:00Z".into()),
                source: "local_bmc_dcmi".into(),
            }),
            ..Default::default()
        });
        let mut ui = SessionView::default();
        ui.observe(&snapshot, Instant::now());
        ui.observe(&snapshot, Instant::now());
        assert_eq!(ui.power.len(), 1);
        snapshot
            .hardware
            .server
            .as_mut()
            .unwrap()
            .power
            .as_mut()
            .unwrap()
            .observed_at = Timestamp("2026-11-11T00:00:10Z".into());
        ui.observe(&snapshot, Instant::now());
        assert_eq!(ui.power.len(), 2);
        snapshot.hardware.server.as_mut().unwrap().power = None;
        ui.observe(&snapshot, Instant::now());
        assert!(ui.power.is_empty());
        assert!(ui.power_observed_at.is_none());
    }

    use super::*;
    #[test]
    fn priorities_override_message_words_and_filters_restore_cleanly() {
        let mut snapshot = demo_snapshot();
        let mut log = snapshot.logs[0].clone();
        log.priority = Some("info".into());
        log.message = "No errors or warnings reported".into();
        assert!(!flagged(&log));
        log.priority = Some("error".into());
        log.message = "Warning handler crashed".into();
        assert!(flagged(&log));
        snapshot.logs = vec![log.clone()];
        let ui = SessionView {
            mode: 2,
            ..SessionView::default()
        };
        ui.apply(View::Logs, &mut snapshot);
        assert_eq!(snapshot.logs.len(), 1);
        log.priority = None;
        log.message = "WARNING problem".into();
        assert!(flagged(&log));
        snapshot.logs = vec![log];
        ui.apply(View::Logs, &mut snapshot);
        assert!(snapshot.logs.is_empty());
    }
    #[test]
    fn units_sensor_identity_and_expiry_are_readable() {
        assert_eq!(readable_limit(Some("12582912KiB")), "12.0GiB");
        assert_eq!(readable_limit(Some("unlimited")), "unlimited");
        let now = OffsetDateTime::parse(
            "2026-11-01T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(
            expiry_status("Nov  3 00:00:00 2026 GMT", now),
            "expires in 2d"
        );
        assert!(expiry_status("2026-10-30T00:00:00Z", now).starts_with("EXPIRED"));
        assert_eq!(
            expiry_status("unavailable", now),
            "expiry date not understood"
        );
        let mut snapshot = demo_snapshot();
        snapshot.temperatures = vec![
            TemperatureSensor {
                name: "coretemp Package id 1".into(),
                source: "/sys/class/hwmon/hwmon4/temp1_input".into(),
                temperature_c: 60.0,
                max_c: Some(90.0),
                critical_c: Some(100.0),
            },
            TemperatureSensor {
                name: "coretemp Core 0".into(),
                source: "/sys/class/hwmon/hwmon4/temp2_input".into(),
                temperature_c: 54.0,
                max_c: Some(90.0),
                critical_c: Some(100.0),
            },
        ];
        assert_eq!(
            sensor_label(&snapshot, &snapshot.temperatures[1]),
            "CPU 1 / Core 0"
        );
        assert!(resolver_label("127.0.0.53").contains("local stub"));
    }
    #[test]
    fn histories_are_bounded_reset_safely_and_do_not_count_initial_log_backlog() {
        let mut snapshot = demo_snapshot();
        snapshot.services[0].restart_count = Some(4);
        let mut ui = SessionView::default();
        let now = Instant::now();
        ui.observe(&snapshot, now);
        assert!(ui.log_activity.is_empty());
        snapshot.services[0].restart_count = Some(6);
        ui.observe(&snapshot, now + Duration::from_secs(10));
        assert_eq!(ui.restart_latest[&snapshot.services[0].name], 2);
        snapshot.services[0].restart_count = Some(1);
        for index in 2..150 {
            ui.observe(&snapshot, now + Duration::from_secs(index * 10));
        }
        assert_eq!(ui.restart_latest[&snapshot.services[0].name], 0);
        assert_eq!(ui.log_activity.len(), 120);
        assert!(ui.thermal.values().all(|values| values.len() <= 120));
        assert!(ui.log_activity.iter().all(|value| *value == 0));
    }
    #[test]
    fn coloured_wide_panes_preserve_columns_and_clip_long_unicode_fields() {
        let left = ink("NAME       VALUE", Ink::Label, true);
        let right = "界".repeat(100);
        let mut out = Vec::new();
        columns(
            format!("{left}\n").as_bytes(),
            right.as_bytes(),
            20,
            30,
            10,
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let line = text.lines().next().unwrap();
        assert_eq!(cell_width(line), 53);
        assert!(line.contains("VALUE"));
        let mut out = b"header\n".to_vec();
        let start = out.len();
        out.extend_from_slice("long detail\n".repeat(20).as_bytes());
        bound_body(&mut out, start, 30, 4);
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 5);
    }
    #[test]
    fn inventory_summary_keeps_backing_devices_and_expands_from_original() {
        let source = demo_snapshot();
        let ui = SessionView::default();
        let mut summary = source.clone();
        ui.apply(View::Disk, &mut summary);
        assert!(summary.mounts.iter().all(investigate::persistent_mount));
        assert!(
            summary
                .block_devices
                .iter()
                .all(|d| !matches!(d.kind.as_str(), "loop" | "rom"))
        );
        let mut expanded = source.clone();
        let ui = SessionView {
            expanded: true,
            ..SessionView::default()
        };
        ui.apply(View::Disk, &mut expanded);
        assert_eq!(expanded.mounts.len(), source.mounts.len());
    }
}
