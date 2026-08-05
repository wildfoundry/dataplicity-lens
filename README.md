# Dataplicity Lens

[![CI](https://github.com/wildfoundry/dataplicity-lens/actions/workflows/ci.yml/badge.svg)](https://github.com/wildfoundry/dataplicity-lens/actions/workflows/ci.yml)
[![Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Processes, services, logs, storage and networking in one terminal toolkit.**

Lens brings the system information we use most often into a consistent set of terminal commands for
Linux and macOS. Start with a host overview, or go directly to processes, services, logs, storage,
networking or health checks.

This repository ships a system overview and six focused commands: **`lens`**, **`lens-top`**,
**`lens-services`**, **`lens-logs`**, **`lens-disk`**, **`lens-net`**, **`lens-system`** and **`lens-health`**. They share
the same host snapshot and process and service identities.

## Why we built Lens

At WildFoundry, we use Linux every day to build and support [Dataplicity](https://www.dataplicity.com/).
Lens began as a tool for our own team: a quicker, more consistent way to inspect processes, services,
logs, storage and networking while working on real systems.

Lens is not a new service or a commercial add-on. Dataplicity remains our core product, and Lens works
independently of it. We are publishing Lens because it is already useful to us, and making it available
may also save time for Dataplicity customers and anyone else looking after Linux or macOS systems. That
is the whole idea.

> **Making Linux make sense.**

## Install

### Raspberry Pi OS, Debian and Ubuntu

Download the package matching `dpkg --print-architecture` from the
[latest release](https://github.com/wildfoundry/dataplicity-lens/releases/latest), then install it
through `apt`:

```sh
dpkg --print-architecture
sudo apt install ./dataplicity-lens_<version>_<architecture>.deb
lens
```

Use `armhf` for 32-bit Raspberry Pi OS, `arm64` for 64-bit Raspberry Pi OS and ARM gateways, and
`amd64` for Intel/AMD Debian or Ubuntu. The release also contains checksummed GNU and musl archives
and RPM packages for other Linux systems.

### macOS with Homebrew

Install Homebrew and Apple's command-line tools if they are not already present, then build, install
and test the full suite from the repository:

```sh
git clone https://github.com/wildfoundry/dataplicity-lens.git
cd dataplicity-lens
scripts/test-homebrew-local.sh --keep
```

The script creates a local source archive, installs the committed formula from source, and checks both
the sample output and native macOS process collection. `--keep` leaves the
formula installed so you can run `lens`, `lens-top --once`, or any specialist command. Omit it for a
clean verification run that uninstalls its temporary tap and package automatically.

Run Lens as your normal account; it does not need `sudo`. Start with the overview, then open the
specialist view you need:

```sh
lens
lens-top --once
lens-services
lens-logs --since "1 hour ago"
lens-disk
lens-net
lens-system --filter ntp
lens-health --json
```

To remove the retained local test installation:

```sh
brew uninstall dataplicity-lens
brew untap local/dataplicity-lens-test
```

See [`docs/INSTALLING.md`](docs/INSTALLING.md) for the package matrix, checksum verification, macOS
prerequisites, removal and source builds.

Until the first release is published, build from source with the pinned toolchain:

```sh
cargo build --release --locked --workspace
./target/release/lens
```

## Use

Start with `lens` for a cockpit, or run a specialist directly. Each specialist supports `--plain`,
`--json`, `--filter` and `--limit`; logs additionally support `--service`, `--severity` and
`--since`. The default limit is 1,000 rows. Pass `--limit 0` when you explicitly want every row in
the selected time range or file.

For practical fault-finding sequences, field interpretation and platform-specific behaviour, use
the [operations guide](docs/OPERATIONS_GUIDE.md). The command pages on the documentation site cover
every interactive view, filter, output mode, action and incomplete-data state in detail.

The interactive cockpit draws the host and process summary first. Services, recent logs, storage and
network details then load once in the background, so a slow platform command does not hold up the
opening screen or normal navigation. Each specialist collects only the system data it displays, and
individual operating-system commands time out instead of holding the whole tool open indefinitely.
Interactive views follow the terminal as it is resized: larger windows show more rows and useful
columns, while smaller windows hide secondary detail before navigation or primary values. Headers
show the local clock, while host summaries retain system uptime. Press `!` to run a one-shot local
diagnostic command in a responsive panel without leaving the live view.

```sh
lens
lens-services --service nginx
lens-logs --since "1 hour ago" --severity error
lens-logs --log-file /var/log/my-app.log --process worker
lens-disk --filter /var
lens-net --filter 443
lens-system --json
lens-health --json
```

When stdout is a terminal, running `lens-top` starts the interactive interface. When stdout is not a
terminal, it automatically emits one plain snapshot, which makes pipes and scheduled collection
unsurprising.

```sh
lens-top
lens-top --plain
lens-top --json
lens-top --jsonl
lens-top --once
lens-top --interval 2s
lens-top --sort cpu
lens-top --sort memory
lens-top --group tree
lens-top --filter-user postgres
lens-top --filter-name nginx
lens-top --filter-service sshd
lens-top --min-cpu 5
lens-top --min-memory 1
lens-top --limit 20
lens-top --theme light
lens-top --no-color
```

### Interactive keys

| Key | Action |
| --- | --- |
| `Up` / `Down` or `j` / `k` | Move |
| `Enter` | Inspect selected process |
| `a` | Review and run a signal against the selected process |
| `Esc` | Go back or close an overlay |
| `/` | Search name, command, PID, user, service and cgroup |
| `f` | Filter (`user:`, `state:`, `cpu:>`, `mem:>`, `name:`, `service:`) |
| `s` | Choose sort key |
| `g` | Cycle no grouping, process tree, user and service/cgroup |
| `Tab` / `Shift+Tab` | Next or previous item |
| `Space` | Pause or resume sampling |
| `r` | Refresh immediately |
| `!` | Open the responsive diagnostic shell |
| `?` | Help |
| `q` or `Ctrl+C` | Quit |

Process signals are available from the interactive `a` menu and as explicit one-shot commands. Lens resolves the process before acting,
checks its PID/start-time identity again immediately before execution, requires `--yes`, and reports
the observed result. Use `--dry-run` first:

```sh
lens-top --signal term --pid 4242 --dry-run
lens-top --signal term --pid 4242 --yes
```

On systemd Linux, press `a` on a selected service, or use the CLI to start, stop, restart, enable or disable it with the same
plan/confirm/result pattern. Lens does not run itself as root; system policy decides whether the
invoking account may perform the action.

```sh
lens-services --action restart --target nginx.service --dry-run
lens-services --action restart --target nginx.service --yes
```

## Plain and structured output

```sh
lens-top --plain --sort memory --limit 10
lens-top --json --filter-user postgres
lens-top --jsonl --min-cpu 5
```

JSON documents always include `schema_version`, an RFC 3339 UTC `generated_at` timestamp, host data,
processes, findings, relationships and optional build metadata. Byte fields are bytes; rates are bytes
per second; percentages use a `0..100` host scale and may exceed `100` for a multithreaded process
using more than one logical CPU. Missing permission-limited fields are `null` or listed under
`unavailable_fields`; they do not abort collection.

The schema and stability policy are documented in [`docs/JSON_SCHEMA.md`](docs/JSON_SCHEMA.md).

## Configuration

No configuration is required. Lens loads, in increasing precedence:

1. Built-in defaults
2. `$XDG_CONFIG_HOME/dataplicity-lens/config.toml` or `~/.config/dataplicity-lens/config.toml`
3. `LENS_TOP_*` environment variables
4. Command-line arguments

```sh
lens-top --print-default-config
```

Lens normally infers a light or dark background from `COLORFGBG` and chooses a contrasting palette.
Some serial and browser terminals do not expose that metadata. Use `--theme light` or `--theme dark`
when needed; `LENS_THEME=light` (or `dark`) applies the same override to the complete suite. Lens
continues to use the terminal's own background rather than painting over it.

## Architecture

The workspace keeps applications thin and gives shared crates real responsibilities:

```text
apps/lens*             Thin cockpit, process explorer and specialist entry points
crates/system          Shared service, log, storage and network composition
crates/model           Shared entities, snapshots, findings and relationships
crates/core            Shared filtering, sorting, grouping and search grammar
crates/platform-linux  Linux collection and kernel parsers
crates/platform-macos  macOS collection and command parsers
crates/history         Bounded deltas, trends and process appearance/disappearance
crates/diagnostics     System checks and findings
crates/output          Plain, JSON and JSON Lines contracts
crates/ui              Shared terminal shell, tables, overlays and detail views
```

Collection, diagnostics and rendering do not duplicate one another. A process identity includes both
PID and start ticks so PID reuse cannot silently join unrelated histories. See
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Performance

Lens bounds every history buffer, caches UID lookups, avoids environment-variable reads, does not scan
process file descriptors beyond counting directory entries, and tolerates processes disappearing
between reads. Criterion benchmarks cover filtering and sorting 10,000 processes. The design targets
sub-100 ms startup and smooth one-second refreshes; actual measurements are tracked in
[`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) rather than claimed without evidence.

## Security and privacy

Lens needs no root account, daemon, setuid binary, network connection, Dataplicity account or
cloud service. It does not read process environments by default and sends nothing off the host.
Normal permission restrictions are preserved and shown as unavailable data.

Release builds use locked dependencies, immutable action references, licence and advisory checks,
SBOMs, checksums and package smoke tests. See [`SECURITY.md`](SECURITY.md) and
[`docs/RELEASING.md`](docs/RELEASING.md).

## Shipped suite

- `lens-services` — navigable service state, restart loops and related processes
- `lens-logs` — a navigable recent-message view, repeated-message folding and service/severity/time filters
- `lens-disk` — navigable block devices, filesystems, mounts, inodes and deleted-open files
- `lens-net` — navigable interfaces, routes and listener ownership
- `lens-system` — clock/NTP, resolver, local identities and visible public certificate files
- `lens-health` — selectable findings with the evidence and suggested checks behind each warning

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for shipped and future scope, and
[`docs/V1_CONTRACT.md`](docs/V1_CONTRACT.md) for the compatibility, action and release gates that
must be met before the project is labelled 1.0.

## Contribute

Read [`CONTRIBUTING.md`](CONTRIBUTING.md), run the full local check suite, and preserve the shared
interaction and data contracts. Security reports must follow [`SECURITY.md`](SECURITY.md).

## Licence

Licensed under the Apache License, Version 2.0. Copyright 2026 WildFoundry Ltd. See [`LICENSE`](LICENSE)
and [`NOTICE`](NOTICE).
