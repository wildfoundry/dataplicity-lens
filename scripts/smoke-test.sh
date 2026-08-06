#!/usr/bin/env bash
set -euo pipefail

binary="${1:-./target/release/lens-top}"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
"$binary" --version
"$binary" --help >/dev/null
"$binary" --demo --plain --limit 3 >/dev/null
"$binary" --demo --json --limit 3 > "$temporary/lens-top.json"
grep -q '"schema_version": "2"' "$temporary/lens-top.json"
"$binary" --demo --jsonl --limit 3 > "$temporary/lens-top.jsonl"
head -n 1 "$temporary/lens-top.jsonl" | grep -q '"record_type":"host"'
