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
8. On tagged releases, the macOS jobs import the Developer ID certificate from Actions secrets,
   sign every executable, submit both architecture archives to Apple's notary service and fail closed
   if signing or notarization is unavailable.
9. The release job publishes only after all architecture jobs succeed.
10. The Homebrew job copies the verified macOS archives to the public tap release, generates the
    formula from `SHA256SUMS`, and opens an auto-merge pull request against the protected tap branch.

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

Every archive and native package contains `lens`, `lens-top`, `lens-services`, `lens-logs`,
`lens-disk`, `lens-net`, `lens-hardware`, `lens-system` and `lens-health`. Archive verification also checks the generated man page,
completions, licences, checksums and CycloneDX output. Debian and RPM packages use the musl target's
statically linked binaries so the package does not inherit the build runner's glibc requirement.

`workflow_dispatch` runs the complete machinery in dry-run mode without publishing a GitHub Release.
Pull requests run the parallel CI target matrix but do not assemble the same release bundle a second
time. No locally compiled artifact may be uploaded as a release asset.

Release jobs receive only `contents: write`, `id-token: write` and `attestations: write`. Pull-request
jobs remain read-only. Third-party actions are pinned to immutable commit SHAs.

The `release` environment must provide these Actions secrets; signing material is never read from a
maintainer workstation by the release workflow:

- `MACOS_CERTIFICATE_P12_BASE64`
- `MACOS_CERTIFICATE_PASSWORD`
- `APPLE_NOTARY_KEY_P8_BASE64`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`
- `HOMEBREW_TAP_TOKEN`, scoped to release, branch and pull-request writes in
  `wildfoundry/homebrew-tap`

The public tap must allow auto-merge while continuing to require its formula analysis check. A tag
is not complete until `brew install wildfoundry/tap/dataplicity-lens`, `brew upgrade
dataplicity-lens`, and `brew uninstall dataplicity-lens` pass on both macOS architectures.
