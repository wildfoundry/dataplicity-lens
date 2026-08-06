# Threat model

This review covers the Lens 1.0 command family, its local collectors, terminal interfaces, typed
process/service actions, diagnostic shell and release pipeline. It assumes the operating system,
the invoking account and explicitly installed Lens binary are trusted. Data read from processes,
logs, filesystems, network services, devices and native command output is untrusted.

## Assets and boundaries

Lens must protect the invoking user's authority, terminal integrity, local system data and the
identity of an action target. It must also preserve the integrity and provenance of release
artifacts. The important boundaries are:

- kernel and operating-system interfaces into the in-process model;
- bounded native subprocesses such as `journalctl`, `systemctl`, `mmcli` and `openssl`;
- untrusted system text rendered into a terminal or exported as plain text/JSON;
- the explicit boundary between inspection, typed actions and the unrestricted diagnostic shell;
- GitHub Actions jobs that turn a protected source revision into release artifacts.

Lens has no network client, account, resident daemon or telemetry path. It does not transmit a
snapshot. A user can deliberately redirect output, attach it to an issue or run a command in the
diagnostic shell; those are explicit actions outside automatic collection.

## Threats and controls

### Excess authority

Inspection and the diagnostic shell run with the invoking user's permissions. Lens does not cache
credentials or elevate its complete process. Process and systemd actions use existing operating-
system authorisation for one named operation. Documentation does not recommend running the suite as
root to suppress an unavailable field.

### Stale or substituted action targets

Process actions reject PID 0, PID 1 and Lens itself. They pin PID plus start-time identity, recollect
immediately before signalling and stop when the identity changed. Service actions reject whitespace,
option-like prefixes and non-exact unit names, use a bounded `systemctl` invocation, and recollect the
unit after execution. Actions are never retried automatically.

### Accidental or scripted state changes

Interactive actions have a separate review screen. Non-interactive changes require `--yes`; a
`--dry-run` path reports the exact plan without mutation. Failures are non-zero and structured output
records the observed result. The diagnostic shell is visually distinct because arbitrary shell text
cannot receive the same target validation or post-action guarantee.

### Hostile system data and terminal control sequences

Diagnostic command results remove terminal control characters before drawing. Interactive views
place collected values in fixed terminal widgets instead of generating commands from them. JSON and
plain exports preserve source data and must be treated as data, not evaluated as shell input. Lens
never constructs a shell command from a collected process, service, path, log or network value.

### Unbounded or unavailable native commands

Collector subprocesses have individual deadlines, null stdin and captured output. A timeout kills
the child, records a collection warning and does not block unrelated domains. Missing commands,
permission denial, malformed output and ordinary process races degrade locally rather than causing
an implicit privileged retry.

### Sensitive inventory disclosure

Snapshots can contain account names, process commands, addresses, log text, SIM identifiers,
certificate paths and device serials. Human cellular views mask subscriber/device identifiers;
structured output remains complete and is documented as sensitive. Lens does not read private keys.
Issue templates and troubleshooting guidance tell users to review and redact exports before sharing.

### Supply-chain substitution

Dependencies and the Rust toolchain are locked. Workflow actions are pinned to immutable commits.
Pull-request tokens are read-only. Tagged release jobs build from a clean revision reachable from
`main`, run tests, licence and advisory checks, smoke-test packages, and publish SHA-256 manifests,
CycloneDX SBOMs and provenance attestations where supported.

## Review result

The 1.0 design intentionally does not provide unattended remediation, credential management,
arbitrary remote execution or a privileged daemon. Adding any of those capabilities, a network
client, persistent command history, automatic telemetry, or a new state-changing domain requires a
new threat-model review before release.
