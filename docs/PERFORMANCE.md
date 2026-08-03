# Performance targets and measurement

The v0.2 design targets:

- startup below 100 ms on an ordinary modern Linux or macOS host
- smooth one-second refresh with at least 10,000 processes
- low idle overhead
- no unbounded history or cache growth

These are targets until measured on release hardware. Do not convert them into marketing claims
without recorded results.

Run the current Criterion benchmark with:

```sh
cargo bench -p lens-top --bench process_pipeline --locked
```

Release validation should additionally record startup time, peak RSS and collection time on small and
large `/proc` fixtures. UID lookups are cached, histories are bounded, and expensive persistence is
not part of v0.2.0.
