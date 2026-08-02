# Dataplicity Lens

**A coherent, modern toolkit for understanding Linux systems.**

Linux already exposes almost everything you need to know. The problem is that the information is
scattered across tools that were never designed together. Lens gives processes, services, logs,
storage and networking one consistent interaction model.

This repository currently ships the first tool, **`lens-top`**: a read-only Linux process explorer
with a fast terminal interface, stable plain text and versioned JSON output. The later Lens tools are
a roadmap, not features claimed to exist today.

Dataplicity Lens is an open-source Linux operations toolkit maintained by WildFoundry Ltd, the team
behind Dataplicity. It is MIT licensed, works locally without an account, and contains no telemetry.

> **Making Linux make sense.**

## Demo

`lens-top --demo` uses deterministic committed data, so screenshots, documentation and tests do not
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

The intended installation path is a signed GitHub Release asset. Choose the archive or native package
for your architecture, verify it against `SHA256SUMS`, then install it.

```sh
# Archive example
curl -LO <release-asset-url>/lens-top-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
curl -LO <release-asset-url>/SHA256SUMS
sha256sum --check SHA256SUMS --ignore-missing
tar -xzf lens-top-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 lens-top-v0.1.0-x86_64-unknown-linux-gnu/lens-top /usr/local/bin/

# Debian package example
sudo apt install ./lens-top_0.1.0_amd64.deb

# RPM package example
sudo rpm -U ./lens-top-0.1.0-1.x86_64.rpm
```

Until the first release is published, build from source with the pinned toolchain:

```sh
cargo build --release --locked -p lens-top
./target/release/lens-top --demo
```

## Use

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

The first release is intentionally read-only. It does not kill, renice or restart processes.

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
apps/lens-top          CLI, configuration, demo source and orchestration
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

`lens-top` needs no root account, daemon, setuid binary, network connection, Dataplicity account or
cloud service. It does not read process environments by default and sends nothing off the host.
Normal permission restrictions are preserved and shown as unavailable data.

Release builds use locked dependencies, immutable action references, licence and advisory checks,
SBOMs, checksums and package smoke tests. See [`SECURITY.md`](SECURITY.md) and
[`docs/RELEASING.md`](docs/RELEASING.md).

## Roadmap

The planned family is:

- `lens-services` — service health, dependencies, startup time, restart loops, processes and logs
- `lens-logs` — journal/file logs, repeated-message folding, rate changes and crash context
- `lens-disk` — filesystems, mounts, growth, inodes and responsible processes/services
- `lens-net` — interfaces, routes, listeners, DNS, process ownership and connectivity diagnosis
- `lens-health` — composed findings from the same shared probes, without duplicate collectors

No placeholder binaries are created for those tools. See [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Contribute

Read [`CONTRIBUTING.md`](CONTRIBUTING.md), run the full local check suite, and preserve the shared
interaction and data contracts. Security reports must follow [`SECURITY.md`](SECURITY.md).

## Licence

MIT. Copyright (c) 2026 WildFoundry Ltd. See [`LICENSE`](LICENSE).
