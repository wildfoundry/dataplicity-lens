#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET is required}"
version="${VERSION:-0.3.0}"
native_packages="${PACKAGE_NATIVE:-true}"
out="${OUTPUT_DIR:-dist/${TARGET}}"
stage="${STAGE_DIR:-stage/${TARGET}}"
native_out="${NATIVE_OUTPUT_DIR:-$out}"
binaries=(lens lens-top lens-services lens-containers lens-logs lens-disk lens-net lens-hardware lens-system lens-health)

rm -rf "$stage" "$out"
mkdir -p "$stage/usr/bin" "$stage/usr/share/doc/dataplicity-lens" "$out"
for binary in "${binaries[@]}"; do
  install -m 0755 "target/${TARGET}/release/${binary}" "$stage/usr/bin/${binary}"
done
if [[ "$TARGET" == *linux* ]]; then
  install -m 0755 packaging/systemd/lens-reporting-enable "$stage/usr/bin/lens-reporting-enable"
  for binary in lens-daemon lens-publisher; do
    install -m 0755 "target/${TARGET}/release/${binary}" "$stage/usr/bin/${binary}"
  done
  mkdir -p "$stage/usr/lib/systemd/system" "$stage/usr/lib/sysusers.d" "$stage/usr/lib/tmpfiles.d" "$stage/usr/lib/systemd/system-preset"
  install -m 0644 packaging/systemd/*.service "$stage/usr/lib/systemd/system/"
  install -m 0644 packaging/systemd/lens-reporting.sysusers "$stage/usr/lib/sysusers.d/lens-reporting.conf"
  install -m 0644 packaging/systemd/lens-reporting.tmpfiles "$stage/usr/lib/tmpfiles.d/lens-reporting.conf"
  install -m 0644 packaging/systemd/lens-reporting.preset "$stage/usr/lib/systemd/system-preset/00-lens-reporting.preset"
fi
install -m 0644 docs/FLEET_REPORTING.md docs/REPORTING_PROTOCOL.md LICENSE NOTICE README.md PHILOSOPHY.md SECURITY.md CHANGELOG.md "$stage/usr/share/doc/dataplicity-lens/"
if [[ -d dist/generated ]]; then
  mkdir -p "$stage/usr/share/man/man1" "$stage/usr/share/bash-completion/completions" "$stage/usr/share/zsh/site-functions" "$stage/usr/share/fish/vendor_completions.d"
  for binary in "${binaries[@]}"; do
    install -m 0644 "dist/generated/man/${binary}.1" "$stage/usr/share/man/man1/${binary}.1"
    gzip -n -f "$stage/usr/share/man/man1/${binary}.1"
    install -m 0644 "dist/generated/completions/${binary}.bash" "$stage/usr/share/bash-completion/completions/${binary}"
    install -m 0644 "dist/generated/completions/_${binary}" "$stage/usr/share/zsh/site-functions/_${binary}"
    install -m 0644 "dist/generated/completions/${binary}.fish" "$stage/usr/share/fish/vendor_completions.d/${binary}.fish"
  done
fi
root="dataplicity-lens-v${version}-${TARGET}"
mkdir -p "$out/$root"
cp -a "$stage/usr/bin" "$out/$root/"
if [[ "$TARGET" == *linux* ]]; then
  mkdir -p "$out/$root/lib" "$out/$root/docs"
  cp -a "$stage/usr/lib/". "$out/$root/lib/"
  install -m 0644 docs/FLEET_REPORTING.md docs/REPORTING_PROTOCOL.md "$out/$root/docs/"
fi
install -m 0644 LICENSE NOTICE README.md "$out/$root/"
if [[ -d dist/generated ]]; then
  mkdir -p "$out/$root/completions"
  install -m 0644 dist/generated/man/*.1 "$out/$root/"
  install -m 0644 dist/generated/completions/* "$out/$root/completions/"
fi
tar -C "$out" -czf "$out/${root}.tar.gz" "$root"
rm -rf "$out/$root"
if [[ "$native_packages" != true ]]; then
  exit 0
fi
: "${DEB_ARCH:?DEB_ARCH is required when PACKAGE_NATIVE=true}"
: "${RPM_ARCH:?RPM_ARCH is required when PACKAGE_NATIVE=true}"
mkdir -p "$native_out"
fpm --input-type dir --output-type deb --name dataplicity-lens --version "$version" --architecture "$DEB_ARCH" --license Apache-2.0 --url https://github.com/wildfoundry/dataplicity-lens --description "Dataplicity Lens Linux operations suite" --maintainer "WildFoundry Ltd" --chdir "$stage" --package "$native_out/dataplicity-lens_${version}_${DEB_ARCH}.deb" .
fpm --input-type dir --output-type rpm --name dataplicity-lens --version "$version" --iteration 1 --architecture "$RPM_ARCH" --license Apache-2.0 --url https://github.com/wildfoundry/dataplicity-lens --description "Dataplicity Lens Linux operations suite" --maintainer "WildFoundry Ltd" --chdir "$stage" --package "$native_out/dataplicity-lens-${version}-1.${RPM_ARCH}.rpm" .
