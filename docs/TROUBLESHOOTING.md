# Troubleshooting

Start by recording the exact build and collecting one bounded result:

```sh
lens-top --version
lens --once
lens-health --json | jq '{findings, collection_warnings}'
```

## A count is empty or data says unavailable

An empty successful collection and an unavailable source are different states. Plain output names
unavailable domains; JSON records the reason in `collection_warnings`. Remove filters, run the
specialist directly and inspect those warnings before concluding that the host has no services,
logs, interfaces, listeners or devices.

```sh
lens-logs --json | jq '{count: (.logs | length), warnings: .collection_warnings}'
lens-net --json | jq '{interfaces, routes, sockets, warnings: .collection_warnings}'
```

Containers report their own namespace. A container can legitimately see no systemd services, host
journal, hardware sensors or host certificates unless those facilities are mounted or exposed.

## A command is slow

Open the specialist for the domain you need instead of waiting for the complete cockpit snapshot.
External collectors have individual deadlines and interactive views keep the last frame visible while
new data arrives. For a slow or unreliable remote connection, use `--once`, narrow the result with a
filter and limit, or select JSON for a single transfer.

```sh
lens-logs --since "15 minutes ago" --severity error --limit 100 --once
lens-net --filter 443 --once
```

`lens-top --once` intentionally samples twice. Its default shell sampling window is at most 250 ms;
an explicit `--interval 2s` requests a two-second measurement window.

## Colours are difficult to read

Automatic detection is not reliable in every browser terminal, serial console or multiplexer. Force
the palette for one command or the current shell:

```sh
lens --theme light
export LENS_THEME=dark
```

Use `lens-top --no-color` for uncoloured process output and `lens-top --ascii` when line-drawing
characters are not displayed correctly.

## Terminal content remains after exit

Current builds clear and restore the terminal on quit. Confirm the version first. If a terminal was
disconnected or killed without allowing cleanup, run `reset` in the shell. Include the terminal
product, dimensions and `lens-top --version` output in a bug report if a normal `q` or `Ctrl+C` exit
still leaves content behind.

## Logs are missing

- Linux journal access follows the current user's journal permissions.
- macOS unified-log access follows macOS privacy controls.
- `--log-file` must name a file the current user can read.
- A time range or service/process/severity filter can validly match no records.

Use `--json` and inspect `collection_warnings`; do not run the complete suite as root merely to make a
warning disappear.

## Services or actions fail

Service inspection uses systemd on Linux and launchd on macOS. Guarded service actions are available
for exact systemd units on Linux. Start with `--dry-run`; execution also requires `--yes` and the
invoking account must already be authorised by the operating system.

Process signals reject PID 0, PID 1, Lens itself, missing processes and identities that changed after
confirmation. This is deliberate protection against PID reuse.

## Network or cellular data is missing

Minimal Linux containers use procfs/sysfs fallbacks when common network commands are absent. Socket
ownership can still be hidden by permissions. Cellular inventory uses ModemManager; check that
`mmcli` can see the modem in the same namespace and user context. An empty modem array accompanied by
a ModemManager warning means unavailable collection, not confirmed absence of hardware.

## Getting help

Open a [GitHub help issue](https://github.com/wildfoundry/dataplicity-lens/issues/new?template=help.yml)
or a [bug report](https://github.com/wildfoundry/dataplicity-lens/issues/new?template=bug_report.yml).
Include the build identity, operating system, architecture, command, terminal product and relevant
`collection_warnings`. Remove hostnames, usernames, addresses, log contents, SIM identifiers,
certificate paths and other private system data before posting.

Suspected vulnerabilities belong in GitHub's private security advisory flow described in
[`SECURITY.md`](../SECURITY.md), not in a public issue.
