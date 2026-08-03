# Contributing

Dataplicity Lens is intentionally opinionated. Contributions should preserve the shared model,
interaction grammar, deterministic diagnostics and output contracts described in `PHILOSOPHY.md` and
`docs/ARCHITECTURE.md`.

Before opening a pull request, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --release --locked
cargo deny check
cargo audit
```

New applications belong under `apps/`. Shared behaviour belongs in an existing crate unless a new
crate has a clear, durable responsibility. Avoid adding dependencies for trivial transformations.
Production code must not use `unsafe`, and `unwrap()` or `expect()` require a comment explaining why
the invariant is sound.

Please include tests for parser edge cases, output changes and interaction behaviour. Changes to the
JSON schema must remain additive within schema version `1` or deliberately introduce a new version.
