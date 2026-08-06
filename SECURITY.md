# Security policy

## Supported versions

Before 1.0, security fixes are made against the latest published release. We may ask reporters to
confirm an issue against the current `main` branch when the relevant code has changed. Older preview
releases do not receive separate security updates.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Report it privately through GitHub's
security advisory flow for this repository. Include affected versions, reproduction steps, impact
and any known mitigation. Do not include credentials, private keys or system data that is not needed
to understand the report.

We will keep the report private while it is assessed and, when a fix is required, coordinate the
release and disclosure with the reporter.

## Trust model

Lens reads local operating-system interfaces and does not require a background daemon, account or
network connection. It does not collect or transmit telemetry. Inspection runs with the invoking
user's permissions.

State-changing process and service operations are explicit, typed actions. They require a precise
target and confirmation, revalidate identity immediately before execution, and report the result.
The diagnostic shell is opened deliberately with `!`; commands entered there are passed to the
user's normal shell with the user's existing permissions. Lens never stores shell input, supplies
credentials or elevates itself.

Release artifacts are built only by GitHub Actions from a version tag reachable from `main`.
Dependencies are locked, workflow actions are pinned to immutable commits, licences and advisories
are checked, packages are smoke-tested, SBOMs and SHA-256 checksums are generated, and GitHub artifact
attestations are produced when the platform supports them. If an attestation cannot be attached,
the release still ships checksums, SBOMs and workflow provenance as the verification fallback.

Pull-request workflows use read-only tokens. Release permissions are granted only to tagged or
explicitly dispatched release jobs. No release secrets are exposed to pull-request code.

See `docs/RELEASING.md` for the full release procedure.
The reviewed assets, trust boundaries, threats and controls are recorded in
`docs/THREAT_MODEL.md`.
