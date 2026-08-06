# Operations guide

This guide is a quick route from an unfamiliar or misbehaving device to the Lens view that can
answer the next question. It assumes Lens is already installed; see [Installing](INSTALLING.md) for
Raspberry Pi OS, Debian, Ubuntu, other Linux systems and macOS.

## Start with the device overview

Run `lens` as your normal user. Confirm the host name and clock first, then read the host pulse,
busiest processes and health summary. Data that is still being collected is labelled as loading.
Unavailable data is labelled separately; Lens does not turn a failed collection into a zero.

Open a selected domain with the arrow keys and `Enter`, or start its command directly:

| Question | Command |
| --- | --- |
| What needs attention on this device? | `lens` |
| Which process is consuming CPU or memory? | `lens-top` |
| Did a service fail or restart repeatedly? | `lens-services` |
| What was logged around a failure? | `lens-logs` |
| Which filesystem is under pressure? | `lens-disk` |
| Does the device have the expected address, route, listener or modem? | `lens-net` |
| Is the device hot, throttled, under-voltage or missing attached hardware? | `lens-hardware` |
| Is time synchronized and which DNS, identity or certificate context applies? | `lens-system` |
| What evidence triggered a warning? | `lens-health` |

Interactive views share the main controls: arrows or `j`/`k` move, `Enter` opens a row, `Esc` goes
back, `/` searches, `r` refreshes, `!` opens a local diagnostic shell, `?` shows contextual help and
`q` quits. The layout adds useful columns and rows as the terminal grows.

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
`log show --last`, such as `30m` or `1h`. Severity is currently inferred from message text, so it is
a useful narrowing tool rather than a complete replacement for native journal priority.

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

A healthy result means that checks with available source data found no problem. It does not mean
every check ran. Review collection warnings when completeness matters.

## Capture a result for support or automation

Use `--once` when you want the same command to print one readable snapshot instead of opening its
interactive screen. Use `--plain` when you want to explicitly select human-readable output, and
`--json` for the shared schema-versioned document. Redirected output defaults to plain text. JSON
preserves entities, findings, relationships and collection warnings so scripts do not have to parse
the terminal layout.

```sh
lens --plain > lens-snapshot.txt
lens-health --json > lens-health.json
```

Review output before sharing it. Process commands, user names, host names, addresses, log messages
and SIM identifiers can contain operationally sensitive information.

The precise output contract is in [JSON schema](JSON_SCHEMA.md). Guardrails for process and service
changes are in [Operational actions](ACTIONS.md).
