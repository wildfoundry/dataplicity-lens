# AGENTS.md

## Cursor Cloud specific instructions

Dataplicity Lens is a pure Rust workspace (edition 2024, pinned to Rust `1.88.0` via
`rust-toolchain.toml`). It builds a suite of local system-inspection CLIs (`lens`, `lens-top`,
`lens-services`, `lens-containers`, `lens-logs`, `lens-disk`, `lens-net`, `lens-hardware`,
`lens-system`, `lens-health`) plus shared crates under `crates/`. There is no server, database,
or external service — the binaries collect data from the host they run on.

### Build / lint / test / run

Standard commands (also documented in `CONTRIBUTING.md` and `.github/workflows/ci.yml`):

- Build (dev): `cargo build --workspace --locked`
- Build (release, what CI/smoke uses): `cargo build --workspace --all-features --release --locked`
- Format check: `cargo fmt --all -- --check`
- Lint: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- Test: `cargo test --workspace --all-features --locked`
- Smoke suite (CI parity, needs a built target dir): `scripts/smoke-suite.sh target/release`

Run a binary directly after building, e.g. `./target/debug/lens --once --plain` or
`./target/release/lens-top`.

### Non-obvious notes

- `lens-top` (and other specialists) open an interactive full-screen TUI when stdout is a TTY.
  In non-interactive contexts (pipes, CI, agent shells) always pass a one-shot flag
  (`--once`, `--plain`, `--json`, or `--jsonl`) so the command returns instead of blocking on a
  live UI. To exercise the interactive TUI, run it inside a real terminal on the desktop and quit
  with `q`.
- Use `--demo` for deterministic, host-independent sample output (used by the smoke scripts,
  snapshot tests, and doc captures). Real collection is the default; `--demo` is only for
  reproducible fixtures.
- `cargo test` includes `insta` snapshot tests and `proptest` cases; no extra services needed.
- The security checks from CI (`cargo deny check`, `cargo audit`) require installing
  `cargo-deny` and `cargo-audit` (not part of the default toolchain). They are optional for
  local development and are not needed to build/run/test the suite.
- Cross-target release builds (musl / arm) in CI use `cross`; only the host target
  (`x86_64-unknown-linux-gnu`) is needed for normal development here.
