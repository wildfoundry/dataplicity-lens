#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

arm_sha="$(printf 'a%.0s' {1..64})"
intel_sha="$(printf 'b%.0s' {1..64})"
manifest="$work/SHA256SUMS"
formula="$work/dataplicity-lens.rb"

printf '%s  %s\n' "$arm_sha" "dataplicity-lens-v0.3.0-aarch64-apple-darwin.tar.gz" > "$manifest"
printf '%s  %s\n' "$intel_sha" "dataplicity-lens-v0.3.0-x86_64-apple-darwin.tar.gz" >> "$manifest"
"$repo/scripts/generate-homebrew-formula.sh" 0.3.0 "$manifest" "$formula"

ruby -c "$formula" >/dev/null
grep -q "dataplicity-lens-v0.3.0-aarch64-apple-darwin.tar.gz" "$formula"
grep -q "dataplicity-lens-v0.3.0-x86_64-apple-darwin.tar.gz" "$formula"
grep -q "$arm_sha" "$formula"
grep -q "$intel_sha" "$formula"
grep -q 'bin.install Dir\["bin/\*"\]' "$formula"
grep -q 'assert_match version.to_s' "$formula"

if "$repo/scripts/generate-homebrew-formula.sh" 0.3.0 /dev/null "$work/invalid.rb" 2>/dev/null; then
  echo "formula generation accepted a manifest without release archives" >&2
  exit 1
fi
