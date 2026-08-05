# Lens 1.0 contract

This is the contract Lens is working toward. A feature appearing in a beta build does not by itself
make the build 1.0.

## Product boundary

Lens is a standalone local operations toolkit built from the system work WildFoundry does every day
while supporting Dataplicity. It does not require a Dataplicity account or service connection. It is
not a fleet manager, remote shell, resident daemon or unattended remediation system.

## Tier-1 target

The intended tier-1 matrix is:

| Platform | Architecture | Required system facilities |
| --- | --- | --- |
| macOS 13 or newer | Apple silicon, Intel | Standard macOS command-line tools |
| Ubuntu 22.04 and 24.04 | x86_64, aarch64 | systemd and journald |
| Debian 12 and 13 | x86_64, aarch64 | systemd and journald |
| Raspberry Pi OS Bookworm | armhf, aarch64 | systemd and journald |

Release artifacts and CI builds are not substitutes for the real-hardware qualification recorded in
the 1.0 project. Other Unix-like systems are best effort until they are added to this matrix.

## Interaction contract

- A terminal UI paints before supplemental collection starts and remains navigable while data loads.
- Specialists collect their own domain and show partial/unavailable data without blocking unrelated
  work.
- External operating-system commands have individual deadlines.
- Services, logs, storage, networking and health use the same move, inspect, search, refresh and quit
  keys.
- Interactive views redraw on terminal resize, use additional rows and columns when available, and
  remove secondary detail when space is limited without hiding navigation or primary values.
- Plain text is for people. JSON is schema-versioned for scripts. Diagnostics go to stderr or
  `collection_warnings`, not into structured stdout.
- `--limit` defaults to 1,000 for suite specialists. `--limit 0` explicitly requests every available
  row in the selected source/time range.

## Action contract

Lens 1.0 may perform only named, typed actions. It does not accept arbitrary commands.

- Process actions: TERM, KILL, HUP, INT, STOP and CONT for one exact PID.
- Linux service actions: start, stop, restart, enable and disable for one exact systemd unit.
- Every action has a dry-run plan, requires explicit confirmation, has a deadline where the OS
  operation can block, and reports the observed post-action state.
- Process identity is checked with PID and start time immediately before execution.
- Lens uses the invoking account and does not keep a privileged daemon or require the TUI to run as
  root.
- launchd service actions remain unavailable until Lens can target and verify them as precisely as
  systemd actions.

See [ACTIONS.md](ACTIONS.md) for commands and failure behavior.

## Data compatibility

Schema version 2 may gain additive fields and collections. Removing a field, changing its meaning or
changing a unit requires a new schema version. A specialist emits the common document shape but
leaves unrelated, uncollected domain arrays empty.

Throughout the 1.x line, documented commands and flags are not removed without a deprecation notice
in at least one minor release. Security fixes may narrow unsafe behavior without that notice.

## Release gates

1. The beta baseline installs cleanly and works for routine WildFoundry support investigations.
2. The platform, CLI, schema and action contracts are documented and contract-tested.
3. Security review, trusted distribution and real-hardware qualification are complete.
4. At least one release candidate completes its agreed soak with no release blockers.
5. Named product, engineering and release owners record a go decision.

The documentation site remains private until WildFoundry explicitly decides otherwise.
