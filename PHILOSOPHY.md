# Dataplicity Lens philosophy

Linux operators live inside a small set of tools that are indispensable, ugly,
inconsistent, and far harder to learn than their jobs should require. Lens is
an attempt to replace that accidental complexity without replacing the Unix
ideas that made those tools useful.

## The rules

### Show the system, not the implementation

A diagnostic tool should answer the operator's question before exposing the
kernel detail behind it. Detail remains available, but it is not the opening
move.

### Beautiful is operational

Legibility is not decoration. Alignment, hierarchy, colour restraint, sensible
units, stable ordering, and explicit warnings reduce mistakes under pressure.
A terminal UI should be calm when the system is not.

### One model, many surfaces

Interactive terminal views, tables, JSON, future web rendering, and automated
diagnostics must all consume the same domain model. Presentation code must not
reimplement collection logic.

### Useful alone, stronger together

Every Lens tool must solve one familiar Linux task by itself. Shared crates,
keyboard conventions, output schemas, diagnostics, and release machinery make
the suite feel like one instrument rather than a bag of commands.

### Machine-readable is a first-class interface

Anything visible to a human should also be available as stable structured
output. Scripts should never need to scrape the terminal UI.

### Read-only by default

Observation is safe. Mutation is deliberate, explicit, and auditable. The
initial Lens suite diagnoses; it does not silently repair.

### Fast enough to become muscle memory

Startup should feel immediate. Refreshes should not stall the interface.
Dependencies must earn their place, and collection work should remain bounded.

### No account, daemon, or telemetry required

Lens works locally and offline. It does not require Dataplicity, transmit usage,
or turn a basic Linux tool into a sales funnel. The project can earn attention
for Dataplicity by being excellent and visibly maintained by WildFoundry.

### Compatibility beats novelty

The suite should run on ordinary supported Linux distributions, package cleanly,
behave predictably over SSH, and degrade gracefully in restricted containers.
New ideas belong in the interaction, not in gratuitous platform requirements.

### Stable names and composable output

Commands, fields, exit codes, and meanings are interfaces. Changes should be
additive where practical and versioned when they cannot be.

## What Lens should become

A cohesive replacement layer for the routine tools operators reach for every
day: processes, services, logs, disks, networks, and system health. Each tool
should be immediately understandable on its own and unmistakably part of the
same family.
