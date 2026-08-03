# Roadmap

## v0.1 — lens-top (shipped foundation)

- Process and host collection
- Session-local CPU, memory and I/O trends
- Search, filters, sorting, grouping and process details
- Plain, JSON and JSON Lines output
- Deterministic findings and demo mode
- GNU/musl binaries, Debian/RPM packages, completions, man page, SBOMs and checksums

## v0.2 — Lens suite

- `lens` cockpit with specialist navigation and stable non-interactive output
- `lens-services` with service state, restart counts and process context
- `lens-logs` with journal/file inputs, folding and service/process/severity/time filtering
- `lens-disk` with block devices, capacity, inodes and deleted-open files
- `lens-net` with interfaces, routes, listeners and owner relationships
- `lens-health` with deterministic cross-domain findings
- Shared schema version 2, fixtures, Pages documentation and complete-suite packages

## Later evidence-led work

Potential additions include service dependency/startup timing, log-rate baselines, directory growth,
DNS and connection probes. They should reuse the canonical model and only ship when their evidence
and operating cost are understood.
