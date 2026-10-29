//! Session-local, bounded performance history. No daemon or guest commands required.
use super::*;

const HISTORY: usize = 300;
const LABELS: [&str; 6] = [
    "CPU",
    "Memory",
    "Network RX",
    "Network TX",
    "Disk read",
    "Disk write",
];

#[derive(Default)]
struct History {
    previous: Option<(Instant, ContainerResources)>,
    series: [VecDeque<Option<u64>>; 6],
}

#[derive(Default)]
pub(crate) struct ContainerMonitor {
    histories: BTreeMap<(String, String), History>,
    pub(crate) allocations: bool,
}

impl ContainerMonitor {
    pub(crate) fn gap(&mut self) {
        for history in self.histories.values_mut() {
            history.previous = None;
            for series in &mut history.series {
                if series.len() == HISTORY {
                    series.pop_front();
                }
                series.push_back(None);
            }
        }
    }

    pub(crate) fn observe(&mut self, containers: &[Container], now: Instant) {
        self.histories.retain(|(runtime, id), _| {
            containers
                .iter()
                .any(|c| &c.runtime == runtime && &c.id == id)
        });
        for container in containers {
            let history = self
                .histories
                .entry((container.runtime.clone(), container.id.clone()))
                .or_default();
            let resources = container.resources.clone().unwrap_or_default();
            let running = container.state == "running";
            let previous = history.previous.as_ref();
            let elapsed = previous
                .map(|(time, _)| now.saturating_duration_since(*time).as_secs_f64())
                .unwrap_or(0.0);
            // Never bridge a collection gap or turn a reset/restart into an I/O spike.
            let rate = |current: Option<u64>, before: Option<u64>| -> Option<u64> {
                if !running || !(0.25..=30.0).contains(&elapsed) {
                    return None;
                }
                Some((current?.checked_sub(before?)? as f64 / elapsed) as u64)
            };
            let before = previous.map(|(_, r)| r);
            let cpu = if running {
                resources
                    .cpu_usage_percent
                    .as_deref()
                    .and_then(|s| s.trim().trim_end_matches('%').parse::<f64>().ok())
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(|n| (n * 100.0) as u64)
                    .or_else(|| {
                        rate(resources.cpu_usage_ns, before.and_then(|r| r.cpu_usage_ns))
                            .map(|n| n / 100_000)
                    })
            } else {
                None
            };
            let sample = [
                cpu,
                resources.memory_usage_bytes.filter(|_| running),
                rate(
                    resources.network_rx_bytes,
                    before.and_then(|r| r.network_rx_bytes),
                ),
                rate(
                    resources.network_tx_bytes,
                    before.and_then(|r| r.network_tx_bytes),
                ),
                rate(
                    resources.disk_read_bytes,
                    before.and_then(|r| r.disk_read_bytes),
                ),
                rate(
                    resources.disk_write_bytes,
                    before.and_then(|r| r.disk_write_bytes),
                ),
            ];
            for (series, value) in history.series.iter_mut().zip(sample) {
                if series.len() == HISTORY {
                    series.pop_front();
                }
                series.push_back(value);
            }
            history.previous = running.then_some((now, resources));
        }
    }

    pub(crate) fn render(
        &self,
        container: &Container,
        rows: u16,
        width: usize,
        out: &mut impl Write,
    ) -> Result<()> {
        let colour = terminal_colour_enabled();
        let line = |out: &mut dyn Write, text: &str| -> Result<()> {
            writeln!(
                out,
                "  {}",
                truncate_text(&console_text(text), width.saturating_sub(4))
            )?;
            Ok(())
        };
        line(
            out,
            &format!(
                "{} / {} / {}",
                container.name, container.runtime, container.state
            ),
        )?;
        if let Some(r) = &container.resources {
            line(out, &container_resource_summary(r))?;
        }
        if rows < 19 {
            line(out, "Enlarge terminal for charts; Tab: allocations")?;
            return Ok(());
        }
        line(
            out,
            "PERFORMANCE   ~2s refresh | session history | Tab: allocations",
        )?;
        let history = self
            .histories
            .get(&(container.runtime.clone(), container.id.clone()));
        let expanded = rows >= 28;
        for (index, label) in LABELS.iter().enumerate() {
            let series = history.map(|h| &h.series[index]);
            let current = series.and_then(|s| s.back()).copied().flatten();
            let format_value = |n: u64| {
                if index == 0 {
                    format!("{:.1}%", n as f64 / 100.0)
                } else if index == 1 {
                    human_bytes(n)
                } else {
                    format!("{}/s", human_bytes(n))
                }
            };
            let current = current
                .map(format_value)
                .unwrap_or_else(|| "waiting / not reported".into());
            let chart_width = if expanded {
                width.saturating_sub(6)
            } else {
                width.saturating_sub(46)
            };
            let chart = sparkline(series, chart_width);
            if expanded {
                let peak = series
                    .and_then(|s| s.iter().flatten().max())
                    .copied()
                    .map(format_value)
                    .unwrap_or_else(|| "-".into());
                line(out, &format!("{label:<12} {current}   peak {peak}"))?;
                writeln!(
                    out,
                    "  {}",
                    ink(
                        &chart,
                        if index < 2 { Ink::Success } else { Ink::Info },
                        colour
                    )
                )?;
            } else {
                line(out, &format!("{label:<11} {current:<25} {chart}"))?;
            }
        }
        if rows >= 23 {
            line(
                out,
                "Charts scale independently; CPU 100% = one logical CPU.",
            )?;
            line(
                out,
                if container.runtime == "lxd" {
                    "Gaps mean unavailable/reset; LXD VM disk I/O is not reported."
                } else {
                    "Gaps mean unavailable counters, collection gaps or restarts."
                },
            )?;
        }
        if rows >= 26
            && let Some(r) = &container.resources
        {
            let processes = r
                .process_count
                .map(|n| n.to_string())
                .unwrap_or_else(|| "not reported".into());
            let used = r
                .root_disk_usage_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "not reported".into());
            line(
                out,
                &format!(
                    "Processes: {processes} | {}: {used}",
                    if container.runtime == "lxd" {
                        "Root disk used"
                    } else {
                        "Writable layer"
                    }
                ),
            )?;
        }
        Ok(())
    }
}

fn sparkline(series: Option<&VecDeque<Option<u64>>>, width: usize) -> String {
    let Some(series) = series else {
        return String::new();
    };
    let visible = series.iter().skip(series.len().saturating_sub(width));
    let peak = visible.clone().flatten().copied().max().unwrap_or(0);
    visible
        .map(|n| match n {
            None => ' ',
            Some(n) => {
                glyphs().bars[if peak == 0 {
                    0
                } else {
                    (*n as u128 * 7 / peak as u128) as usize
                }]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn container(id: &str, cpu: u64, net: u64) -> Container {
        let mut c = demo_snapshot().containers.remove(0);
        c.id = id.into();
        c.state = "running".into();
        c.resources = Some(ContainerResources {
            cpu_usage_ns: Some(cpu),
            network_rx_bytes: Some(net),
            memory_usage_bytes: Some(100),
            ..Default::default()
        });
        c
    }
    #[test]
    fn computes_multicore_rates_and_ignores_resets_gaps_and_missing_samples() {
        let now = Instant::now();
        let mut monitor = ContainerMonitor::default();
        monitor.observe(&[container("a", 1_000_000_000, 100)], now);
        monitor.observe(
            &[container("a", 5_000_000_000, 2100)],
            now + Duration::from_secs(2),
        );
        let history = monitor.histories.values().next().unwrap();
        assert_eq!(history.series[0].back(), Some(&Some(20_000))); // 200% of one CPU
        assert_eq!(history.series[2].back(), Some(&Some(1000)));
        monitor.observe(&[container("a", 1, 0)], now + Duration::from_secs(4));
        assert_eq!(
            monitor.histories.values().next().unwrap().series[0].back(),
            Some(&None)
        );
        monitor.observe(
            &[container("a", 1_000_000_000, 1000)],
            now + Duration::from_secs(40),
        );
        assert_eq!(
            monitor.histories.values().next().unwrap().series[2].back(),
            Some(&None)
        );
    }
    #[test]
    fn histories_are_bounded_isolated_and_stopped_instances_do_not_show_stale_usage() {
        let now = Instant::now();
        let mut monitor = ContainerMonitor::default();
        for i in 0..400 {
            monitor.observe(
                &[container("a", i, i), container("b", i * 2, i * 3)],
                now + Duration::from_secs(i * 2),
            );
        }
        assert_eq!(monitor.histories.len(), 2);
        assert!(
            monitor
                .histories
                .values()
                .all(|h| h.series[0].len() == HISTORY)
        );
        let mut stopped = container("a", 900, 900);
        stopped.state = "exited".into();
        monitor.observe(&[stopped], now + Duration::from_secs(802));
        assert_eq!(monitor.histories.len(), 1);
        assert_eq!(
            monitor.histories.values().next().unwrap().series[1].back(),
            Some(&None)
        );
    }
    #[test]
    fn charts_render_in_ascii_and_compact_terminals_without_false_zeroes() {
        let c = container("a", 1, 1);
        let mut monitor = ContainerMonitor::default();
        monitor.observe(std::slice::from_ref(&c), Instant::now());
        for rows in [18, 24, 30, 40] {
            let text = crate::tests::with_glyphs(true, || {
                let mut out = Vec::new();
                monitor.render(&c, rows, 80, &mut out).unwrap();
                String::from_utf8(out).unwrap()
            });
            assert!(text.is_ascii());
            assert!(text.lines().count() <= usize::from(rows.saturating_sub(8)));
            if rows >= 19 {
                assert!(text.contains("waiting / not reported"));
            }
        }
    }
}
