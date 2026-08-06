# Contributing

Dataplicity Lens is intentionally opinionated. Contributions should preserve the shared model,
interaction grammar, deterministic diagnostics and output contracts described in `PHILOSOPHY.md` and
`docs/ARCHITECTURE.md`.

For usage questions, bugs and feature requests, use [GitHub
Issues](https://github.com/wildfoundry/dataplicity-lens/issues). For a substantial change, open an
issue first so the behaviour and platform impact can be agreed before implementation.

## Development setup

Install the Rust toolchain declared in `rust-toolchain.toml`, clone the repository, and build the
locked workspace:

```sh
git clone https://github.com/wildfoundry/dataplicity-lens.git
cd dataplicity-lens
cargo build --workspace --locked
```

Linux and macOS collectors use native operating-system interfaces, so run platform-specific changes
on the affected platform where possible. Deterministic fixtures under `tests/fixtures/` cover parser
work that can be tested on either platform.

## Before opening a pull request

Run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --release --locked
cargo deny check
cargo audit
```

Keep a pull request focused on one problem. Include tests for changed behaviour, update user
documentation when commands or output change, and add a short entry under `Unreleased` in
`CHANGELOG.md` for a user-visible change. Do not include real host data, logs, addresses or credentials
in fixtures, screenshots or issue reports.

New applications belong under `apps/`. Shared behaviour belongs in an existing crate unless a new
crate has a clear, durable responsibility. Avoid adding dependencies for trivial transformations.
Production code must not use `unsafe`, and `unwrap()` or `expect()` require a comment explaining why
the invariant is sound.

Please include tests for parser edge cases, output changes and interaction behaviour. Changes to the
JSON schema must remain additive within schema version `2` or deliberately introduce a new version.

Deterministic sample data is a contributor tool, not the normal user path. Use `--demo` for UI work,
snapshot tests and documentation captures; see `docs/DEMO.md`. Installation and usage documentation
should lead with real collection from the user's machine.

By submitting a contribution, you agree that it may be distributed under the repository's Apache
License 2.0.
