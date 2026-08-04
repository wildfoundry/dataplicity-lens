#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
formula="$repo/packaging/homebrew/dataplicity-lens.rb"
version="$(awk '/^\[workspace.package\]$/{p=1;next}/^\[/{p=0}p&&/^version = /{gsub(/[" ]/,"",$3);print $3;exit}' "$repo/Cargo.toml")"
formula_version="$(awk '/^  version /{gsub(/"/,"",$2);print $2;exit}' "$formula")"
if [[ -z "$version" || "$formula_version" != "$version" ]]; then
  echo "Homebrew formula version ($formula_version) does not match workspace version ($version)" >&2
  exit 1
fi
work="$(mktemp -d)"
tap="local/dataplicity-lens-test"
keep=false
[[ "${1:-}" == "--keep" ]] && keep=true
installed=false
tap_created=false

cleanup() {
  rm -rf "$work"
  if [[ "$keep" != true ]]; then
    if [[ "$installed" == true ]]; then
      brew uninstall --force dataplicity-lens >/dev/null
    fi
    if [[ "$tap_created" == true ]]; then
      brew untap "$tap" >/dev/null
    fi
  fi
}
trap cleanup EXIT

if brew list --formula dataplicity-lens >/dev/null 2>&1; then
  echo "dataplicity-lens is already installed; refusing to overwrite it" >&2
  exit 1
fi

archive="$work/dataplicity-lens-${version}.tar.gz"
COPYFILE_DISABLE=1 tar \
  --exclude=.git \
  --exclude=target \
  --exclude=stage \
  --exclude='dist/local' \
  -C "$repo" -czf "$archive" .
checksum="$(shasum -a 256 "$archive" | awk '{print $1}')"

awk -v archive="file://$archive" -v checksum="$checksum" '
  /^  url / {
    print "  url \"" archive "\""
    print "  sha256 \"" checksum "\""
    next
  }
  { print }
' "$formula" > "$work/dataplicity-lens.rb"

if brew tap | grep -qx "$tap"; then
  echo "$tap already exists; refusing to modify it" >&2
  exit 1
fi
brew tap-new --no-git "$tap" >/dev/null
tap_created=true
tap_dir="$(brew --repository "$tap")"
install -m 0644 "$work/dataplicity-lens.rb" "$tap_dir/Formula/dataplicity-lens.rb"

HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_FROM_API=1 \
  brew install --build-from-source "$tap/dataplicity-lens"
installed=true
HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_FROM_API=1 brew test "$tap/dataplicity-lens"

if [[ "$keep" == true ]]; then
  echo "Homebrew installation passed and was kept. Try: lens --demo or lens-top --once"
else
  echo "Homebrew installation and tests passed; the temporary installation was removed."
fi
