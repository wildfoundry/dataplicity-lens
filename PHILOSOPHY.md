# Dataplicity Lens philosophy

## Making the command line make sense

Linux already exposes the facts. Lens makes the system make sense.

Linux tools are individually powerful, but they were rarely designed together. Routine inspection
therefore demands that an operator remember unrelated commands, output formats, filters and mental
models. Lens exists to make that work coherent without hiding the Linux system underneath it.

**A coherent command line for modern Linux.**

## Principles

### Consistency

A user who learns one Lens tool should already know how to use the others. Keys, search grammar,
filters, sorting, grouping, output schemas, severity language and visual hierarchy are shared product
interfaces rather than local implementation choices.

### Relationships, not isolated lists

A process belongs to a user and a parent, often a service, cgroup or container, opens files and
sockets, and writes logs. Lens models those relationships explicitly so future tools can preserve
context as the user moves between domains.

### Progressive disclosure

Show the useful summary first. Keep the raw evidence available. Experts should be able to drill down
without forcing every user to begin at kernel-detail level.

### Human-first and machine-friendly

The interactive terminal experience must be excellent, but every important result is also available
as stable plain text, JSON or JSON Lines. Scripts must never need to scrape a TUI.

### Beauty is clarity

Colour, spacing, alignment and graphics communicate meaning. They are not decoration. A calm,
legible display reduces operational mistakes when a system is under pressure.

### Fast and native

Lens tools start quickly, remain responsive over SSH and serial terminals, bound their history and
memory use, and avoid dependencies that do not earn their cost.

### Local and private

No account, cloud service, daemon, telemetry or network connection is required. Lens is safe to run
on production hosts and does not turn system inspection into a lead-capture flow.

### Open

Lens is licensed under Apache License 2.0, forkable and useful independently of Dataplicity.
Dataplicity Lens is maintained by WildFoundry Ltd, the team behind Dataplicity; that provenance
should build trust without interrupting use.

### Inspect first, act deliberately

Lens leads with evidence. When an operator chooses to act, the target and effect must be explicit,
the current identity must be checked again, and the observed result must be reported. Lens does not
hide privilege prompts, retry state changes, or turn findings into unattended remediation.

The built-in diagnostic shell keeps manual investigation beside live system data. It is a visible,
user-opened workspace that uses the invoking account; it is not a privileged or remote shell.
