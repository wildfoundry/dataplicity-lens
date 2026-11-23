#!/usr/bin/python3 -I
"""Read-only local server summary. No credentials, environment dump or network requests."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def text(value, limit=96):
    return re.sub(r"[^a-zA-Z0-9 _./:@+-]", "?", str(value))[:limit]


def summary():
    try:
        metadata = json.loads(Path("/etc/lens-instance.json").read_bytes()[:65536])
    except (OSError, ValueError):
        metadata = {}
    result = {key: metadata.get(key, "unknown") for key in (
        "host_id", "instance", "service", "environment", "class", "network",
        "configuration_revision", "instance_configuration_revision")}
    profile = metadata.get("build_profile", {})
    result["build_profile"] = profile.get("name", "unknown") if isinstance(profile, dict) else "unknown"
    result["lens_version"] = profile.get("lens_version", "not selected") if isinstance(profile, dict) else "unknown"
    result["load_1m"] = round(os.getloadavg()[0], 2)
    fs = os.statvfs("/")
    result["root_total_gib"] = round(fs.f_blocks * fs.f_frsize / 2**30, 1)
    result["root_used_gib"] = round((fs.f_blocks - fs.f_bfree) * fs.f_frsize / 2**30, 1)
    try:
        state = subprocess.run(["/usr/bin/systemctl", "is-system-running"], capture_output=True, timeout=.5)
        result["system_state"] = text(state.stdout.decode().strip(), 24) or "unknown"
    except (OSError, subprocess.TimeoutExpired):
        result["system_state"] = "unknown"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--welcome", action="store_true")
    args = parser.parse_args()
    data = summary()
    if args.json:
        print(json.dumps(data, sort_keys=True))
        return
    safe = {k: text(v) for k, v in data.items()}
    print("Dataplicity server profile")
    print(f"  {safe['service']}  |  {safe['environment'].upper()}  |  {safe['instance']}")
    print(f"  Class {safe['class']}  |  configuration {safe['instance_configuration_revision']}  |  build {safe['build_profile']}")
    print(f"  System {safe['system_state']}  |  load {safe['load_1m']}  |  root {safe['root_used_gib']}/{safe['root_total_gib']} GiB")
    tools = "  Local status: dp-status"
    if Path("/usr/bin/lens").is_file():
        tools += "     Inspection: lens --help"
    print(tools)
    if Path("/run/reboot-required").exists():
        print("  Reboot required: plan a maintenance window.")
    if args.welcome:
        print()


if __name__ == "__main__":
    main()
