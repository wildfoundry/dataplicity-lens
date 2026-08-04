# Changelog

All notable changes to Dataplicity Lens will be documented here. The project follows semantic
versioning once the first public release is tagged.

## [Unreleased]

### Added

- Shared Linux process, history, diagnostics, terminal UI and output architecture.
- A production-safe `lens-top` process explorer with TUI, plain, JSON and JSON Lines output.
- Deterministic demo mode, fixture tests, packaging and release automation.
- The `lens` cockpit plus service, log, disk, network and health specialist binaries.
- Schema-version-2 system entities and cross-domain relationships.
- Complete-suite GNU/musl archives, Debian/RPM packaging and Pages documentation.
- Raspberry Pi OS packages for 64-bit and 32-bit Raspberry Pi systems.

### Changed

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

## [0.2.0] - unreleased

Lens suite MVP release target.

## [0.1.0] - unreleased

Initial release target.
