//! A bounded overview of actual workloads, with explicit inspection limits.
use std::collections::BTreeSet;

use super::*;

#[derive(Debug)]
struct Workload {
    key: String,
    name: String,
    processes: usize,
    cpu: f64,
    rss: u64,
}

fn workloads(snapshot: &SystemSnapshot) -> Vec<Workload> {
    let mut groups: BTreeMap<String, Workload> = BTreeMap::new();
    for process in &snapshot.processes {
        // Linux kernel workers descend from kthreadd. Keep them in the full Processes view.
        if (process.pid == ProcessId(2) || process.parent_pid == Some(ProcessId(2)))
            && process.rss_bytes == 0
            && process.virtual_memory_bytes == 0
            && process.command_line.as_deref().is_none_or(str::is_empty)
        {
            continue;
        }
        let vm = if process.name.starts_with("qemu-system") {
            snapshot.containers.iter().find(|vm| {
                vm.runtime == "libvirt"
                    && process.command_line.as_deref().is_some_and(|cmd| {
                        let mut args = cmd.split_whitespace();
                        args.any(|argument| argument == "-uuid")
                            && args.next() == Some(vm.id.as_str())
                    })
            })
        } else {
            None
        };
        let (key, name) = if let Some(vm) = vm {
            (format!("vm:{}", vm.id), format!("VM {}", vm.name))
        } else if let Some(service) = &process.service {
            (
                format!("service:{}:{}", process.user.uid, service.name),
                service.name.clone(),
            )
        } else {
            (
                format!("process:{}:{}", process.pid, process.start_time_ticks),
                process.name.clone(),
            )
        };
        let row = groups.entry(key.clone()).or_insert(Workload {
            key,
            name,
            processes: 0,
            cpu: 0.0,
            rss: 0,
        });
        row.processes += 1;
        row.cpu += process.cpu_percent;
        row.rss = row.rss.saturating_add(process.rss_bytes);
    }
    let mut rows: Vec<_> = groups.into_values().collect();
    rows.sort_by(|left, right| {
        right
            .cpu
            .total_cmp(&left.cpu)
            .then_with(|| right.rss.cmp(&left.rss))
            .then_with(|| left.name.cmp(&right.name))
    });
    rows
}

pub(super) fn render_workloads(
    snapshot: &SystemSnapshot,
    width: usize,
    rows: u16,
    out: &mut impl Write,
) -> Result<()> {
    let mut capacity = if rows < 36 {
        usize::from(rows.saturating_sub(31)).max(1)
    } else {
        (3 + usize::from(rows.saturating_sub(36)) * 2 / 3).min(10)
    };
    let workloads = workloads(snapshot);
    let side_by_side = width >= 110 && rows >= 40;
    if side_by_side && rows < 48 {
        capacity = capacity.saturating_sub(2).max(1);
    }
    let cell_width = if side_by_side {
        width.saturating_sub(6) / 2
    } else {
        width.saturating_sub(4)
    };
    let name_width = cell_width.saturating_sub(34).max(8);
    let content: Vec<_> = workloads
        .iter()
        .take(capacity)
        .map(|workload| {
            format!(
                "{:<name_width$} {:>6.1}% {:>9} {:>3}p",
                truncate_text(&workload.name, name_width),
                workload.cpu,
                human_bytes(workload.rss),
                workload.processes,
            )
        })
        .collect();
    writeln!(out)?;
    let colour = terminal_colour_enabled();
    if side_by_side {
        let left = panel_lines(
            "BUSIEST WORKLOADS  CPU / RSS SUM / PROCS",
            &content,
            cell_width,
        );
        let resource = resources(snapshot);
        let mut resource: Vec<_> = resource.into_iter().take(content.len()).collect();
        resource.resize(content.len(), String::new());
        let right = panel_lines("HOST RESOURCES", &resource, cell_width);
        for (left, right) in left.iter().zip(&right) {
            writeln!(
                out,
                "  {}  {}",
                style_frame_line(
                    left,
                    "BUSIEST WORKLOADS  CPU / RSS SUM / PROCS",
                    Ink::Info,
                    Ink::Bright,
                    colour
                ),
                style_frame_line(right, "HOST RESOURCES", Ink::Brand, Ink::Bright, colour)
            )?;
        }
    } else {
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(
                    "BUSIEST WORKLOADS  CPU / RSS SUM / PROCS",
                    width.saturating_sub(4)
                ),
                Ink::Label,
                colour
            )
        )?;
        for line in content {
            writeln!(
                out,
                "  {}",
                ink(
                    &truncate_text(&line, width.saturating_sub(4)),
                    Ink::Bright,
                    colour
                )
            )?;
        }
        if rows >= 60 {
            writeln!(out, "\n  {}", ink("HOST RESOURCES", Ink::Label, colour))?;
            for line in resources(snapshot).into_iter().take(5) {
                writeln!(
                    out,
                    "  {}",
                    ink(
                        &truncate_text(&line, width.saturating_sub(4)),
                        Ink::Muted,
                        colour
                    )
                )?;
            }
        }
    }
    Ok(())
}

fn resources(snapshot: &SystemSnapshot) -> Vec<String> {
    let memory = &snapshot.host.memory;
    let mut rows = vec![
        format!(
            "RAM {} used / {} total",
            human_bytes(memory.used_bytes),
            human_bytes(memory.total_bytes)
        ),
        format!(
            "Available {}  |  Swap {} / {}",
            human_bytes(memory.available_bytes),
            human_bytes(memory.swap_used_bytes),
            human_bytes(memory.swap_total_bytes)
        ),
    ];
    let mut mounts: Vec<_> = snapshot
        .mounts
        .iter()
        .filter(|mount| mount.source.starts_with("/dev/") && !mount.source.starts_with("/dev/loop"))
        .collect();
    mounts.sort_by(|a, b| {
        b.used_percent
            .total_cmp(&a.used_percent)
            .then_with(|| a.target.cmp(&b.target))
    });
    for mount in mounts.iter().take(3) {
        rows.push(format!(
            "{}  {:.0}% used  |  {} free",
            mount.target,
            mount.used_percent,
            human_bytes(mount.available_bytes)
        ));
    }
    let vms = snapshot
        .containers
        .iter()
        .filter(|c| c.instance_type.as_deref() == Some("virtual-machine"))
        .count();
    if vms > 0 {
        rows.push(format!(
            "{} VMs  |  {} running",
            vms,
            snapshot
                .containers
                .iter()
                .filter(|c| c.instance_type.as_deref() == Some("virtual-machine")
                    && c.state == "running")
                .count()
        ));
    }
    if let Some(sensor) = snapshot
        .temperatures
        .iter()
        .max_by(|a, b| a.temperature_c.total_cmp(&b.temperature_c))
    {
        rows.push(format!(
            "Hottest sensor: {} {:.0}C",
            sensor.name, sensor.temperature_c
        ));
    }
    rows
}

pub(super) fn limits(snapshot: &SystemSnapshot) -> Vec<&str> {
    snapshot
        .collection_warnings
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn render_coverage(
    snapshot: &SystemSnapshot,
    width: usize,
    maximum: usize,
    out: &mut impl Write,
) -> Result<()> {
    let limits = limits(snapshot);
    let colour = terminal_colour_enabled();
    let sampled = OffsetDateTime::parse(
        &snapshot.generated_at.0,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|time| {
        format!(
            "{}s ago",
            (OffsetDateTime::now_utc() - time).whole_seconds().max(0)
        )
    })
    .unwrap_or_else(|_| "time unknown".into());
    let summary = format!(
        "Inventory sampled {sampled} | {} collection limit{}",
        limits.len(),
        if limits.len() == 1 { "" } else { "s" }
    );
    writeln!(
        out,
        "  {}",
        ink(
            &truncate_text(&summary, width.saturating_sub(4)),
            Ink::Muted,
            colour
        )
    )?;
    for reason in limits.iter().take(maximum) {
        writeln!(
            out,
            "  {}",
            ink(
                &truncate_text(reason, width.saturating_sub(4)),
                Ink::Attention,
                colour
            )
        )?;
    }
    if limits.len() > maximum {
        writeln!(
            out,
            "  {}",
            ink(
                "Open Health for all collection limits.",
                Ink::Attention,
                colour
            )
        )?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(super) struct WorkloadHistory(BTreeMap<String, VecDeque<u64>>);

impl WorkloadHistory {
    pub(super) fn observe(&mut self, snapshot: &SystemSnapshot) {
        let rows = workloads(snapshot);
        let keys: BTreeSet<_> = rows.iter().take(32).map(|row| row.key.clone()).collect();
        self.0.retain(|key, _| keys.contains(key));
        for row in rows.into_iter().take(32) {
            push_bounded(
                self.0.entry(row.key).or_default(),
                (row.cpu * 100.0).round() as u64,
                ACTIVITY_HISTORY_CAPACITY,
            );
        }
    }
}

/// Extra panels consume only the rows left after navigation and the main overview.
pub(super) fn render_details(
    snapshot: &SystemSnapshot,
    history: &WorkloadHistory,
    width: usize,
    budget: usize,
    out: &mut impl Write,
) -> Result<()> {
    if budget < 10 {
        return Ok(());
    }
    let columns = if width >= 110 { 2 } else { 1 };
    let cell = width.saturating_sub(4 + (columns - 1) * 2) / columns;
    let mut panels: Vec<OverviewPanel> = Vec::new();
    // Two lines of chart plus current value make each workload readable even at modest heights.
    let chart_count = ((budget / 2).saturating_sub(3) / 5 * columns).clamp(1, 8);
    for row in workloads(snapshot).into_iter().take(chart_count) {
        let mut lines = vec![format!(
            "CPU {:>5.1}%    RAM {:>9}    {} proc",
            row.cpu,
            human_bytes(row.rss),
            row.processes
        )];
        if let Some(values) = history.0.get(&row.key).filter(|values| !values.is_empty()) {
            let graph_width = cell.saturating_sub(4);
            let peak = values
                .iter()
                .rev()
                .take(graph_width)
                .copied()
                .max()
                .unwrap_or(0);
            lines.extend(block_chart_rows(values, graph_width, peak.max(100), 2));
            lines.push(format!(
                "CPU peak {:.1}% | {} samples",
                peak as f64 / 100.0,
                values.len()
            ));
        } else {
            lines.push("Collecting CPU history...".into());
        }
        let accent = if row.key.starts_with("vm:") {
            Ink::Brand
        } else {
            Ink::Info
        };
        let mut panel = OverviewPanel::new(&row.name, lines, accent);
        panel.tones = vec![Ink::Bright, accent, accent, Ink::Muted];
        panels.push(panel);
    }
    writeln!(
        out,
        "\n  {}",
        ink(
            &truncate_text(
                "WORKLOAD CPU HISTORY   100% = one logical CPU | each chart scales to its visible peak",
                width.saturating_sub(4)
            ),
            Ink::Label,
            terminal_colour_enabled()
        )
    )?;
    let mut remaining = budget.saturating_sub(2);
    for pair in panels.chunks(columns) {
        let height = pair
            .iter()
            .map(|panel| panel.lines.len())
            .max()
            .unwrap_or(0);
        if remaining < height + 3 {
            break;
        }
        draw_panels(pair, cell, height, out)?;
        remaining -= height + 3;
    }
    let mut details = Vec::new();
    let vms: Vec<_> = snapshot
        .containers
        .iter()
        .map(|vm| {
            let resources = vm.resources.as_ref();
            format!(
                "{:<28} {:<9} {:>8}   RAM {:>9}",
                vm.name,
                vm.state,
                resources
                    .and_then(|r| r.cpu_limit.as_deref())
                    .unwrap_or("not reported"),
                resources
                    .and_then(|r| r.memory_usage_bytes)
                    .map(human_bytes)
                    .unwrap_or_else(|| "not reported".into())
            )
        })
        .collect();
    if !vms.is_empty() {
        let tones = snapshot
            .containers
            .iter()
            .map(|vm| match vm.state.as_str() {
                "running" => Ink::Bright,
                "dead" => Ink::Critical,
                "unknown" => Ink::Attention,
                _ => Ink::Muted,
            })
            .collect();
        let mut panel = OverviewPanel::new(
            "INSTANCES   state / CPU allocation / host RSS",
            vms,
            Ink::Brand,
        );
        panel.tones = tones;
        details.push(panel);
    }
    let mut sensors: Vec<_> = snapshot.temperatures.iter().collect();
    sensors.sort_by(|a, b| {
        b.temperature_c
            .total_cmp(&a.temperature_c)
            .then_with(|| a.name.cmp(&b.name))
    });
    if !sensors.is_empty() {
        details.push(OverviewPanel::new(
            "TEMPERATURES   hottest first",
            sensors
                .iter()
                .map(|s| format!("{:>5.1}C   {}", s.temperature_c, s.name))
                .collect(),
            Ink::Label,
        ));
        if let Some(panel) = details.last_mut() {
            panel.tones = sensors
                .iter()
                .map(|sensor| {
                    if sensor
                        .critical_c
                        .is_some_and(|limit| sensor.temperature_c >= limit)
                        || sensor.temperature_c >= 90.0
                    {
                        Ink::Critical
                    } else if sensor.temperature_c
                        >= sensor
                            .max_c
                            .unwrap_or(80.0)
                            .min(sensor.critical_c.unwrap_or(90.0))
                    {
                        Ink::Attention
                    } else {
                        Ink::Bright
                    }
                })
                .collect();
        }
    }
    let mut services: Vec<_> = snapshot
        .services
        .iter()
        .filter(|s| s.active == "failed" || s.restart_count.unwrap_or(0) > 0)
        .collect();
    services.sort_by_key(|s| {
        (
            s.active != "failed",
            std::cmp::Reverse(s.restart_count.unwrap_or(0)),
            &s.name,
        )
    });
    details.push(OverviewPanel::new(
        "SERVICES   failures / restart counts",
        services
            .iter()
            .map(|s| {
                format!(
                    "{}  {}/{}  restarts {}",
                    s.name,
                    s.active,
                    s.sub,
                    s.restart_count.unwrap_or(0)
                )
            })
            .chain(
                services
                    .is_empty()
                    .then(|| "OK  No failures or restarts reported".into()),
            )
            .collect(),
        Ink::Label,
    ));
    if let Some(panel) = details.last_mut() {
        panel.tones = if services.is_empty() {
            vec![Ink::Success]
        } else {
            services
                .iter()
                .map(|service| {
                    if service.active == "failed" {
                        Ink::Critical
                    } else {
                        Ink::Attention
                    }
                })
                .collect()
        };
    }
    details.push(OverviewPanel::new(
        "NETWORK   interface / state / addresses",
        snapshot
            .interfaces
            .iter()
            .filter(|i| !i.addresses.is_empty())
            .filter(|i| {
                !i.addresses
                    .iter()
                    .all(|address| address.starts_with("fe80:"))
            })
            .map(|i| format!("{:<12} {:<8} {}", i.name, i.state, i.addresses.join("  ")))
            .collect(),
        Ink::Success,
    ));
    details.push(OverviewPanel::new(
        "STORAGE   used / available / capacity",
        snapshot
            .mounts
            .iter()
            .filter(|m| m.source.starts_with("/dev/") && !m.source.starts_with("/dev/loop"))
            .map(|m| {
                format!(
                    "{:<16} {:>3.0}%   {} free / {}",
                    m.target,
                    m.used_percent,
                    human_bytes(m.available_bytes),
                    human_bytes(m.total_bytes)
                )
            })
            .collect(),
        Ink::Label,
    ));
    if let Some(panel) = details.last_mut() {
        panel.tones = snapshot
            .mounts
            .iter()
            .filter(|m| m.source.starts_with("/dev/") && !m.source.starts_with("/dev/loop"))
            .map(|mount| {
                if mount.used_percent >= 97.0 {
                    Ink::Critical
                } else if mount.used_percent >= 90.0 {
                    Ink::Attention
                } else {
                    Ink::Bright
                }
            })
            .collect();
    }
    details.push(OverviewPanel::new(
        "LOGS   recent flagged entries",
        snapshot
            .logs
            .iter()
            .rev()
            .filter(|log| {
                let text = log.message.to_ascii_lowercase();
                text.contains("error") || text.contains("fail") || text.contains("warn")
            })
            .map(|log| {
                format!(
                    "{} {}: {}",
                    log.timestamp
                        .split_once('T')
                        .map_or(log.timestamp.as_str(), |(_, clock)| clock
                            .get(..8)
                            .unwrap_or(clock)),
                    log.unit.as_deref().unwrap_or(&log.source),
                    journal::display(&log.message)
                )
            })
            .collect(),
        Ink::Attention,
    ));
    let reasons = limits(snapshot);
    if !reasons.is_empty() {
        let mut panel = OverviewPanel::new(
            "COVERAGE   unavailable sources",
            reasons.into_iter().map(str::to_owned).collect(),
            Ink::Attention,
        );
        panel.tones = vec![Ink::Attention; panel.lines.len()];
        details.push(panel);
    }
    if remaining >= 7 {
        writeln!(
            out,
            "\n  {}",
            ink("HOST INVENTORY", Ink::Label, terminal_colour_enabled())
        )?;
        remaining -= 2;
    }
    let bands = details.len().div_ceil(columns);
    for (index, pair) in details.chunks(columns).enumerate() {
        if remaining < 5 {
            break;
        }
        let allowance = (remaining / (bands - index)).saturating_sub(3).max(2);
        let height = pair
            .iter()
            .map(|panel| panel.lines.len().max(1))
            .max()
            .unwrap_or(1)
            .min(allowance);
        draw_panels(pair, cell, height, out)?;
        remaining -= height + 3;
    }
    Ok(())
}

pub(super) struct OverviewPanel {
    title: String,
    pub(super) lines: Vec<String>,
    accent: Ink,
    tones: Vec<Ink>,
}

impl OverviewPanel {
    pub(super) fn new(title: &str, lines: Vec<String>, accent: Ink) -> Self {
        Self {
            title: title.into(),
            lines,
            accent,
            tones: Vec::new(),
        }
    }
}

// Colour never participates in width calculations, and labels remain meaningful without it.
pub(super) fn style_frame_line(
    line: &str,
    title: &str,
    accent: Ink,
    body: Ink,
    enabled: bool,
) -> String {
    if line.starts_with(glyphs().frame_top_left) {
        let needle = format!(" {title} ");
        if let Some((before, after)) = line.split_once(&needle) {
            return format!(
                "{}{}{}",
                ink(before, Ink::Border, enabled),
                ink(&needle, accent, enabled),
                ink(after, Ink::Border, enabled)
            );
        }
    }
    if line.starts_with(glyphs().frame_vertical) {
        let mut chars = line.chars();
        let first = chars.next().unwrap_or(' ');
        let last = chars.next_back().unwrap_or(' ');
        return format!(
            "{}{}{}",
            ink(&first.to_string(), Ink::Border, enabled),
            ink(chars.as_str(), body, enabled),
            ink(&last.to_string(), Ink::Border, enabled)
        );
    }
    ink(line, Ink::Border, enabled)
}

pub(super) fn draw_panels(
    panels: &[OverviewPanel],
    width: usize,
    height: usize,
    out: &mut impl Write,
) -> Result<()> {
    let frames: Vec<_> = panels
        .iter()
        .map(|panel| {
            let actual_height = height.min(panel.lines.len().max(1));
            let title = truncate_text(&panel.title, width.saturating_sub(4));
            let mut content: Vec<_> = panel
                .lines
                .iter()
                .take(actual_height)
                .map(|line| truncate_text(&journal::display(line), width.saturating_sub(4)))
                .collect();
            let mut tones = panel.tones.clone();
            if content.is_empty() {
                content.push("No entries reported; see coverage limits".into());
                tones = vec![Ink::Muted];
            }
            if panel.lines.len() > actual_height && actual_height > 1 {
                content[actual_height - 1] = format!(
                    "+ {} more in specialist",
                    panel.lines.len() - actual_height + 1
                );
                tones.resize(actual_height, Ink::Bright);
                tones[actual_height - 1] = Ink::Muted;
            }
            content.resize(actual_height, String::new());
            panel_lines(&title, &content, width)
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    style_frame_line(
                        line,
                        &title,
                        panel.accent,
                        tones
                            .get(index.saturating_sub(1))
                            .copied()
                            .unwrap_or(Ink::Bright),
                        terminal_colour_enabled(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    writeln!(out)?;
    for row in 0..height + 2 {
        writeln!(
            out,
            "  {}",
            frames
                .iter()
                .map(|frame| frame.get(row).cloned().unwrap_or_else(|| " ".repeat(width)))
                .collect::<Vec<_>>()
                .join("  ")
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_hierarchy_keeps_borders_quiet_and_labels_independent_of_colour() {
        let raw = panel_lines("VM builder", &["CPU 12%  RAM 2GiB".into()], 50);
        let header = style_frame_line(&raw[0], "VM builder", Ink::Brand, Ink::Bright, true);
        assert!(header.contains(&ink(" VM builder ", Ink::Brand, true)));
        let prefix = raw[0].split_once(" VM builder ").expect("title").0;
        assert!(header.starts_with(&ink(prefix, Ink::Border, true)));
        let body = style_frame_line(&raw[1], "VM builder", Ink::Brand, Ink::Bright, true);
        assert!(body.contains("CPU 12%  RAM 2GiB"));
        assert!(!body.contains(Ink::Brand.foreground(false)));
        assert_eq!(
            style_frame_line(&raw[1], "VM builder", Ink::Brand, Ink::Bright, false),
            raw[1]
        );
    }

    #[test]
    fn extra_panels_follow_the_available_height_and_keep_stable_histories() {
        let mut snapshot = demo_snapshot();
        snapshot.processes.truncate(1);
        snapshot.processes[0].cpu_percent = 250.5;
        let mut history = WorkloadHistory::default();
        history.observe(&snapshot);
        let key = workloads(&snapshot)[0].key.clone();
        assert_eq!(history.0[&key].back(), Some(&25050));
        for width in [80, 120, 200] {
            for budget in [0, 7, 8, 15, 30, 80, 160] {
                let mut output = Vec::new();
                render_details(&snapshot, &history, width, budget, &mut output).expect("panels");
                assert!(output.iter().filter(|byte| **byte == b'\n').count() <= budget);
                let text = String::from_utf8(output).expect("UTF-8");
                assert!(text.lines().all(|line| line.chars().count() <= width));
                if budget >= 15 {
                    assert!(text.contains("CPU peak 250.5%"));
                }
                if budget < 8 {
                    assert!(text.is_empty());
                }
            }
        }
        snapshot.processes.clear();
        history.observe(&snapshot);
        assert!(history.0.is_empty());
    }

    #[test]
    fn resolves_vm_names_and_groups_services_without_hiding_normal_processes() {
        let mut snapshot = demo_snapshot();
        snapshot.processes.truncate(1);
        let mut process = snapshot.processes[0].clone();
        process.name = "qemu-system-x86".into();
        process.command_line = Some(
            "/usr/bin/qemu-system-x86_64 -name guest=builder-one,debug-threads=on -uuid abc".into(),
        );
        process.service = None;
        snapshot.processes = vec![process.clone()];
        let mut vm = demo_snapshot().containers[0].clone();
        vm.runtime = "libvirt".into();
        vm.id = "abc".into();
        vm.name = "builder-one".into();
        snapshot.containers = vec![vm];
        assert_eq!(workloads(&snapshot)[0].name, "VM builder-one");
        snapshot.containers[0].id = "abcd".into();
        assert_eq!(workloads(&snapshot)[0].name, "qemu-system-x86");
        process.name = "worker".into();
        process.command_line = None;
        process.pid = ProcessId(2);
        process.rss_bytes = 0;
        process.virtual_memory_bytes = 0;
        snapshot.processes.push(process);
        assert_eq!(workloads(&snapshot).len(), 1);
        snapshot.processes[0].service = Some(ServiceReference {
            name: "build.service".into(),
            inferred: false,
        });
        let mut second = snapshot.processes[0].clone();
        second.pid = ProcessId(9999);
        snapshot.processes.push(second);
        let rows = workloads(&snapshot);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].processes, 2);
        assert_eq!(rows[0].rss, snapshot.processes[0].rss_bytes * 2);
    }

    #[test]
    fn missing_access_is_explained_without_claiming_the_host_is_clear() {
        let mut snapshot = SystemSnapshot::empty("fixture");
        snapshot
            .collection_warnings
            .push("libvirt read-only socket access denied".into());
        snapshot.findings = diagnose(&snapshot);
        assert_eq!(snapshot.findings[0].id, "collection.incomplete");
        assert_eq!(snapshot.findings[0].severity, Severity::Information);
        let mut output = Vec::new();
        render_cockpit_content(
            &snapshot,
            &CpuActivity::default(),
            &NetworkActivity::default(),
            0,
            false,
            60,
            &mut output,
        )
        .expect("cockpit");
        let text = String::from_utf8(output).expect("text");
        assert!(text.contains("LIMITED COVERAGE"));
        assert!(!text.contains("ALL CLEAR"));
        assert!(!text.contains("0 CRITICAL"));
        let mut output = Vec::new();
        render_health_specialist(&snapshot, 0, true, 40, 100, &mut output).expect("health");
        let text = String::from_utf8(output).expect("text");
        assert!(text.contains("libvirt read-only socket access denied"));
        assert!(text.contains("WHAT TO CHECK"));
    }

    #[test]
    fn coverage_deduplicates_limits_and_preserves_the_reason() {
        let mut snapshot = demo_snapshot();
        snapshot.collection_warnings = vec![
            "libvirt access denied".into(),
            "libvirt access denied".into(),
        ];
        assert_eq!(limits(&snapshot), vec!["libvirt access denied"]);
        let mut out = Vec::new();
        render_coverage(&snapshot, 100, 10, &mut out).expect("coverage");
        let text = String::from_utf8(out).expect("text");
        assert!(text.contains("1 collection limit"));
        assert!(text.contains("libvirt access denied"));
    }
}
