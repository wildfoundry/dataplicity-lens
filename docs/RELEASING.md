# Releasing

1. Ensure the release commit is on `main` and the workspace version matches the intended tag.
2. Update `CHANGELOG.md`.
3. Create and push an annotated `vMAJOR.MINOR.PATCH` tag.
4. GitHub Actions verifies the tag is reachable from `main`, runs formatting, lint, tests,
   documentation, cargo-deny and cargo-audit, then builds every artifact from a clean checkout.
5. The workflow generates man pages, shell completions, GNU/musl archives, Debian/RPM packages,
   CycloneDX SBOMs, SHA-256 checksums and provenance attestations where the repository plan permits.
6. Native packages are installed and smoke-tested before publication.
7. ARMv6 hard-float Raspberry Pi binaries are run under emulation and their `armhf` packages are inspected.
8. The release job publishes only after all architecture jobs succeed.
9. The separate `Update Homebrew tap` workflow runs when the GitHub Release is published, downloads
    the verified macOS archives from that release, recomputes their checksums, and opens a pull
    request against the protected tap branch. The formula installs those archives directly from the
    Lens GitHub Release (same pattern as `dataplicity-cli`).

To reproduce package assembly after building both targets and generating the `lens-top` man page and
completions:

```sh
VERSION=0.3.0 TARGET=x86_64-unknown-linux-gnu PACKAGE_NATIVE=false scripts/package-suite.sh
VERSION=0.3.0 TARGET=x86_64-unknown-linux-musl DEB_ARCH=amd64 RPM_ARCH=x86_64 \
  NATIVE_OUTPUT_DIR=dist/x86_64-unknown-linux-gnu scripts/package-suite.sh
scripts/smoke-suite.sh target/x86_64-unknown-linux-musl/release
# Raspberry Pi OS 32-bit builds use cross:
cross build --release --locked --target arm-unknown-linux-musleabihf --workspace
VERSION=0.3.0 TARGET=arm-unknown-linux-musleabihf DEB_ARCH=armhf RPM_ARCH=armv6hl \
  NATIVE_OUTPUT_DIR=dist/arm-unknown-linux-gnueabihf scripts/package-suite.sh
scripts/verify-release.sh dist/release
```

Every archive and native package contains `lens`, `lens-top`, `lens-services`, `lens-containers`,
`lens-logs`, `lens-disk`, `lens-net`, `lens-hardware`, `lens-system` and `lens-health`. Archive verification also checks the generated man page,
completions, licences, checksums and CycloneDX output. Debian and RPM packages use the musl target's
statically linked binaries so the package does not inherit the build runner's glibc requirement.

`workflow_dispatch` runs the complete machinery in dry-run mode without publishing a GitHub Release.
Pull requests run the parallel CI target matrix but do not assemble the same release bundle a second
time. No locally compiled artifact may be uploaded as a release asset.

Release jobs receive only `contents: write`, `id-token: write` and `attestations: write`. Pull-request
jobs remain read-only. Third-party actions are pinned to immutable commit SHAs.

Homebrew publication follows the `dataplicity-cli` repository's release-triggered tap workflow. The
Lens repository must provide `HOMEBREW_TAP_TOKEN` owned by `dataplicity-release-bot` with contents write access to
`wildfoundry/homebrew-tap` (enough to push a formula branch and open a PR), plus the repository
variables `HOMEBREW_TAP_REPOSITORY` and `HOMEBREW_FORMULA_NAME`. The tap PR is reviewed and merged
only after its required formula analysis check passes. A tag is not complete until adding
`wildfoundry/tap`, trusting the formula, installing `dataplicity-lens`, upgrading it and uninstalling
it pass on both macOS architectures.
