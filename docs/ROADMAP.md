# Roadmap

## v0.1 — lens-top

- Process and host collection
- Session-local CPU, memory and I/O trends
- Search, filters, sorting, grouping and process details
- Plain, JSON and JSON Lines output
- System warnings and sample data
- GNU/musl binaries, Debian/RPM packages, completions, man page, SBOMs and checksums

## v0.2 — Lens suite

- `lens` cockpit with specialist navigation and stable non-interactive output
- `lens-services` with service state, restart counts and process context
- `lens-logs` with journal/file inputs, folding and service/process/severity/time filtering
- `lens-disk` with block devices, capacity, inodes and deleted-open files
- `lens-net` with interfaces, routes, listeners and owner relationships
- `lens-health` with warnings from every specialist check
- Shared schema version 2, fixtures and complete-suite packages

## Under consideration

Possible additions include service dependency and startup timing, log-rate baselines, directory growth,
DNS checks and connection probes. We will prioritize work that proves useful in day-to-day support and
operations.
