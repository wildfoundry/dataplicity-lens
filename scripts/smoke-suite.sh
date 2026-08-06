#!/usr/bin/env bash
set -euo pipefail

directory="${1:-./target/release}"
specialists=(lens-services lens-logs lens-disk lens-net lens-hardware lens-system lens-health)
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

"$directory/lens" --demo --plain >/dev/null
scripts/smoke-test.sh "$directory/lens-top"
for binary in "${specialists[@]}"; do
  "$directory/$binary" --demo --plain --limit 2 >/dev/null
  "$directory/$binary" --demo --json --limit 2 > "$temporary/$binary.json"
  grep -q '"schema_version": "2"' "$temporary/$binary.json"
done
