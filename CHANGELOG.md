# Changelog

All notable changes to Dataplicity Lens will be documented here. The project follows semantic
versioning once the first public release is tagged.

## [Unreleased]

### Added

- The 1.x platform/support policy and a threat model covering local data, actions, hostile input,
  privilege boundaries and release provenance.
- Operator documentation for platform compatibility, the command-line and exit-status contract,
  structured-output fields, package verification and upgrades, and practical troubleshooting.
- Shared Linux process, history, diagnostics, terminal UI and output architecture.
- A production-safe `lens-top` process explorer with TUI, plain, JSON and JSON Lines output.
- Deterministic demo mode, fixture tests, packaging and release automation.
- The `lens` cockpit plus service, log, disk, network and health specialist binaries.
- Schema-version-2 system entities and cross-domain relationships.
- Complete-suite GNU/musl archives, Debian/RPM packaging and Pages documentation.
- Raspberry Pi OS packages for 64-bit and 32-bit Raspberry Pi systems.
- Responsive diagnostic shell overlays that keep live system data visible while local commands run.
- Local clock information throughout the interactive suite.
- Guarded interactive process signals and systemd service actions with pinned targets, explicit
  review, confirmation and post-action verification.
- A `lens-system` view for clock/NTP state, resolver configuration, local accounts and groups, and
  public certificate files visible to the invoking user.
- A `lens-hardware` view for device identity, firmware, Linux thermal/hwmon sensors, Raspberry Pi
  power and throttling status, and USB/serial inventory on Linux and macOS.
- Live RX/TX rate charts in `lens-net`, with per-interface activity and responsive compact output.
- CI-only Developer ID signing and Apple notarization for native Apple Silicon and Intel release
  archives, plus the same release-triggered protected Homebrew tap PR flow used by `dataplicity-cli`.

### Changed

- Security and repository settings docs no longer describe an internal-only repository, ready for
  the public open-source release.
- Interactive commands now choose contrasting colours for light and dark terminal backgrounds, with
  explicit `--theme` and `LENS_THEME` overrides for terminals that do not report their background.
- Primary text now follows the terminal's own foreground and automatic accent colours retain
  contrast when browser terminals omit their light-background metadata.
- Cockpit and specialist frames now overwrite in place before clearing trailing cells; static views
  avoid one-second clock repaints that caused flashing on slower remote terminals.
- Browser terminals receive the contrast-tested RGB palette even when they omit colour capability
  metadata, and every interactive command explicitly clears and resets the terminal on exit.
- Cockpit log summaries report flagged errors and warnings instead of a capped row count. System
  summaries report clock, DNS and login context instead of capped certificate totals.
- System detail focuses on login-capable identities and locally managed certificates, with subject,
  issuer, expiry and path metadata where OpenSSL is available; bulk root CA catalogues are omitted.
- System context is split into visible Clock/NTP, DNS, Users, Groups and Certificates sections;
  Tab, Shift+Tab and keys 1–5 jump directly between them.
- `--once` now produces a plain snapshot consistently across the cockpit and every specialist;
  irrelevant specialist flags fail clearly, visible log fields participate in filtering, and an
  explicit `lens-top --interval` is honoured for one-shot measurement.
- Anchored cockpit and specialist footers clear rows vacated by shorter detail screens, preventing
  list content from remaining underneath storage, log and other drill-down views.
- Direct Rust dependencies are updated to their current Rust-1.88-compatible releases.
- Terminal refreshes are emitted as synchronized frames, process views respect the terminal's own
  background colour, and the fixed-width updating badge no longer shifts the header.
- Linux network collection falls back to sysfs and procfs when minimal containers do not include
  `iproute2`; storage health findings open the affected mount directly.
- Every command page now documents operational workflows, screen fields, controls, filters, action
  safety, platform differences and incomplete-data semantics; a cross-tool operations guide connects
  the views into practical fault-finding sequences.
- The `lens` cockpit now opens with useful host and process status while slower service, log,
  storage and network checks continue in the background.
- Interactive specialist screens now open immediately with a loading state and collect only the
  selected domain; opening Storage no longer waits for unrelated log, service and network scans.
- Logs now opens as a navigable list with message details. On macOS it shows the latest minute
  first, then loads the previous hour in the background without moving the selected message.
- Health findings can now be selected and opened to review their evidence and suggested checks;
  common service and storage findings appear while the remaining checks continue.
- Storage and Network publish their primary data before slower device, open-file and listener
  probes finish.
- Documentation navigation groups the specialist commands under the suite instead of promoting one
  command on its own.
- Search now opens as a modal card instead of replacing the specialist screen.
- Process lists keep the selected row centred when possible, including after reaching the last row.
- Storage summaries name the root filesystem clearly, and every specialist uses the same host-name
  discovery path on macOS.
- User documentation now leads with installation and live collection; deterministic sample data is
  kept in contributor documentation.
- Pull requests no longer repeat complete release-bundle assembly after the parallel CI builds.
- Cockpit log summaries now distinguish collection failure from a genuine zero-entry result.
- Wide storage detail views use paired fields instead of leaving most of the terminal empty.
- Installation guidance now starts with Raspberry Pi OS and Debian packages, with macOS as a
  supported secondary path.
- User documentation now describes shipped behavior only; roadmap and unimplemented-feature copy
  has been removed from the product site.
- Debian and RPM packages now use statically linked binaries so Raspberry Pi OS and Debian installs
  do not inherit the build runner's glibc version.
- System-section navigation keeps rows above and below the selection visible when the cursor crosses
  a section boundary instead of jumping the selected row to the top of the terminal.

## [0.2.0] - unreleased

Lens suite MVP release target.

## [0.1.0] - unreleased

Initial release target.
