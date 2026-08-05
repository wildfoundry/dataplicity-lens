#!/usr/bin/env bash
set -euo pipefail

directory="${LENS_BINARY_DIR:-./target/release}"
binaries=(lens lens-top lens-services lens-logs lens-disk lens-net lens-system lens-health)
mkdir -p dist/demo
if [[ ! -x "$directory/lens" ]]; then
  cargo build --release --locked --workspace
fi
for binary in "${binaries[@]}"; do
  COLUMNS=120 "$directory/$binary" --demo --plain > "dist/demo/${binary}.txt"
  "$directory/$binary" --demo --json > "dist/demo/${binary}.json"
done
printf 'Wrote deterministic plain and JSON captures for %s\n' "${binaries[*]}"
