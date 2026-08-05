#!/usr/bin/env bash
set -euo pipefail

binary="${1:-./target/release/lens-top}"
out="${2:-dist/generated}"
mkdir -p "$out/man" "$out/completions"
"$binary" --generate-man "$out/man/lens-top.1"
"$binary" --generate-completion bash --generate-output "$out/completions/lens-top.bash"
"$binary" --generate-completion zsh --generate-output "$out/completions/_lens-top"
"$binary" --generate-completion fish --generate-output "$out/completions/lens-top.fish"
directory="$(dirname "$binary")"
for command in lens lens-services lens-logs lens-disk lens-net lens-hardware lens-system lens-health; do
  "$directory/$command" --generate-man "$out/man/$command.1"
  "$directory/$command" --generate-completion bash --generate-output "$out/completions/$command.bash"
  "$directory/$command" --generate-completion zsh --generate-output "$out/completions/_$command"
  "$directory/$command" --generate-completion fish --generate-output "$out/completions/$command.fish"
done
