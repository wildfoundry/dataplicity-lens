#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <version> <SHA256SUMS> <output-formula>" >&2
  exit 2
fi

version="$1"
checksums="$2"
output="$3"
tap_repository="${HOMEBREW_TAP_REPOSITORY:-wildfoundry/homebrew-tap}"
release="dataplicity-lens-v${version}"
arm_archive="${release}-aarch64-apple-darwin.tar.gz"
intel_archive="${release}-x86_64-apple-darwin.tar.gz"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
  echo "invalid version: $version" >&2
  exit 1
}
[[ -f "$checksums" ]] || {
  echo "checksum manifest not found: $checksums" >&2
  exit 1
}
[[ "$tap_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo "invalid Homebrew tap repository: $tap_repository" >&2
  exit 1
}

checksum_for() {
  local archive="$1"
  local matches checksum
  matches="$(awk -v archive="$archive" '$2 == archive || $2 == "*" archive { print $1 }' "$checksums")"
  [[ "$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')" == 1 ]] || {
    echo "expected exactly one checksum for $archive" >&2
    return 1
  }
  checksum="$(printf '%s' "$matches")"
  [[ "$checksum" =~ ^[0-9a-fA-F]{64}$ ]] || {
    echo "invalid checksum for $archive" >&2
    return 1
  }
  printf '%s' "$checksum" | tr '[:upper:]' '[:lower:]'
}

arm_sha="$(checksum_for "$arm_archive")"
intel_sha="$(checksum_for "$intel_archive")"
mkdir -p "$(dirname "$output")"

{
  printf '%s\n' '# typed: strict'
  printf '%s\n' '# frozen_string_literal: true'
  printf '\n'
  printf '%s\n' '# Installs the complete Dataplicity Lens command suite.'
  printf '%s\n' 'class DataplicityLens < Formula'
  printf '%s\n' '  desc "System operations toolkit for Linux and macOS"'
  printf '%s\n' '  homepage "https://lens.dataplicity.com/"'
  printf '  version "%s"\n' "$version"
  printf '%s\n' '  license "Apache-2.0"'
  printf '%s\n' '  depends_on :macos'
  printf '\n'
  printf '%s\n' '  on_macos do'
  printf '%s\n' '    on_arm do'
  printf '      url "https://github.com/%s/releases/download/%s/%s"\n' "$tap_repository" "$release" "$arm_archive"
  printf '      sha256 "%s"\n' "$arm_sha"
  printf '%s\n' '    end'
  printf '%s\n' '    on_intel do'
  printf '      url "https://github.com/%s/releases/download/%s/%s"\n' "$tap_repository" "$release" "$intel_archive"
  printf '      sha256 "%s"\n' "$intel_sha"
  printf '%s\n' '    end'
  printf '%s\n' '  end'
  printf '\n'
  printf '%s\n' '  def install'
  printf '%s\n' '    bin.install Dir["bin/*"]'
  printf '%s\n' '    man1.install Dir["*.1"]'
  printf '%s\n' '    bash_completion.install Dir["completions/*.bash"]'
  printf '%s\n' '    zsh_completion.install Dir["completions/_*"]'
  printf '%s\n' '    fish_completion.install Dir["completions/*.fish"]'
  printf '%s\n' '  end'
  printf '\n'
  printf '%s\n' '  test do'
  printf '%s\n' '    output = shell_output("#{bin}/lens-top --demo --json")'
  printf '%s\n' "    assert_match '\"schema_version\": \"2\"', output"
  printf '%s\n' '    assert_match version.to_s, shell_output("#{bin}/lens-top --version")'
  printf '%s\n' '  end'
  printf '%s\n' 'end'
} > "$output"
