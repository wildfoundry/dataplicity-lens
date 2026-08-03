# Releasing

1. Ensure the release commit is on `main` and the workspace version matches the intended tag.
2. Update `CHANGELOG.md`.
3. Create and push an annotated `vMAJOR.MINOR.PATCH` tag.
4. GitHub Actions verifies the tag is reachable from `main`, runs formatting, lint, tests,
   documentation, cargo-deny and cargo-audit, then builds every artifact from a clean checkout.
5. The workflow generates man pages, shell completions, GNU/musl archives, Debian/RPM packages,
   CycloneDX SBOMs, SHA-256 checksums and provenance attestations where the repository plan permits.
6. Native packages are installed and smoke-tested before publication.
7. The release job publishes only after all architecture jobs succeed.

`workflow_dispatch` and pull requests run the same machinery in dry-run mode without publishing a
GitHub Release. No locally compiled artifact may be uploaded as a release asset.

Release jobs receive only `contents: write`, `id-token: write` and `attestations: write`. Pull-request
jobs remain read-only. Third-party actions are pinned to immutable commit SHAs.
