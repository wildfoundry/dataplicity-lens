# Roadmap

## v0.1 — lens-top

- Process and host collection
- Session-local CPU, memory and I/O trends
- Search, filters, sorting, grouping and process details
- Plain, JSON and JSON Lines output
- Deterministic findings and demo mode
- GNU/musl binaries, Debian/RPM packages, completions, man page, SBOMs and checksums

## lens-services

Service health, dependency relationships, startup duration, restart loops, processes and related logs.

## lens-logs

Journal and file logs, repeated-message folding, rate changes, severity filters, crash context and
service/process relationships.

## lens-disk

Filesystems, mounts, block devices, directory responsibility, growth, inodes and deleted-but-open
files, connected to the processes and services responsible.

## lens-net

Interfaces, addresses, routes, listeners, connections, DNS, process ownership and connectivity
diagnosis.

## lens-health

Composed findings from all mature shared probes. It must not create a second set of collectors or a
second diagnostics language. The findings model is deliberately suitable for later reuse by
Dataplicity Pulse.

The roadmap does not imply that these later binaries exist yet.
