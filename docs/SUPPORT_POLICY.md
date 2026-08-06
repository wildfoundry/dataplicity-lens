# Support and compatibility policy

Lens `0.3.x` is the release-candidate contract for 1.0. The compatibility promises below take
effect with 1.0.0; preview releases can still correct a contract before that tag.

## Tier-1 platforms for 1.x

| Platform | Minimum release | Architectures | Native service/log source |
| --- | --- | --- | --- |
| Debian | 12 (Bookworm) | x86-64, ARM64 | systemd and journald |
| Ubuntu | 24.04 LTS | x86-64, ARM64 | systemd and journald |
| Raspberry Pi OS | Bookworm | ARM64 and 32-bit hard-float (`armhf`) | systemd and journald |
| macOS | 15 | Apple silicon and Intel | launchd and unified log |

Tier 1 means that the release pipeline builds the relevant architecture, package installation and
the platform's native suite are tested, and a release-blocking regression is fixed or explicitly
stops the release. The real-hardware qualification record is maintained separately from this
contract and must be complete before 1.0.0.

Other current systemd Linux distributions and newer operating-system releases are supported on a
best-effort basis when the documented native facilities are present. Minimal containers expose only
their own namespaces and mounted facilities. A missing optional facility limits that domain rather
than making unrelated views fail.

## Stable 1.x surface

Throughout 1.x, Lens keeps these interfaces compatible:

- the nine command names and documented common flags;
- exit-status and stdout/stderr behaviour;
- schema version 2 field meanings and units;
- configuration-file keys and documented `LENS_*` environment variables;
- package names and installed command names.

Additive optional JSON fields and new enum values can appear in a minor release. Scripts must ignore
unknown fields and values. Removing a field, changing its meaning or unit, renaming a command, or
changing an existing exit-status meaning requires a new major version unless the old behaviour is a
security vulnerability.

## Deprecation and fixes

The latest 1.x release receives bug and security fixes. A deprecated command, flag, configuration
key or field remains functional for the rest of 1.x and is named in release notes. Platform support
can be withdrawn only when the operating-system vendor no longer supports the release or the native
facility is no longer maintainable; the change is announced before the affected Lens release.

Security reports follow [`SECURITY.md`](../SECURITY.md). Usage questions and compatibility reports
belong in GitHub Issues and should include the Lens version, operating system, architecture and
relevant `collection_warnings`.
