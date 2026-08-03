# Architecture

## Dependency direction

```text
lens-model
  ^     ^       ^
  |     |       |
lens-core   lens-history   lens-platform-linux
  ^             ^                 ^
  |             |                 |
lens-ui     lens-diagnostics      |
      \          |               /
       \      lens-output        /
        \         ^             /
             apps/lens-top
```

`lens-model` owns canonical user-facing types. Platform collection produces the model; history mutates
sample-derived rates; diagnostics consumes snapshots and bounded history; output and UI render the
same model. The application composes those parts and owns CLI/configuration policy.

No presentation crate reads `/proc`, and no collector emits terminal strings.

## Identity and PID reuse

A process is identified by `(pid, start_time_ticks)`, not PID alone. Deltas and histories are joined
only when both values match. A process disappearing during collection is skipped; it is not an error.
Broken parent references and tree cycles are bounded and rendered safely.

## Collection policy

The Linux collector reads only local, read-only interfaces:

- `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`
- `/proc/[pid]/stat`, `status`, `cmdline`, `io`, `cgroup`, `fd`, `exe`
- `/proc/sys/kernel/hostname`, `/proc/sys/kernel/osrelease`
- `/etc/os-release`, `/etc/passwd`

Required host-level files produce a clear top-level error. Optional process fields are represented as
unavailable and collection continues. Environment variables are deliberately not read.

## History and rates

History is session-local and bounded. CPU usage is calculated from process tick deltas relative to
host tick deltas. I/O rates use counter deltas over measured elapsed time. Counter decreases are
saturating, protecting against process replacement, resets and malformed data.

No persistence is included in v0.1.0.

## Findings

Findings are deterministic structures with identifiers, severity, evidence, related entities and
suggested actions. They use cautious wording when evidence is suggestive. The diagnostics engine does
not make network calls or use an AI model.

## Terminal safety

The terminal session is represented by a guard. Raw mode and the alternate screen are restored in its
`Drop` implementation, including normal errors and unwinding panics. Ctrl+C is handled as an ordinary
key event while raw mode is active. Non-terminal stdout never opens the TUI.

## Future tools

Future applications should reuse model entities, relationship types, query grammar, diagnostics,
output and UI components. A new crate is justified only by a durable responsibility, not by a desire
to split small files.
