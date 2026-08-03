# Dataplicity Lens

**A coherent, modern toolkit for understanding Linux and macOS systems.**

Modern operating systems already expose almost everything you need to know. The problem is that the information is
scattered across tools that were never designed together. Lens gives processes, services, logs,
storage and networking one consistent interaction model.

This repository ships a read-only cockpit and six focused commands: **`lens`**, **`lens-top`**,
**`lens-services`**, **`lens-logs`**, **`lens-disk`**, **`lens-net`** and **`lens-health`**. They share
one canonical snapshot, relationship and finding model rather than collecting competing versions of
the same host.

Dataplicity Lens is an open-source system operations toolkit maintained by WildFoundry Ltd, the team
behind Dataplicity. It is licensed under Apache License 2.0, works locally without an account, and
contains no telemetry.

> **Making Linux make sense.**

## Demo

Every binary supports deterministic `--demo` data, so screenshots, documentation and tests do not
depend on the machine running them.

```text
 Dataplicity Lens · production-gateway-04              1.0s · running
 CPU  18%  ▁▂▂▃▄▃▂▂      Memory  44%  ▂▂▃▃▄▄▄▅      Load  0.41 0.38 0.31
 Processes  5        Running  1       Zombies  1       Findings  1
 PID     PROCESS          USER       CPU     MEM      READ       WRITE      STATE
 8421    image-worker     service    38.2%   12.4%    4.2 MB/s   380 KB/s   Running
 1027    postgres         postgres    7.8%   18.1%    820 KB/s   1.1 MB/s   Sleeping
 2214    mqtt-bridge      mqtt        3.1%    1.8%    110 KB/s   90 KB/s    Sleeping
 Attention
 zombie process detected: PID 9462, parent image-worker
 / Search   f Filter   s Sort   g Group   Enter Inspect   ? Help   q Quit
```

Generate a repeatable text capture with:

```sh
scripts/capture-demo.sh
```

## Install

### macOS with Homebrew

From a source checkout, build and install the full suite through Homebrew:

```sh
scripts/test-homebrew-local.sh --keep
```

The script creates a local source archive, installs the committed formula from source, and runs both
the deterministic suite checks and a native macOS process collection check. `--keep` leaves the
formula installed so you can run `lens`, `lens-top --once`, or any specialist command. Omit it for a
clean verification run that uninstalls its temporary tap and package automatically.

The intended installation path is a signed GitHub Release asset. Choose the archive or native package
for your architecture, verify it against `SHA256SUMS`, then install it.

```sh
# Archive example
curl -LO <release-asset-url>/dataplicity-lens-v0.2.0-x86_64-unknown-linux-gnu.tar.gz
curl -LO <release-asset-url>/SHA256SUMS
sha256sum --check SHA256SUMS --ignore-missing
tar -xzf dataplicity-lens-v0.2.0-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 dataplicity-lens-v0.2.0-x86_64-unknown-linux-gnu/bin/* /usr/local/bin/

# Debian package example
sudo apt install ./dataplicity-lens_0.2.0_amd64.deb

# RPM package example
sudo rpm -U ./dataplicity-lens-0.2.0-1.x86_64.rpm
```

Until the first release is published, build from source with the pinned toolchain:

```sh
cargo build --release --locked --workspace
./target/release/lens --demo --plain
```

## Use

Start with `lens` for a cockpit, or run a specialist directly. Each specialist supports `--plain`,
`--json`, `--demo`, `--filter` and `--limit`; logs additionally support `--service`, `--severity` and
`--since`.

```sh
lens
lens-services --service nginx
lens-logs --since "1 hour ago" --severity error
lens-logs --log-file /var/log/my-app.log --process worker
lens-disk --filter /var
lens-net --filter 443
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
lens-top --no-color
lens-top --demo
```

### Interactive keys

| Key | Action |
| --- | --- |
| `Up` / `Down` or `j` / `k` | Move |
| `Enter` | Inspect selected process |
| `Esc` | Go back or close an overlay |
| `/` | Search name, command, PID, user, service and cgroup |
| `f` | Filter (`user:`, `state:`, `cpu:>`, `mem:>`, `name:`, `service:`) |
| `s` | Choose sort key |
| `g` | Cycle no grouping, process tree, user and service/cgroup |
| `Tab` / `Shift+Tab` | Next or previous item |
| `Space` | Pause or resume sampling |
| `r` | Refresh immediately |
| `?` | Help |
| `q` or `Ctrl+C` | Quit |

The suite is intentionally read-only. It does not kill, renice, restart, delete or reconfigure.

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

## Architecture

The workspace keeps applications thin and gives shared crates real responsibilities:

```text
apps/lens*             Thin cockpit, process explorer and specialist entry points
crates/system          Shared service, log, storage and network composition
crates/model           Canonical entities, snapshots, findings and relationships
crates/core            Shared filtering, sorting, grouping and search grammar
crates/platform-linux  Read-only Linux collection and robust kernel parsers
crates/history         Bounded deltas, trends and process appearance/disappearance
crates/diagnostics     Deterministic evidence-based findings
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

- `lens-services` — service state, restart loops and related processes
- `lens-logs` — recent journal logs, repeated-message folding and service/severity/time filters
- `lens-disk` — block devices, filesystems, mounts, inodes and deleted-open files
- `lens-net` — interfaces, routes and listener ownership
- `lens-health` — composed findings from the same shared probes, without duplicate collectors

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for shipped and future scope.

## Contribute

Read [`CONTRIBUTING.md`](CONTRIBUTING.md), run the full local check suite, and preserve the shared
interaction and data contracts. Security reports must follow [`SECURITY.md`](SECURITY.md).

## Licence

Licensed under the Apache License, Version 2.0. Copyright 2026 WildFoundry Ltd. See [`LICENSE`](LICENSE)
and [`NOTICE`](NOTICE).
