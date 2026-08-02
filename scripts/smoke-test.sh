#!/usr/bin/env bash
set -euo pipefail

binary="${1:-./target/release/lens-top}"
"$binary" --version
"$binary" --help >/dev/null
"$binary" --demo --plain --limit 3 >/dev/null
"$binary" --demo --json --limit 3 | grep -q '"schema_version": "1"'
"$binary" --demo --jsonl --limit 3 | head -n 1 | grep -q '"record_type":"host"'
