//! Evidence and relationships that turn inventory into a useful investigation.
use super::*;
use overview::OverviewPanel;

pub(super) fn persistent_mount(mount: &Mount) -> bool {
    actionable_mount(mount)
        && !matches!(
            mount.filesystem.as_str(),
            "tmpfs" | "ramfs" | "efivarfs" | "squashfs" | "overlay" | "aufs"
        )
        && !mount.source.starts_with("/dev/loop")
}
pub(super) fn inode_percent(mount: &Mount) -> Option<f64> {
    Some(mount.inode_used? as f64 * 100.0 / mount.inode_total.filter(|n| *n > 0)? as f64)
}
pub(super) fn binding(socket: &Socket) -> &'static str {
    let host = endpoint_host(&socket.local);
    let (address, scope) = host
        .split_once('%')
        .map_or((host, None), |(address, scope)| (address, Some(scope)));
    if address
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
    {
        "local only"
    } else if matches!(address, "0.0.0.0" | "::" | "*") {
        if scope.is_some() {
            "interface-bound"
        } else {
            "all addresses"
        }
    } else {
        "specific address"
    }
}
pub(super) fn network_role(snapshot: &SystemSnapshot, interface: &Interface) -> String {
    if snapshot.routes.iter().any(|route| {
        (route.destination == "default"
            || route.destination == "0.0.0.0/0"
            || route.destination == "::/0")
            && route.interface.as_deref() == Some(&interface.name)
    }) {
        return "DEFAULT ROUTE".into();
    }
    let Some(details) = snapshot.network_details.get(&interface.name) else {
        return if interface.idle_bridge {
            "idle bridge"
        } else {
            "interface"
        }
        .into();
    };
    if details.kind == "bridge" {
        return format!("bridge / {} ports", details.ports.len());
    }
    if let Some(master) = &details.master {
        return format!("member of {master}");
    }
    if details.kind == "physical"
        && details.carrier == Some(false)
        && interface.addresses.is_empty()
    {
        return "unconnected / unused".into();
    }
    details.kind.clone()
}

pub(super) fn summary(view: View, snapshot: &SystemSnapshot) -> String {
    match view {
        View::Disk => {
            let real: Vec<_> = snapshot
                .mounts
                .iter()
                .filter(|m| persistent_mount(m))
                .collect();
            let fullest = real
                .iter()
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
            format!(
                "{} persistent mounts | {} other mounts | {} devices | {} deleted-open files{}",
                real.len(),
                snapshot.mounts.len() - real.len(),
                snapshot.block_devices.len(),
                snapshot.deleted_open_files.len(),
                fullest
                    .map(|m| format!(" | fullest {} {:.0}%", m.target, m.used_percent))
                    .unwrap_or_default()
            )
        }
        View::Net => {
            let default = snapshot
                .routes
                .iter()
                .find(|r| matches!(r.destination.as_str(), "default" | "0.0.0.0/0" | "::/0"));
            let all = snapshot
                .sockets
                .iter()
                .filter(|s| binding(s) == "all addresses")
                .count();
            format!(
                "Default {} via {} | {} interfaces | {} listeners ({} bind all addresses)",
                default
                    .and_then(|r| r.interface.as_deref())
                    .unwrap_or("not reported"),
                default
                    .and_then(|r| r.gateway.as_deref())
                    .unwrap_or("not reported"),
                snapshot.interfaces.len(),
                snapshot.sockets.len(),
                all
            )
        }
        View::Services => format!(
            "{} services | {} failed | {} with restarts | {} attributed processes",
            snapshot.services.len(),
            snapshot
                .services
                .iter()
                .filter(|s| s.active == "failed")
                .count(),
            snapshot
                .services
                .iter()
                .filter(|s| s.restart_count.unwrap_or(0) > 0)
                .count(),
            snapshot
                .processes
                .iter()
                .filter(|p| p.service.is_some())
                .count()
        ),
        View::Containers => format!(
            "{} instances | {} running | {} stopped | {} other states",
            snapshot.containers.len(),
            snapshot
                .containers
                .iter()
                .filter(|c| c.state == "running")
                .count(),
            snapshot
                .containers
                .iter()
                .filter(|c| c.state == "exited")
                .count(),
            snapshot
                .containers
                .iter()
                .filter(|c| !matches!(c.state.as_str(), "running" | "exited"))
                .count()
        ),
        View::Logs => {
            let flagged = snapshot
                .logs
                .iter()
                .filter(|log| {
                    matches!(
                        log.priority.as_deref(),
                        Some("error" | "critical" | "warning")
                    )
                })
                .count();
            let repeats: u64 = snapshot.logs.iter().map(|log| log.repeated.max(1)).sum();
            format!(
                "{} records | {} flagged | {} occurrences in collected window | Enter reads full message",
                snapshot.logs.len(),
                flagged,
                repeats
            )
        }
        View::Hardware => format!(
            "{} sensors | {} attached devices | {} | firmware {}",
            snapshot.temperatures.len(),
            snapshot.hardware_devices.len(),
            snapshot
                .temperatures
                .iter()
                .max_by(|a, b| a.temperature_c.total_cmp(&b.temperature_c))
                .map(|s| format!("hottest {} {:.1}C", s.name, s.temperature_c))
                .unwrap_or_else(|| "temperature unavailable".into()),
            snapshot
                .hardware
                .firmware_version
                .as_deref()
                .unwrap_or("not reported")
        ),
        View::System => format!(
            "Clock {} | {} DNS servers | {} accounts | {} groups | {} public certificates",
            snapshot
                .clock
                .ntp_synchronized
                .map(|yes| if yes {
                    "synchronized"
                } else {
                    "NOT synchronized"
                })
                .unwrap_or("sync unknown"),
            snapshot.dns.nameservers.len(),
            snapshot.accounts.len(),
            snapshot.groups.len(),
            snapshot.certificates.len()
        ),
        View::Health => format!(
            "{} critical | {} attention | {} information | {} collection limits",
            snapshot
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Critical)
                .count(),
            snapshot
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Attention)
                .count(),
            snapshot
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Information)
                .count(),
            overview::limits(snapshot).len()
        ),
        View::Processes => String::new(),
    }
}

pub(super) fn followup(
    view: View,
    snapshot: &SystemSnapshot,
    selected: usize,
) -> Option<(View, String)> {
    match view {
        View::Services => Some((View::Logs, snapshot.services.get(selected)?.name.clone())),
        View::Logs => {
            let unit = snapshot.logs.get(selected)?.unit.as_ref()?;
            unit.ends_with(".service")
                .then(|| (View::Services, unit.clone()))
        }
        View::Health => snapshot
            .findings
            .get(selected)?
            .related_entities
            .iter()
            .find_map(|entity| match entity {
                EntityId::Mount(name) => Some((View::Disk, name.clone())),
                EntityId::BlockDevice(name) => Some((View::Disk, name.clone())),
                EntityId::Service(name) => Some((View::Services, name.clone())),
                EntityId::Interface(name) => Some((View::Net, name.clone())),
                EntityId::Container(name) => Some((View::Containers, name.clone())),
                _ => None,
            }),
        _ => None,
    }
}

fn mount_for<'a>(snapshot: &'a SystemSnapshot, path: &str) -> Option<&'a Mount> {
    snapshot
        .mounts
        .iter()
        .filter(|mount| {
            path == mount.target
                || (mount.target == "/" && path.starts_with('/'))
                || path
                    .strip_prefix(&mount.target)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
        .max_by_key(|mount| mount.target.len())
}

fn cards(view: View, snapshot: &SystemSnapshot, selected: usize) -> Vec<OverviewPanel> {
    let mut cards = Vec::new();
    let mut add = |title: &str, lines: Vec<String>, accent| {
        if !lines.is_empty() {
            cards.push(OverviewPanel::new(title, lines, accent));
        }
    };
    match view {
        View::Disk => {
            if let Some(mount) = snapshot.mounts.get(selected) {
                let mut lines = vec![format!(
                    "{} total | {} available | {} used",
                    human_bytes(mount.total_bytes),
                    human_bytes(mount.available_bytes),
                    human_bytes(mount.used_bytes)
                )];
                if let Some(percent) = inode_percent(mount) {
                    lines.push(format!(
                        "Inodes {:.1}% used{}",
                        percent,
                        if percent >= 90.0 {
                            " — files can fail even with free bytes"
                        } else {
                            ""
                        }
                    ));
                }
                if mount.total_bytes > mount.used_bytes.saturating_add(mount.available_bytes) {
                    lines.push(format!(
                        "{} outside user-available space (reserved/overhead)",
                        human_bytes(mount.total_bytes - mount.used_bytes - mount.available_bytes)
                    ));
                }
                lines.push(
                    if persistent_mount(mount) {
                        "Persistent filesystem: capacity and inode pressure matter"
                    } else {
                        "Memory-backed or pseudo filesystem: separate from persistent disk capacity"
                    }
                    .into(),
                );
                add("CAPACITY / INODES", lines, Ink::Info);
                let name = snapshot
                    .block_devices
                    .iter()
                    .find(|device| {
                        device
                            .mountpoints
                            .iter()
                            .any(|target| target == &mount.target)
                    })
                    .map(|device| device.name.as_str())
                    .or_else(|| mount.source.strip_prefix("/dev/"));
                let mut devices = Vec::new();
                if let Some(name) = name {
                    let mut current = Some(name.to_owned());
                    for _ in 0..8 {
                        let Some(name) = current.take() else { break };
                        let Some(device) = snapshot.block_devices.iter().find(|d| d.name == name)
                        else {
                            break;
                        };
                        let meta = snapshot.storage_details.get(&name);
                        devices.push(format!(
                            "{} [{}] {} {}",
                            device.name,
                            device.kind,
                            human_bytes(device.size_bytes),
                            meta.and_then(|m| m.model.as_deref()).unwrap_or("")
                        ));
                        current = meta.and_then(|m| m.parent.clone());
                    }
                }
                if devices.is_empty() {
                    devices.push("Backing device mapping is not reported for this source".into());
                }
                devices.push(
                    "Capacity is filesystem data; SMART/RAID member health is not checked".into(),
                );
                add("BACKING DEVICES", devices, Ink::Brand);
                let deleted = snapshot
                    .deleted_open_files
                    .iter()
                    .filter(|file| {
                        mount_for(snapshot, &file.path)
                            .is_some_and(|owner| owner.target == mount.target)
                    })
                    .map(|file| {
                        format!(
                            "PID {} {} {} {}",
                            file.pid
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "?".into()),
                            file.command,
                            file.size_bytes
                                .map(human_bytes)
                                .unwrap_or_else(|| "size unknown".into()),
                            file.path
                        )
                    })
                    .collect();
                add("DELETED FILES STILL HELD OPEN", deleted, Ink::Attention);
            } else if let Some(device) = snapshot
                .block_devices
                .get(selected.saturating_sub(snapshot.mounts.len()))
            {
                let mut lines = vec![format!(
                    "{} {} {}",
                    device.name,
                    device.kind,
                    human_bytes(device.size_bytes)
                )];
                if let Some(meta) = snapshot.storage_details.get(&device.name) {
                    if let Some(model) = &meta.model {
                        lines.push(format!("Model: {model}"));
                    }
                    if let Some(parent) = &meta.parent {
                        lines.push(format!("Parent device: {parent}"));
                    }
                    if let Some(rotational) = meta.rotational {
                        lines.push(format!(
                            "Kernel media flag: {}",
                            if rotational {
                                "rotational / controller-reported"
                            } else {
                                "non-rotational"
                            }
                        ));
                    }
                    if let Some(ro) = meta.read_only {
                        lines.push(format!("Read-only: {ro}"));
                    }
                    if let (Some(read), Some(write)) = (meta.read_bytes, meta.write_bytes) {
                        lines.push(format!(
                            "Cumulative I/O: {} read / {} written (not rates)",
                            human_bytes(read),
                            human_bytes(write)
                        ));
                    }
                }
                lines.push(
                    "RAID controllers may expose a logical volume, not individual drives".into(),
                );
                add("DEVICE IDENTITY / LIMITS", lines, Ink::Brand);
                add(
                    "CHILD PARTITIONS",
                    snapshot
                        .storage_details
                        .iter()
                        .filter(|(_, meta)| meta.parent.as_deref() == Some(&device.name))
                        .map(|(name, _)| name.clone())
                        .collect(),
                    Ink::Label,
                );
            }
            add("NEXT CHECK",vec!["If bytes are free, check inode exhaustion and reserved space first".into(),"Deleted-open space returns when its owner closes the file; do not delete it again".into()],Ink::Label);
        }
        View::Net => {
            let interfaces = snapshot.interfaces.len();
            let routes = interfaces + snapshot.routes.len();
            if let Some(interface) = snapshot.interfaces.get(selected) {
                let mut lines = vec![format!("Role: {}", network_role(snapshot, interface))];
                if let Some(meta) = snapshot.network_details.get(&interface.name) {
                    lines.push(format!(
                        "Carrier: {} | speed: {} | duplex: {}",
                        meta.carrier
                            .map(|up| if up { "present" } else { "absent" })
                            .unwrap_or("unknown"),
                        meta.speed_mbps
                            .map(|n| format!("{n} Mb/s"))
                            .unwrap_or_else(|| "unknown".into()),
                        meta.duplex.as_deref().unwrap_or("unknown")
                    ));
                    lines.push(format!(
                        "MAC {} | MTU {}",
                        meta.mac_address.as_deref().unwrap_or("unknown"),
                        meta.mtu
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    ));
                    if !meta.ports.is_empty() {
                        lines.push(format!("Bridge ports: {}", meta.ports.join(", ")));
                    }
                    lines.push(format!(
                        "Cumulative errors RX {} / TX {} | drops RX {} / TX {}",
                        counter(meta.rx_errors),
                        counter(meta.tx_errors),
                        counter(meta.rx_dropped),
                        counter(meta.tx_dropped)
                    ));
                    lines.push("Error counters are lifetime totals; compare samples before diagnosing a current fault".into());
                }
                add("LINK / TOPOLOGY", lines, Ink::Success);
                add(
                    "ROUTES USING THIS INTERFACE",
                    snapshot
                        .routes
                        .iter()
                        .filter(|r| r.interface.as_deref() == Some(&interface.name))
                        .map(|r| r.raw.clone())
                        .collect(),
                    Ink::Info,
                );
                add(
                    "BOUND LISTENERS",
                    snapshot
                        .sockets
                        .iter()
                        .filter(|s| socket_on_interface(s, interface))
                        .map(|s| {
                            format!(
                                "{} {} [{}] {}",
                                s.protocol,
                                s.local,
                                binding(s),
                                s.owner.as_deref().unwrap_or("owner not permitted/reported")
                            )
                        })
                        .collect(),
                    Ink::Label,
                );
            } else if selected < routes {
                let route = &snapshot.routes[selected - interfaces];
                let lines = snapshot
                    .interfaces
                    .iter()
                    .filter(|i| Some(i.name.as_str()) == route.interface.as_deref())
                    .map(|i| {
                        format!(
                            "{} [{}] {} | {}",
                            i.name,
                            i.state,
                            network_role(snapshot, i),
                            i.addresses.join(" ")
                        )
                    })
                    .collect();
                add("OUTGOING LINK", lines, Ink::Success);
                add("REACHABILITY",vec!["A configured route is not proof that the gateway or Internet is reachable".into(),"Check link carrier, address and DNS before testing an external destination".into()],Ink::Label);
            } else if let Some(socket) = snapshot.sockets.get(selected - routes) {
                let mut lines = vec![format!("Binding scope: {}", binding(socket))];
                lines.push(match binding(socket){"local only"=>"Other hosts cannot connect directly to this loopback binding", "all addresses"=>"Accepts traffic addressed to local interfaces; firewall/NAT still control reachability", _=>"Bound to a specific local address; check the owning interface and route"}.into());
                if socket.owner.is_none() {
                    lines.push("Owner unavailable under this account; this does not mean no process owns the socket".into());
                }
                if let Some(pid) = socket.process_id {
                    if let Some(process) = snapshot.processes.iter().find(|p| p.pid == pid) {
                        lines.push(format!(
                            "PID {pid} {} | service {}",
                            process.name,
                            process
                                .service
                                .as_ref()
                                .map(|s| s.name.as_str())
                                .unwrap_or("not attributed")
                        ));
                    }
                }
                add("EXPOSURE / OWNER", lines, Ink::Info);
            }
            add("INTERPRETATION",vec!["No carrier on an unused physical port or empty bridge is usually expected".into(),"VM bridge/tap traffic can be counted at multiple layers; do not sum it as uplink traffic".into()],Ink::Label);
        }
        View::Services => {
            if let Some(service) = snapshot.services.get(selected) {
                let processes: Vec<_> = snapshot
                    .processes
                    .iter()
                    .filter(|p| p.service.as_ref().is_some_and(|s| s.name == service.name))
                    .collect();
                let rss = processes
                    .iter()
                    .fold(0u64, |sum, p| sum.saturating_add(p.rss_bytes));
                add(
                    "ATTRIBUTED PROCESSES",
                    std::iter::once(format!(
                        "{} processes | {} summed RSS (shared pages may repeat)",
                        processes.len(),
                        human_bytes(rss)
                    ))
                    .chain(processes.iter().map(|p| {
                        format!(
                            "PID {} {} {}",
                            p.pid,
                            p.name,
                            p.command_line.as_deref().unwrap_or("")
                        )
                    }))
                    .collect(),
                    Ink::Info,
                );
                add("NEXT CHECK",vec!["g opens this service's collected logs; inspect the failure before restarting".into(),"A restart count is cumulative, not proof of a current restart loop".into()],Ink::Label);
            }
        }
        View::Containers => {
            if let Some(vm) = snapshot.containers.get(selected) {
                if let Some(resources) = &vm.resources {
                    add("STORAGE ATTACHMENTS", resources.mounts.clone(), Ink::Brand);
                    add("RESOURCE MEANING",vec!["CPU 100% is one logical CPU; compare usage with configured allocation".into(),if vm.runtime=="libvirt"{"Host RSS is not guest RAM usage; image allocation is not guest filesystem use"}else{"Unset limits mean shared resources; missing counters mean unavailable"}.into(),"Guest services, filesystems and application health are not inspected".into()],Ink::Label);
                }
            }
        }
        View::Logs => {
            if let Some(log) = snapshot.logs.get(selected) {
                let same: Vec<_> = snapshot
                    .logs
                    .iter()
                    .filter(|other| other.unit == log.unit && other.source == log.source)
                    .collect();
                add(
                    "SAME SOURCE IN THIS WINDOW",
                    same.iter()
                        .rev()
                        .take(6)
                        .map(|log| format!("{} {}", log.timestamp, log.message))
                        .collect(),
                    Ink::Label,
                );
                add("INTERPRETATION",vec![format!("This record represents {} occurrence(s); counts cover only the collected window",log.repeated.max(1)),"Linux journal levels are recorded priorities; file/macOS levels may be inferred".into(),"g opens the attributed service when present".into()],Ink::Label);
            }
        }
        View::Hardware => {
            add(
                "THERMAL HEADROOM",
                snapshot
                    .temperatures
                    .iter()
                    .map(|sensor| {
                        format!(
                            "{} {:.1}C | {}",
                            sensor.name,
                            sensor.temperature_c,
                            sensor
                                .critical_c
                                .or(sensor.max_c)
                                .map(|limit| format!(
                                    "{:.1}C to reported threshold",
                                    limit - sensor.temperature_c
                                ))
                                .unwrap_or_else(|| "hardware threshold not reported".into())
                        )
                    })
                    .collect(),
                Ink::Label,
            );
            add(
                "COVERAGE",
                vec![
                    "Sensor temperatures do not certify fan, PSU, RAID or SMART health".into(),
                    "Firmware version is inventory; it does not establish update availability"
                        .into(),
                ],
                Ink::Label,
            );
        }
        View::System => {
            add(
                "TIME / NAME RESOLUTION",
                vec![
                    format!(
                        "NTP synchronization: {}",
                        snapshot
                            .clock
                            .ntp_synchronized
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "not reported".into())
                    ),
                    format!(
                        "Configured resolvers: {}",
                        snapshot.dns.nameservers.join(", ")
                    ),
                    "Configured DNS servers are not proof that DNS queries succeed".into(),
                ],
                Ink::Info,
            );
            add(
                "PRIVILEGED GROUPS",
                snapshot
                    .groups
                    .iter()
                    .filter(|g| {
                        matches!(
                            g.name.as_str(),
                            "sudo" | "wheel" | "admin" | "docker" | "lxd" | "libvirt"
                        )
                    })
                    .map(|g| {
                        format!(
                            "{}: {}",
                            g.name,
                            if g.members.is_empty() {
                                "no explicit members".into()
                            } else {
                                g.members.join(", ")
                            }
                        )
                    })
                    .collect(),
                Ink::Label,
            );
            add("NEXT CHECK",vec!["Check clock sync before investigating certificate validity or authentication".into(),"Group membership is inventory; primary groups and external policy may grant additional access".into()],Ink::Label);
        }
        View::Health => {
            add("INVESTIGATION",vec!["Follow the selected finding's evidence, then press g for its related specialist".into(),"Severity helps triage; collection limits describe checks that could not run".into()],Ink::Label);
        }
        View::Processes => {}
    }
    let reasons = overview::limits(snapshot);
    add(
        "COLLECTION LIMITS",
        reasons.into_iter().map(str::to_owned).collect(),
        Ink::Attention,
    );
    cards
}
fn counter(value: Option<u64>) -> String {
    value
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unknown".into())
}
fn socket_on_interface(socket: &Socket, interface: &Interface) -> bool {
    let host = endpoint_host(&socket.local);
    let address = if let Some((address, scope)) = host.split_once('%') {
        if scope != interface.name {
            return false;
        }
        if matches!(address, "0.0.0.0" | "::" | "*") {
            return true;
        }
        address
    } else {
        host
    };
    if binding(socket) == "all addresses" {
        return true;
    }
    interface
        .addresses
        .iter()
        .any(|value| value.split('/').next() == Some(address))
}

pub(super) fn render(
    view: View,
    snapshot: &SystemSnapshot,
    selected: usize,
    width: usize,
    budget: usize,
    out: &mut impl Write,
) -> Result<()> {
    if budget < 6 {
        return Ok(());
    }
    let cards = cards(view, snapshot, selected);
    let columns = if width >= 120 { 2 } else { 1 };
    let cell = width.saturating_sub(4 + 2 * (columns - 1)) / columns;
    let bands = cards.len().div_ceil(columns);
    let mut remaining = budget;
    for (index, pair) in cards.chunks(columns).enumerate() {
        if remaining < 5 {
            break;
        }
        let allowance = (remaining / (bands - index)).saturating_sub(3).max(2);
        let height = pair
            .iter()
            .map(|p| p.lines.len())
            .max()
            .unwrap_or(0)
            .min(allowance);
        overview::draw_panels(pair, cell, height, out)?;
        remaining -= height + 3;
    }
    Ok(())
}

pub(super) fn next_section(
    view: View,
    snapshot: &SystemSnapshot,
    selected: usize,
) -> Option<usize> {
    let bounds = match view {
        View::Disk => vec![
            (0, snapshot.mounts.len()),
            (snapshot.mounts.len(), snapshot.block_devices.len()),
        ],
        View::Net => vec![
            (0, snapshot.interfaces.len()),
            (snapshot.interfaces.len(), snapshot.routes.len()),
            (
                snapshot.interfaces.len() + snapshot.routes.len(),
                snapshot.sockets.len(),
            ),
            (
                snapshot.interfaces.len() + snapshot.routes.len() + snapshot.sockets.len(),
                snapshot.cellular_modems.len(),
            ),
        ],
        _ => return None,
    };
    let starts: Vec<_> = bounds
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(start, _)| start)
        .collect();
    starts
        .iter()
        .copied()
        .find(|start| *start > selected)
        .or_else(|| starts.first().copied())
}

pub(super) fn selection_key(view: View, snapshot: &SystemSnapshot, index: usize) -> Option<String> {
    Some(match view {
        View::Services => format!("service:{}", snapshot.services.get(index)?.name),
        View::Containers => {
            let c = snapshot.containers.get(index)?;
            format!("container:{}:{}", c.runtime, c.id)
        }
        View::Disk => {
            if let Some(m) = snapshot.mounts.get(index) {
                format!("mount:{}", m.target)
            } else {
                format!(
                    "block:{}",
                    snapshot
                        .block_devices
                        .get(index.checked_sub(snapshot.mounts.len())?)?
                        .name
                )
            }
        }
        View::Net => {
            if let Some(i) = snapshot.interfaces.get(index) {
                format!("interface:{}", i.name)
            } else {
                let index = index.checked_sub(snapshot.interfaces.len())?;
                if let Some(r) = snapshot.routes.get(index) {
                    format!("route:{}:{:?}:{:?}", r.destination, r.interface, r.gateway)
                } else {
                    let index = index.checked_sub(snapshot.routes.len())?;
                    if let Some(s) = snapshot.sockets.get(index) {
                        format!("socket:{}:{}", s.protocol, s.local)
                    } else {
                        format!(
                            "modem:{}",
                            snapshot
                                .cellular_modems
                                .get(index.checked_sub(snapshot.sockets.len())?)?
                                .path
                        )
                    }
                }
            }
        }
        View::Hardware => {
            let rows = hardware_rows(snapshot);
            let r = rows.get(index)?;
            format!("hardware:{}:{}", r.0, r.2)
        }
        _ => return None,
    })
}

/// Wrap details instead of discarding the most useful part of a long log message.
pub(super) fn wrapped(text: &str, width: usize, maximum: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for source in text.lines() {
        let chars: Vec<_> = source
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if chars.is_empty() {
            lines.push(String::new());
        }
        for chunk in chars.chunks(width) {
            lines.push(chunk.iter().collect());
        }
    }
    if lines.len() > maximum && maximum > 0 {
        lines.truncate(maximum);
        lines[maximum - 1] = "Message continues in --json output".into();
    }
    lines
        .into_iter()
        .map(|line| truncate_text(&line, width))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_network_history_follows_the_default_route_not_vm_taps() {
        let mut snapshot = demo_snapshot();
        snapshot.interfaces.truncate(1);
        snapshot.interfaces[0].name = "uplink0".into();
        snapshot.interfaces[0].rx_bytes = Some(100);
        snapshot.interfaces[0].tx_bytes = Some(200);
        let mut tap = snapshot.interfaces[0].clone();
        tap.name = "vnet0".into();
        snapshot.interfaces.push(tap);
        snapshot.routes.truncate(1);
        snapshot.routes[0].destination = "default".into();
        snapshot.routes[0].interface = Some("uplink0".into());
        let mut activity = NetworkActivity::default();
        let now = Instant::now();
        activity.set_scope(&snapshot);
        activity.observe(&snapshot.interfaces, now);
        snapshot.interfaces[0].rx_bytes = Some(1124);
        snapshot.interfaces[0].tx_bytes = Some(2248);
        snapshot.interfaces[1].rx_bytes = Some(1000100);
        snapshot.interfaces[1].tx_bytes = Some(2000200);
        activity.observe(&snapshot.interfaces, now + Duration::from_secs(2));
        assert_eq!(activity.current(), Some((512, 1024)));
        assert_eq!(activity.scope_label(), "UPLINK uplink0");
        assert!(activity.interface_rate("vnet0").is_some());
        snapshot.routes.clear();
        activity.set_scope(&snapshot);
        assert!(activity.current().is_none());
        assert_eq!(activity.scope_label(), "ALL LINKS");
    }

    #[test]
    fn distinguishes_loopback_wildcard_and_interface_scoped_listeners() {
        let mut socket = demo_snapshot().sockets[0].clone();
        let mut interface = demo_snapshot().interfaces[0].clone();
        interface.name = "br-guests".into();
        interface.addresses = vec!["10.44.0.1/24".into()];
        socket.local = "127.99.0.1:9000".into();
        assert_eq!(binding(&socket), "local only");
        assert!(!socket_on_interface(&socket, &interface));
        socket.local = "[::1]:9000".into();
        assert_eq!(binding(&socket), "local only");
        socket.local = "0.0.0.0:9000".into();
        assert_eq!(binding(&socket), "all addresses");
        assert!(socket_on_interface(&socket, &interface));
        socket.local = "0.0.0.0%br-guests:67".into();
        assert_eq!(binding(&socket), "interface-bound");
        assert!(socket_on_interface(&socket, &interface));
        interface.name = "uplink".into();
        assert!(!socket_on_interface(&socket, &interface));
    }
    #[test]
    fn mount_attribution_respects_path_boundaries_and_longest_match() {
        let mut snapshot = demo_snapshot();
        let mut mount = snapshot.mounts[0].clone();
        mount.target = "/data".into();
        snapshot.mounts.push(mount.clone());
        mount.target = "/data/guests".into();
        snapshot.mounts.push(mount);
        assert_eq!(
            mount_for(&snapshot, "/data/guests/disk.qcow2").map(|m| m.target.as_str()),
            Some("/data/guests")
        );
        assert_eq!(
            mount_for(&snapshot, "/database/file").map(|m| m.target.as_str()),
            Some("/")
        );
    }
    #[test]
    fn persistent_mounts_and_uplink_lead_and_selection_survives_refresh() {
        let mut previous = demo_snapshot();
        let mut pseudo = previous.mounts[0].clone();
        pseudo.target = "/run".into();
        pseudo.source = "tmpfs".into();
        pseudo.filesystem = "tmpfs".into();
        pseudo.used_percent = 100.0;
        previous.mounts.insert(0, pseudo);
        let mut next = previous.clone();
        sort_snapshot_domain(View::Disk, &mut next, None);
        assert_eq!(next.mounts[0].target, "/");
        assert_eq!(
            preserve_specialist_selection(View::Disk, &previous, &next, 0),
            1
        );
        assert_eq!(next_section(View::Disk, &next, 0), Some(next.mounts.len()));
        next.block_devices.clear();
        assert_eq!(next_section(View::Disk, &next, 0), Some(0));
    }
    #[test]
    fn inode_exhaustion_is_independent_of_free_disk_bytes() {
        let mut snapshot = demo_snapshot();
        snapshot.mounts[0].used_percent = 10.0;
        snapshot.mounts[0].inode_total = Some(1000);
        snapshot.mounts[0].inode_used = Some(980);
        let findings = diagnose(&snapshot);
        assert!(
            findings
                .iter()
                .any(|f| f.id.starts_with("disk.inodes.") && f.severity == Severity::Critical)
        );
        snapshot.mounts[0].inode_total = Some(0);
        assert_eq!(inode_percent(&snapshot.mounts[0]), None);
    }
    #[test]
    fn inspection_panels_are_bounded_for_every_specialist_and_link_to_evidence() {
        let snapshot = demo_snapshot();
        assert_eq!(
            followup(View::Services, &snapshot, 0).map(|p| p.0),
            Some(View::Logs)
        );
        for view in View::ALL {
            for width in [40, 80, 120, 200] {
                for budget in [0, 6, 12, 40, 120] {
                    let mut out = Vec::new();
                    render(view, &snapshot, 0, width, budget, &mut out).expect("insights");
                    assert!(out.iter().filter(|b| **b == b'\n').count() <= budget);
                    assert!(
                        String::from_utf8(out)
                            .expect("text")
                            .lines()
                            .all(|line| line.chars().count() <= width)
                    );
                }
            }
        }
        let lines = wrapped("a very long log message\n\x1b details", 8, 20);
        assert!(lines.iter().all(|line| line.chars().count() <= 8));
        assert!(!lines.join("").contains('\x1b'));
    }
}
