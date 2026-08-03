#!/usr/bin/env bash
set -euo pipefail

out="${1:-dist/sbom}"
mkdir -p "$out"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# cargo-cyclonedx 0.5.7 writes bom.json beside each workspace manifest.
cargo cyclonedx --format json --all
find . -name 'bom.json' -not -path './target/*' -print0 | while IFS= read -r -d '' file; do
  package="$(basename "$(dirname "$file")")"
  cp "$file" "$out/${package}.cdx.json"
done

if ! find "$out" -type f -name '*.cdx.json' -print -quit | grep -q .; then
  echo "cargo-cyclonedx did not produce an SBOM" >&2
  exit 1
fi
