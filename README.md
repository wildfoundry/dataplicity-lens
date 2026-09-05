# Dataplicity Lens

[![CI](https://img.shields.io/github/actions/workflow/status/wildfoundry/dataplicity-lens/ci.yml?branch=main&label=CI)](https://github.com/wildfoundry/dataplicity-lens/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/wildfoundry/dataplicity-lens?display_name=tag)](https://github.com/wildfoundry/dataplicity-lens/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Raspberry%20Pi-0B3D32)](https://lens.dataplicity.com/compatibility.html)
[![Docs](https://img.shields.io/badge/docs-lens.dataplicity.com-1F6F5B)](https://lens.dataplicity.com/)

**Open-source terminal toolkit for Linux and macOS system inspection.**

Dataplicity Lens is a local CLI suite for understanding a host in focus: processes and resource use,
systemd / launchd services, journal and file logs, disk and mounts, network interfaces and listeners,
hardware inventory, system context, and health findings — in one consistent terminal experience.

Built for Raspberry Pi OS, Debian, Ubuntu, other Linux systems, and macOS. No daemon, no cloud account,
no telemetry. Install a package, run `lens`, and work with the machine in front of you.

> **Making Linux make sense.**

**Docs:** [lens.dataplicity.com](https://lens.dataplicity.com/) · **Install:** [install guide](https://lens.dataplicity.com/install.html) · **Releases:** [GitHub Releases](https://github.com/wildfoundry/dataplicity-lens/releases)

---

## Why Lens

Linux already exposes the facts. Operators still juggle `top` / `htop`, `systemctl`, `journalctl`,
`df`, `ip`, `lsusb`, and a dozen other formats. Lens brings that system information into one toolkit
with shared process and service identities, so you can move from a noisy process to its service,
recent logs, and listeners without starting over.

Lens is maintained by [WildFoundry](https://www.dataplicity.com/) (the team behind Dataplicity). It is
not a Dataplicity commercial add-on and works independently of Dataplicity. We publish it because we
use it every day for real support work on Linux systems — including fleets of Raspberry Pi and gateway
devices.

## Features

| Need | Command | What you get |
| --- | --- | --- |
| Host overview | `lens` | Cockpit summary of the machine |
| Process monitor | `lens-top` | Live TUI process explorer (CPU, memory, I/O, state, signals) |
| Services | `lens-services` | systemd / launchd state, restart loops, related processes |
| Containers | `lens-containers` | Docker/Podman/nerdctl inventory and safe start/stop/restart |
| Logs | `lens-logs` | Recent journal, unified log, and local file messages |
| Storage | `lens-disk` | Block devices, filesystems, mounts, inodes, deleted-open files |
| Networking | `lens-net` | Interfaces, routes, listeners, live receive/transmit charts |
| Hardware | `lens-hardware` | Identity, temperatures, firmware, USB / serial inventory |
| System context | `lens-system` | Clock / NTP, resolver, login identities, local certificates |
| Health checks | `lens-health` | Findings with evidence and suggested follow-ups |

Every specialist supports interactive terminal use plus one-shot **plain text**, **JSON**, and **JSON
Lines** for scripts and scheduled checks. Flags that do not apply are rejected instead of silently
ignored.

## Install

### Raspberry Pi OS, Debian, and Ubuntu

Download the `.deb` matching `dpkg --print-architecture` from
[GitHub Releases](https://github.com/wildfoundry/dataplicity-lens/releases), then install with `apt`:

```sh
dpkg --print-architecture
sudo apt install ./dataplicity-lens_<version>_<architecture>.deb
lens
```

| Architecture | Typical systems |
| --- | --- |
| `armhf` | 32-bit Raspberry Pi OS |
| `arm64` | 64-bit Raspberry Pi OS and ARM gateways |
| `amd64` | Intel / AMD Debian and Ubuntu |

Debian and RPM packages ship statically linked binaries, so they are not tied to the build machine's
glibc. Releases also include checksummed GNU and musl archives for other Linux systems.

### macOS with Homebrew

```sh
brew tap wildfoundry/tap
brew trust --formula wildfoundry/tap/dataplicity-lens
brew install wildfoundry/tap/dataplicity-lens
lens --version
```

The formula installs a native Apple Silicon or Intel archive from GitHub Releases (no local Rust
compile). Homebrew verifies the published SHA-256 digest before installation.

### Quick start

Run as your normal user — Lens does not need `sudo` to inspect the host:

```sh
lens
lens-top --once
lens-services
lens-containers
lens-logs --since "1 hour ago"
lens-disk
lens-net
lens-hardware
lens-system --filter ntp
lens-health --json
```

Full package matrix, checksum verification, upgrades, and removal:
[`docs/INSTALLING.md`](docs/INSTALLING.md) · [online install guide](https://lens.dataplicity.com/install.html)

Build from source (contributors):

```sh
cargo build --release --locked --workspace
./target/release/lens
```

## Use

Start with `lens` for the cockpit, or open a specialist directly. Common flags across the suite:

- `--once` / `--plain` — one human-readable snapshot (redirected stdout is plain automatically)
- `--json` / `--jsonl` — structured output for automation
- `--fields LIST` / `--quiet` — project JSON or assert with exit codes only
- `--filter` / `--match` / `--limit` — narrow results (`--limit 0` returns every available row)
- Opt-in asserts: `--fail-if-empty`, `--fail-if-any`, `--expect-count*`, `--fail-on`, `--fail-on-collection-warnings` (exit `3` on miss)
- Logs also support `--service`, `--process`, `--severity`, `--since`, and repeatable `--log-file`

```sh
lens --once
lens-services --name nginx.service --active active --fail-if-empty --quiet
lens-containers --runtime docker --state running --fail-if-empty --quiet
lens-logs --since "1 hour ago" --severity error
lens-disk --mount /var --min-used-percent 80 --fail-if-any --quiet
lens-net --listening --port 22 --expect-count-min 1 --json --fields sockets
lens-hardware --class usb --serial ABC --match exact
lens-health --fail-on critical --fail-on-collection-warnings --quiet
```

### Process explorer (`lens-top`)

When stdout is a terminal, `lens-top` opens the interactive process monitor. Use `--once`, `--plain`,
`--json`, or `--jsonl` for one-shot output.

```sh
lens-top
lens-top --plain --sort memory --limit 20
lens-top --json --filter-user postgres
lens-top --filter-name nginx --min-cpu 5
lens-top --group tree --theme light
```

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k` | Move |
| `Enter` | Inspect selected process |
| `a` | Review and run a signal |
| `/` | Search name, command, PID, user, service, cgroup |
| `f` | Filter (`user:`, `state:`, `cpu:>`, `mem:>`, `name:`, `service:`) |
| `s` / `g` | Sort / group |
| `!` | Responsive diagnostic shell |
| `?` / `q` | Help / quit |

Process signals, systemd service actions and container actions use an explicit plan → confirm →
result flow (`--dry-run`, then `--yes`). Lens does not run itself as root; system policy decides
whether the account may act.

```sh
lens-top --signal term --pid 4242 --dry-run
lens-top --signal term --exact-name nginx --expect-name nginx --dry-run
lens-top --signal term --pid 4242 --yes
lens-services --action restart --target nginx.service --dry-run
lens-services --action restart --name nginx.service --match exact --expect-active active --yes
lens-containers --action restart --name edge-mqtt --match exact --dry-run
lens-containers --action start --name metrics-agent --match exact --expect-status running --yes
```

Practical fault-finding sequences: [`docs/OPERATIONS_GUIDE.md`](docs/OPERATIONS_GUIDE.md) ·
[suite overview](https://lens.dataplicity.com/lens-suite.html)

## Plain text and JSON

```sh
lens-top --plain --sort memory --limit 10
lens-top --json --filter-user postgres
lens-top --jsonl --min-cpu 5
```

JSON documents always include `schema_version`, an RFC 3339 UTC `generated_at` timestamp, host data,
processes, findings, relationships, and optional build metadata. Missing permission-limited fields are
`null` or listed under `unavailable_fields` — they do not abort collection.

- Schema and stability: [`docs/JSON_SCHEMA.md`](docs/JSON_SCHEMA.md)
- CLI, streaming, exit status, and actions: [`docs/CLI_CONTRACT.md`](docs/CLI_CONTRACT.md)

## Configuration

No configuration is required. Precedence, lowest to highest:

1. Built-in defaults
2. `$XDG_CONFIG_HOME/dataplicity-lens/config.toml` or `~/.config/dataplicity-lens/config.toml`
3. `LENS_TOP_*` environment variables
4. Command-line arguments

```sh
lens-top --print-default-config
```

Theme: Lens infers light/dark from `COLORFGBG` when available. Override with `--theme light|dark` or
`LENS_THEME=light|dark` for the full suite.

## Design

```text
apps/lens*             Thin cockpit and specialist entry points
crates/system          Shared service, log, storage, and network composition
crates/model           Entities, snapshots, findings, relationships
crates/core            Filtering, sorting, grouping, search grammar
crates/platform-linux  Linux collection and kernel parsers
crates/platform-macos  macOS collection and command parsers
crates/history         Bounded deltas and process appearance / disappearance
crates/diagnostics     System checks and findings
crates/output          Plain, JSON, and JSON Lines contracts
crates/ui              Shared terminal shell, tables, overlays
```

- **Local and private** — no root requirement by default, no network connection, no Dataplicity
  account, nothing sent off the host
- **Fast** — bounded history, timed-out OS commands, responsive interactive views over SSH and serial
- **Safe actions** — inspect first; re-check process identity before signals or service changes

More: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) · [`PHILOSOPHY.md`](PHILOSOPHY.md) ·
[`SECURITY.md`](SECURITY.md)

## Documentation

| Topic | Link |
| --- | --- |
| Documentation site | [lens.dataplicity.com](https://lens.dataplicity.com/) |
| Install, upgrade, remove | [docs/INSTALLING.md](docs/INSTALLING.md) |
| Operations guide | [docs/OPERATIONS_GUIDE.md](docs/OPERATIONS_GUIDE.md) |
| Platform compatibility | [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) |
| CLI and automation contract | [docs/CLI_CONTRACT.md](docs/CLI_CONTRACT.md) |
| JSON schema | [docs/JSON_SCHEMA.md](docs/JSON_SCHEMA.md) |
| Troubleshooting | [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |
| Support policy | [docs/SUPPORT_POLICY.md](docs/SUPPORT_POLICY.md) |
| Threat model | [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) |
| Releasing | [docs/RELEASING.md](docs/RELEASING.md) |

## Contribute

Read [`CONTRIBUTING.md`](CONTRIBUTING.md), run the full local check suite, and preserve the shared
interaction and data contracts. Usage support: [GitHub Issues](SUPPORT.md). Security reports:
[`SECURITY.md`](SECURITY.md).

## Licence

Licensed under the Apache License, Version 2.0. Copyright 2026 WildFoundry Ltd.
See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
