# Operations guide

This guide is a quick route from an unfamiliar or misbehaving device to the Lens view that can
answer the next question. It assumes Lens is already installed; see [Installing](INSTALLING.md) for
Raspberry Pi OS, Debian, Ubuntu, other Linux systems and macOS.

## Start with the device overview

Run `lens` as your normal user. Confirm the host name and clock first, then read the host pulse,
busiest workloads and health summary. Data that is still being collected is labelled as loading.
Unavailable data is labelled separately; Lens does not turn a failed collection into a zero.

Open a selected domain with the arrow keys and `Enter`, or start its command directly:

| Question | Command |
| --- | --- |
| What needs attention on this device? | `lens` |
| Which process is consuming CPU or memory? | `lens-top` |
| Did a service fail or restart repeatedly? | `lens-services` |
| Is a Docker/Podman/nerdctl container missing, exited or stuck? | `lens-containers` |
| What was logged around a failure? | `lens-logs` |
| Which filesystem is under pressure? | `lens-disk` |
| Does the device have the expected address, route, listener or modem? | `lens-net` |
| Is the device hot, throttled, under-voltage or missing attached hardware? | `lens-hardware` |
| Is time synchronized and which DNS, identity or certificate context applies? | `lens-system` |
| What evidence triggered a warning? | `lens-health` |

Interactive views share the main controls: arrows or `j`/`k` move, `Enter` opens a row, `Esc` goes
back, `/` searches, `r` refreshes, `!` opens a local diagnostic shell, `?` shows contextual help and
`q` quits. The layout adds useful columns and rows as the terminal grows.

Cockpit CPU and network charts collect roughly once a second, with one sample per column.
History fills the chart as samples arrive, then scrolls at its left edge; widening the terminal
reveals retained older samples. Chart peaks describe the visible window. Cockpit summaries and
specialist inventories refresh in the background ten seconds after each completed collection.
Container details retain their two-second sampling interval. Slow collectors extend these intervals;
automatic inventory collections never overlap.

The overview groups processes belonging to the same service and identifies local QEMU VMs by
UUID. It hides idle Linux kernel workers and shows at most ten workloads; `lens-top` retains the
complete process list. CPU and RSS are summed within a workload (shared resident pages can be
counted more than once). Larger terminals add RAM, swap, real storage mounts and temperature. Spare height adds CPU history
charts for the busiest workloads, followed by VM allocations, temperature readings, service failures
and restart counts, network addresses, storage, flagged log entries and collection limits. Charts
use one sample per column, scale to their visible peak and identify 100% as one logical CPU.
Workload histories are local to this session and follow stable workload identities.

The overview uses subdued frames and bright readings, cyan CPU charts, green network activity and violet VM identities.
Amber marks collection limits or attention; red marks observed failures or exceeded critical
thresholds. State words, headings and units remain visible with `NO_COLOR` or ASCII output.
The overview omits sensor source paths; Hardware retains those details.

Inventory age and collection limits appear below health. A limited snapshot says LIMITED COVERAGE
when no actionable findings are visible. Open Health for the actual collection reasons and next
checks; NO ACTIONABLE ISSUES only describes checks with available source data.

A virtual bridge with no attached ports normally has no carrier. Lens keeps it in the network
inventory but does not warn that it is down unless it carries the default route. Unaddressed,
unused physical ports also do not generate interface-down warnings.

Lens follows the terminal's own background and automatically selects a contrasting palette when the
terminal publishes `COLORFGBG`. If a browser, serial console or terminal multiplexer does not report
its background, start any command with `--theme light` or `--theme dark`. Set `LENS_THEME=light` or
`LENS_THEME=dark` to apply the choice to every Lens command in that shell.

## A service is not working

1. Run `lens-services --service NAME` and inspect load, active and sub-state. A loaded service can
   still be failed or inactive.
2. Check whether the restart count is rising. A briefly running service may be in a restart loop.
3. Query logs over the time the failure occurred:

   ```sh
   lens-logs --since "30 minutes ago" --service NAME --severity error
   ```

   `--service` uses systemd attribution and is Linux-only. On macOS, use `--process` or `--filter`
   with the relevant launchd label or process name.
4. If a restart is appropriate on systemd Linux, review it before execution:

   ```sh
   lens-services --action restart --target exact.service --dry-run
   lens-services --action restart --target exact.service --yes
   ```

Lens invokes `systemctl` as the current user. Existing policy decides whether the action is allowed.

## The device is slow or under memory pressure

Open `lens-top`. Check host CPU, memory and load before blaming the first process in the list. Sort by
CPU for active compute pressure or memory for resident memory pressure. Open a process to compare its
current value with its bounded history and inspect its complete command and executable.

Useful command snapshots include:

```sh
lens-top --plain --sort cpu --limit 20
lens-top --json --sort memory --limit 20
lens-top --filter-name worker --min-cpu 5
```

A process CPU value can exceed 100% when it uses more than one logical CPU. RSS is resident memory;
it is usually more useful for immediate pressure than virtual address-space size. Processes can exit
between kernel reads, so unavailable per-process fields are normal on a busy machine.

Before signalling a process, verify its identity and use a dry run. Lens checks PID and start time
again immediately before delivery to avoid acting on a reused PID.

```sh
lens-top --signal term --pid 4242 --dry-run
lens-top --signal term --pid 4242 --yes
```

## Storage is full or writes are failing

Run `lens-disk` and open the affected mount. Check both byte capacity and inode use: a filesystem can
have free bytes but no free inodes. Confirm that the mount path is the one used by the application;
container, removable and simulator mounts can make the fullest entry irrelevant to the failure.

If space did not return after deleting a large file, inspect deleted-but-open files. A running
process can retain the underlying storage until it closes the file or exits. Lens reports the
process, path and retained size where the platform exposes them.

```sh
lens-disk --plain --filter /var
lens-disk --json | jq '{filesystems, warnings: .collection_warnings}'
```

## The device cannot connect—or exposes an unexpected port

Run `lens-net` and check in this order:

1. Is the expected interface up, does it have the expected address, and do the RX/TX charts show
   traffic when the application is active?
2. Is there a default route, and does it use the intended interface and gateway?
3. Is the application listening on the intended address and port?

A wildcard listener (`0.0.0.0` or `::`) accepts connections on all local addresses, but does not by
itself prove that a firewall, router or mobile network permits remote access. A loopback listener is
local to the device.

```sh
lens-net --plain --filter 443
lens-net --json | jq '{interfaces, routes, listeners, cellular_modems}'
```

On Linux, Lens queries ModemManager when `mmcli` is installed. It can show modem registration state,
access technology, signal quality, operator and SIM identifiers made available by ModemManager.
Treat ICCIDs and related SIM identifiers as sensitive inventory data before sharing output.

## Investigate logs without hiding collection failures

`lens-logs` reads a bounded snapshot rather than following indefinitely. The default is the newest
1,000 records; use `--limit 0` only with a deliberate time range or file when the complete result is
needed.

```sh
lens-logs --since "1 hour ago" --severity error
lens-logs --log-file /var/log/my-app.log --filter timeout
lens-logs --since "30 minutes ago" --limit 0 --json
```

On Linux, `--since` accepts `journalctl` time expressions. On macOS, use durations accepted by
`log show --last`, such as `30m` or `1h`. Linux journal priority and service attribution come from structured journal fields. File logs and
macOS logs may infer severity from message text. Missing priorities remain unknown.

If no records appear, remove filters and inspect `collection_warnings` before concluding that no logs
exist:

```sh
lens-logs --json | jq '{count: (.logs | length), warnings: .collection_warnings}'
```

## Use health findings as leads

`lens-health` combines process, service, log, storage, network and hardware checks. Open a finding to read the
observed evidence, related entity and suggested next check. The severity is a triage aid; follow the
evidence in the relevant specialist before taking action.

```sh
lens-health --plain
lens-health --filter disk
lens-health --json | jq '.findings'
```

Collection warnings produce an informational `collection.incomplete` finding with the unavailable
sources and suggested checks. They do not become a critical incident, and they prevent a limited
snapshot from claiming complete health coverage.

## Script without jq for common gates

Lens can assert on filtered results directly. Exit `3` means the policy missed; exit `1` still means
collection or action failure; exit `2` means bad flags or an ambiguous action target.

```sh
lens-services --name nginx.service --active active --fail-if-empty --quiet
lens-top --filter-state zombie --fail-if-any --quiet
lens-net --listening --port 22 --expect-count-min 1 --quiet
lens-disk --mount / --min-used-percent 90 --fail-if-any --quiet
lens-health --fail-on critical --fail-on-collection-warnings --quiet
lens-services --json --fields services,findings
```

Use `--match exact` when a substring filter is too loose. Combine selectors with `--dry-run` actions
to resolve a unique target before `--yes`.

## Capture a result for support or automation

Use `--once` when you want the same command to print one readable snapshot instead of opening its
interactive screen. Use `--plain` when you want to explicitly select human-readable output, and
`--json` for the shared schema-versioned document. Redirected output defaults to plain text. JSON
preserves entities, findings, relationships and collection warnings so scripts do not have to parse
the terminal layout. `--fields` projects JSON/JSONL; `--quiet` keeps only the exit status.

```sh
lens --plain > lens-snapshot.txt
lens-health --json > lens-health.json
lens-health --fail-on critical --quiet
```

Review output before sharing it. Process commands, user names, host names, addresses, log messages
and SIM identifiers can contain operationally sensitive information.

The precise output contract is in [JSON schema](JSON_SCHEMA.md). Exit status, standard streams and
one-shot semantics are in the [command-line contract](CLI_CONTRACT.md). Guardrails for process and
service changes are in [Operational actions](ACTIONS.md). For platform-specific availability and
common failure modes, see [Compatibility](COMPATIBILITY.md) and
[Troubleshooting](TROUBLESHOOTING.md).


## Follow the evidence between views

Specialists show a compact summary of the collected domain. Inspecting an item adds related
facts and next checks when there is spare space; missing access remains visible. Press `g` in a
service detail to open its collected logs, in an attributed log to inspect its service, or in a
health finding to open its related storage, network, service or instance view. These are read-only
navigation steps. Selected services, instances, mounts and interfaces remain selected across refreshes.

Storage puts persistent filesystems before memory/pseudo mounts and physical/logical disks before
loop images. Wider lists show total and available bytes plus inode use. `Tab` jumps between mounts
and devices. Inspect a filesystem to see its partition/device chain, reserved/overhead space and
visible deleted-open files; inspect a device to see its model, parent and read-only/media flags.
RAID controllers may expose only a logical disk. These checks do not inspect SMART or RAID members.
Inode pressure creates a health finding independently of free bytes. Linux storage detail also
samples kernel disk counters about once a second, with separate read/write charts for the selected
partition or device. Rates use elapsed time; missing/reset/replaced devices clear the sampling
baseline. Kernel active time is explicitly distinct from latency, queue depth or saturation.
These rates are session-local; one-shot JSON exposes cumulative counters.

Network puts default-route interfaces first. `Tab` jumps between interfaces, routes, listeners
and modems. Inspect a link for carrier, negotiated speed/duplex, MAC/MTU, bridge membership, attached
routes and bound listeners. A physically unconnected, unused port and an empty bridge are distinct
from a configured uplink failure. Error/drop counters are cumulative, so compare samples before
calling them a current problem. The aggregate history uses default-route interfaces when present, with an explicit UPLINK label.
Without a default route it says ALL LINKS; interface/bridge traffic may then be counted at more
than one layer. Per-interface rates remain available in either case.
Listener scope distinguishes loopback, wildcard, interface-bound and specific-address sockets;
it is not a firewall or reachability test. IPv4 and IPv6 routes are collected when `ip` is available.

Services lead with failures and restart counts by default and show attributed process/RSS context
in detail. Restart counts are cumulative, not proof of an active loop. Process detail includes its
visible parent and direct children, with child CPU and summed RSS. Logs wrap full messages instead
of clipping them to one line, and keep adjacent-source context inside the collected window.
Hardware adds thermal headroom without claiming fan/PSU/disk health coverage. System highlights
clock synchronization, configured resolvers and explicit privileged-group membership; configured
DNS is not proof of successful resolution, and group lists do not capture all access policy.
