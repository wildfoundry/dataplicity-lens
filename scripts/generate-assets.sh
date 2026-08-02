#!/usr/bin/env bash
set -euo pipefail

binary="${1:-./target/release/lens-top}"
out="${2:-dist/generated}"
mkdir -p "$out/man" "$out/completions"
"$binary" --generate-man "$out/man/lens-top.1"
"$binary" --generate-completion bash --generate-output "$out/completions/lens-top.bash"
"$binary" --generate-completion zsh --generate-output "$out/completions/_lens-top"
"$binary" --generate-completion fish --generate-output "$out/completions/lens-top.fish"
