#!/usr/bin/env bash
set -euo pipefail

directory="${1:-dist/release}"
[[ -f "$directory/SHA256SUMS" ]] || { echo "missing SHA256SUMS" >&2; exit 1; }
for pattern in 'dataplicity-lens-v*-*.tar.gz' 'dataplicity-lens_*.deb' 'dataplicity-lens-*.rpm'; do
  find "$directory" -maxdepth 1 -type f -name "$pattern" -print -quit | grep -q . || { echo "missing release asset matching $pattern" >&2; exit 1; }
done
[[ $(find "$directory" -maxdepth 1 -type f -name 'dataplicity-lens-v*-*.tar.gz' | wc -l) -ge 6 ]] || { echo "expected six Linux and macOS target archives" >&2; exit 1; }
[[ $(find "$directory" -maxdepth 1 -type f -name 'dataplicity-lens_*.deb' | wc -l) -ge 2 ]] || { echo "expected two Debian packages" >&2; exit 1; }
[[ $(find "$directory" -maxdepth 1 -type f -name 'dataplicity-lens-*.rpm' | wc -l) -ge 2 ]] || { echo "expected two RPM packages" >&2; exit 1; }
for archive in "$directory"/dataplicity-lens-v*-*.tar.gz; do
  listing="$(tar -tzf "$archive")"
  for binary in lens lens-top lens-services lens-logs lens-disk lens-net lens-health; do
    grep -q "/bin/${binary}$" <<<"$listing" || { echo "$archive is missing $binary" >&2; exit 1; }
    grep -q "/${binary}.1$" <<<"$listing" || { echo "$archive is missing the $binary man page" >&2; exit 1; }
    grep -q "/completions/${binary}.bash$" <<<"$listing" || { echo "$archive is missing $binary completions" >&2; exit 1; }
  done
  grep -q '/LICENSE$' <<<"$listing" || { echo "$archive is missing LICENSE" >&2; exit 1; }
  grep -q '/NOTICE$' <<<"$listing" || { echo "$archive is missing NOTICE" >&2; exit 1; }
done
(
  cd "$directory"
  sha256sum --check SHA256SUMS
)
find "$directory" -type f -name '*.cdx.json' -print -quit | grep -q . || {
  echo "missing CycloneDX SBOM" >&2
  exit 1
}
