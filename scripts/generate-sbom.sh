#!/usr/bin/env bash
set -euo pipefail

out="${1:-dist/sbom}"
mkdir -p "$out"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cargo cyclonedx --format json --all --output-cdx
find . -name '*.cdx.json' -not -path './target/*' -print0 | while IFS= read -r -d '' file; do
  name="$(basename "$(dirname "$file")")-$(basename "$file")"
  cp "$file" "$out/$name"
done

if ! find "$out" -type f -name '*.json' -print -quit | grep -q .; then
  echo "cargo-cyclonedx did not produce an SBOM" >&2
  exit 1
fi
