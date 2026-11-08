#!/usr/bin/env python3
"""Build the optional package; never install/enable/reconfigure host services."""

import argparse
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    parser.add_argument("--version", default="0.3.11+infra4")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="lens-infra-package-") as temporary:
        stage = Path(temporary)
        package = stage / "usr/lib/dataplicity-lens-infra"
        package.mkdir(parents=True)
        shutil.copytree(root / "apps/lens-infra/lens_infra", package / "lens_infra", ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "._*"))
        shutil.copy2(root / "packaging/infra/launcher.py", package / "launcher.py")
        mappings = [("packaging/infra/lens-infra", "usr/bin/lens-infra", 0o755),
                    ("packaging/infra/lens-infra.service", "usr/lib/systemd/system/lens-infra.service", 0o644),
                    ("packaging/infra/lens-ingress@.service", "usr/lib/systemd/system/lens-ingress@.service", 0o644),
                    ("packaging/infra/lens-infra.preset", "usr/lib/systemd/system-preset/00-lens-infra.preset", 0o644),
                    ("packaging/infra/lens-infra.sysusers", "usr/lib/sysusers.d/lens-infra.conf", 0o644)]
        for source, destination, mode in mappings:
            target = stage / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(root / source, target)
            target.chmod(mode)
        debian = stage / "DEBIAN"
        debian.mkdir()
        (debian / "control").write_text(f"Package: dataplicity-lens-infra\nVersion: {args.version}\nArchitecture: all\nMaintainer: WildFoundry Ltd\nDepends: python3 (>= 3.11)\nRecommends: libvirt-clients, python3-libvirt, qemu-utils, cloud-image-utils, haproxy, nftables\nDescription: Optional local Lens infrastructure controller\n Local-only policy, inventory and explicit infrastructure operations.\n Disabled on installation. No hosted account or web connection required.\n")
        # Temporary-directory mode and the builder's umask must not become system
        # directory permissions in the installed package.
        stage.chmod(0o755)
        for directory in stage.rglob("*"):
            if directory.is_dir():
                directory.chmod(0o755)
        subprocess.run(["dpkg-deb", "--build", "--root-owner-group", str(stage), str(output)], check=True)
    print(output)


if __name__ == "__main__":
    main()
