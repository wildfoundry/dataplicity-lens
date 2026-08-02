# Contributing

Lens is intentionally opinionated. Contributions should preserve the dependency
rules in `docs/ARCHITECTURE.md`, keep structured output aligned with the domain
model, and avoid platform or presentation logic leaking across crate boundaries.

Before opening a pull request, run:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

New tools belong under `apps/`. Shared behaviour belongs in an existing crate or
a narrowly named new crate. Avoid adding dependencies for trivial formatting or
small data transformations.
