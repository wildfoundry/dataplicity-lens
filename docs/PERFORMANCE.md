# Performance and measurement

Lens keeps the interface responsive by:

- draw the interactive host/process overview without waiting for supplemental system commands
- keep navigation responsive while services, logs, storage, networking and hardware load in the background
- overwrite interactive frames before clearing trailing cells, keeping the previous view visible on
  slower browser and remote terminals
- show an interactive specialist immediately, then collect only that specialist's domain in the
  background
- sample interface byte counters on a lightweight worker for live network charts
- apply an eight-second deadline to each external platform command so a stuck utility cannot hold a
  view open indefinitely (`LENS_COLLECTOR_TIMEOUT_MS` can shorten this for testing)
- refresh the process view smoothly with large process tables
- no unbounded history or cache growth

Run the current Criterion benchmark with:

```sh
cargo bench -p lens-top --bench process_pipeline --locked
```

Release validation records startup time, peak RSS and collection time on small and large `/proc`
fixtures. UID lookups are cached and histories are bounded.
