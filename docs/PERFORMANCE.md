# Performance targets and measurement

The v0.2 design targets:

- draw the interactive host/process overview without waiting for supplemental system commands
- keep navigation responsive while services, logs, storage and networking load in the background
- show an interactive specialist immediately, then collect only that specialist's domain in the
  background
- apply an eight-second deadline to each external platform command so a stuck utility cannot hold a
  view open indefinitely (`LENS_COLLECTOR_TIMEOUT_MS` can shorten this for testing)
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
not part of v0.3.0.
