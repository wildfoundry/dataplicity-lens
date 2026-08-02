#!/usr/bin/env bash
set -euo pipefail

directory="${1:-dist/release}"
required=(
  "lens-top-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
  "lens-top-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
  "lens-top-v0.1.0-x86_64-unknown-linux-musl.tar.gz"
  "lens-top-v0.1.0-aarch64-unknown-linux-musl.tar.gz"
  "lens-top_0.1.0_amd64.deb"
  "lens-top_0.1.0_arm64.deb"
  "lens-top-0.1.0-1.x86_64.rpm"
  "lens-top-0.1.0-1.aarch64.rpm"
  "SHA256SUMS"
)
for file in "${required[@]}"; do
  [[ -f "$directory/$file" ]] || { echo "missing release asset: $file" >&2; exit 1; }
done
(
  cd "$directory"
  sha256sum --check SHA256SUMS
)
find "$directory" -type f -name '*.cdx.json' -print -quit | grep -q . || {
  echo "missing CycloneDX SBOM" >&2
  exit 1
}
