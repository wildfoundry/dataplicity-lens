#!/usr/bin/env bash
set -euo pipefail

out="${1:-dist/sbom}"
mkdir -p "$out"

cargo cyclonedx --format json --all >/dev/null

generated=()
while IFS= read -r -d '' file; do
  generated+=("$file")
done < <(find apps crates -type f \( -name '*.cdx.json' -o -name 'bom.json' \) -print0)

if (( ${#generated[@]} == 0 )); then
  echo "cargo-cyclonedx did not produce an SBOM" >&2
  exit 1
fi

for file in "${generated[@]}"; do
  name="$(basename "$file")"
  if [[ "$name" == "bom.json" ]]; then
    name="$(basename "$(dirname "$file")").cdx.json"
  fi
  cp "$file" "$out/$name"
  rm -f "$file"
done

if ! find "$out" -type f -name '*.cdx.json' -print -quit | grep -q .; then
  echo "no CycloneDX JSON files were collected" >&2
  exit 1
fi
