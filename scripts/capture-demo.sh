#!/usr/bin/env bash
set -euo pipefail

binary="${LENS_TOP_BINARY:-./target/release/lens-top}"
mkdir -p dist/demo
if [[ ! -x "$binary" ]]; then
  cargo build --release --locked -p lens-top
fi
COLUMNS=120 "$binary" --demo --plain > dist/demo/lens-top.txt
"$binary" --demo --json > dist/demo/lens-top.json
printf 'Wrote %s and %s\n' dist/demo/lens-top.txt dist/demo/lens-top.json
