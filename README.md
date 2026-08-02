# Dataplicity Lens

Dataplicity Lens is a cohesive suite of fast, humane Linux diagnostics tools.
It starts with `lens-top`, a process and system monitor built around a shared
model, diagnostics engine, output layer, Linux platform adapter, and terminal
UI.

The project is maintained by WildFoundry Ltd and released under the MIT licence.
It does not require a Dataplicity account and contains no telemetry.

## First tool: lens-top

`lens-top` gives operators a calm view of CPU, memory, load, process pressure,
and actionable diagnostics.

```text
cargo run -p lens-top
cargo run -p lens-top -- --once
cargo run -p lens-top -- --format json
cargo run -p lens-top -- --sort memory --filter postgres --limit 25
```

Interactive keys:

| Key | Action |
| --- | --- |
| `q` or `Esc` | Quit |
| `j` / `Down` | Select next process |
| `k` / `Up` | Select previous process |
| `c` | Sort by CPU |
| `m` | Sort by memory |
| `p` | Sort by PID |
| `n` | Sort by name |
| `d` | Toggle ascending/descending |

When stdout is not a terminal, `lens-top` automatically emits a one-shot table
instead of attempting to open the TUI.

## Workspace

```text
apps/
  lens-top/             First executable
crates/
  core/                 Shared filtering, sorting, and source contracts
  model/                Stable domain and output model
  ui/                   Shared terminal interaction and visual language
  diagnostics/          Human-readable system findings
  output/               Table, JSON, and NDJSON renderers
  platform-linux/       Linux collection implementation
```

Future applications plug into the same crates:

- `lens-services`
- `lens-logs`
- `lens-disk`
- `lens-net`
- `lens-health`

See [PHILOSOPHY.md](PHILOSOPHY.md) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design constraints.

## Development

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run -p lens-top
```

The pinned Rust toolchain is defined in `rust-toolchain.toml`.

## Releases

Pushing a tag such as `v0.1.0`, or running the release workflow manually,
produces:

- x86-64 and ARM64 Linux binaries
- compressed binary archives
- Debian packages
- RPM packages
- SPDX JSON SBOMs
- SHA-256 checksum manifests
- a GitHub Release containing all artifacts

## Status

This repository currently establishes the architecture and an operational first
application. The model and output schema should be treated as early-stage until
the first stable release.
