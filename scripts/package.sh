#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET is required}"
: "${DEB_ARCH:?DEB_ARCH is required}"
: "${RPM_ARCH:?RPM_ARCH is required}"

version="${VERSION:-0.1.0}"
binary="${BINARY:-target/${TARGET}/release/lens-top}"
generated="${GENERATED_DIR:-dist/generated}"
out="${OUTPUT_DIR:-dist/${TARGET}}"
stage="${STAGE_DIR:-stage/${TARGET}}"

rm -rf "$stage" "$out"
mkdir -p \
  "$stage/usr/bin" \
  "$stage/usr/share/doc/lens-top" \
  "$stage/usr/share/man/man1" \
  "$stage/usr/share/bash-completion/completions" \
  "$stage/usr/share/zsh/site-functions" \
  "$stage/usr/share/fish/vendor_completions.d" \
  "$out"

install -m 0755 "$binary" "$stage/usr/bin/lens-top"
install -m 0644 LICENSE README.md PHILOSOPHY.md SECURITY.md CHANGELOG.md \
  "$stage/usr/share/doc/lens-top/"
install -m 0644 packaging/common/README.txt "$stage/usr/share/doc/lens-top/PACKAGE-README.txt"
install -m 0644 "$generated/man/lens-top.1" "$stage/usr/share/man/man1/lens-top.1"
install -m 0644 "$generated/completions/lens-top.bash" \
  "$stage/usr/share/bash-completion/completions/lens-top"
install -m 0644 "$generated/completions/_lens-top" "$stage/usr/share/zsh/site-functions/_lens-top"
install -m 0644 "$generated/completions/lens-top.fish" \
  "$stage/usr/share/fish/vendor_completions.d/lens-top.fish"

gzip -n -f "$stage/usr/share/man/man1/lens-top.1"

archive_root="lens-top-v${version}-${TARGET}"
mkdir -p "$out/$archive_root"
install -m 0755 "$binary" "$out/$archive_root/lens-top"
install -m 0644 LICENSE README.md "$out/$archive_root/"
install -m 0644 "$generated/man/lens-top.1" "$out/$archive_root/lens-top.1"
mkdir -p "$out/$archive_root/completions"
install -m 0644 "$generated/completions/"* "$out/$archive_root/completions/"
tar -C "$out" -czf "$out/${archive_root}.tar.gz" "$archive_root"
rm -rf "$out/$archive_root"

fpm \
  --input-type dir \
  --output-type deb \
  --name lens-top \
  --version "$version" \
  --architecture "$DEB_ARCH" \
  --license MIT \
  --url "https://github.com/wildfoundry/dataplicity-lens" \
  --description "A coherent, modern Linux process explorer" \
  --maintainer "WildFoundry Ltd" \
  --chdir "$stage" \
  --package "$out/lens-top_${version}_${DEB_ARCH}.deb" \
  .

fpm \
  --input-type dir \
  --output-type rpm \
  --name lens-top \
  --version "$version" \
  --iteration 1 \
  --architecture "$RPM_ARCH" \
  --license MIT \
  --url "https://github.com/wildfoundry/dataplicity-lens" \
  --description "A coherent, modern Linux process explorer" \
  --maintainer "WildFoundry Ltd" \
  --chdir "$stage" \
  --package "$out/lens-top-${version}-1.${RPM_ARCH}.rpm" \
  .

cp "$binary" "$out/lens-top-v${version}-${TARGET}"
