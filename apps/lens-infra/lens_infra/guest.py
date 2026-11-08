#!/usr/bin/env python3
"""Fixed guest workload runner. Installed only inside newly created Lens VMs."""

import hashlib
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tarfile


def command(argv, *, env=None, timeout=300):
    p = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, env=env, timeout=timeout, check=False)
    if p.returncode:
        raise RuntimeError("workload command failed")


def manifest():
    return json.loads(Path("/run/lens/manifest.json").read_text())


def unit(name, content):
    path = Path("/etc/systemd/system") / name
    path.write_text(content)
    path.chmod(0o644)
    command(["/usr/bin/systemctl", "daemon-reload"])
    command(["/usr/bin/systemctl", "reset-failed", name])
    command(["/usr/bin/systemctl", "enable", "--now", name])


def firewall_rules(m, existing=False):
    rules = "delete table inet lens_guest\n" if existing else ""
    rules += "table inet lens_guest { chain input { type filter hook input priority -10; policy drop;\n"
    rules += 'iifname "lo" accept\nct state established,related accept\nudp sport 67 udp dport 68 accept\n'
    rules += "icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert } accept\n"
    if m.get("allowed_port"):
        rules += "tcp dport " + str(int(m["allowed_port"])) + " accept\n"
    rules += "}\n}\n"
    return rules


def firewall(m):
    # This runs only inside a newly created, single-service VM. A shim and its
    # application share loopback; peers can reach only the declared service port.
    path = Path("/run/lens/firewall.nft")
    existing = subprocess.run(["/usr/sbin/nft", "list", "table", "inet", "lens_guest"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    rules = firewall_rules(m, existing)
    path.write_text(rules)
    command(["/usr/sbin/nft", "--check", "-f", str(path)])
    command(["/usr/sbin/nft", "-f", str(path)])


def apply():
    m = manifest()
    firewall(m)
    if m["kind"] == "oci":
        for container in m["containers"]:
            name = "lens-" + container["name"]
            image = container["image"]
            registry_env = None
            if container.get("registry_secret"):
                config = Path("/run/lens/registry") / container["name"]
                config.mkdir(parents=True, mode=0o700, exist_ok=True)
                shutil.copyfile(Path("/run/lens/secrets") / container["registry_secret"], config / "config.json")
                (config / "config.json").chmod(0o600)
                registry_env = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "DOCKER_CONFIG": str(config)}
            if container.get("archive"):
                image = container["image_id"]
                archive = Path(container["archive"])
                with archive.open("rb") as f:
                    if hashlib.file_digest(f, "sha256").hexdigest() != container["archive_sha256"]:
                        raise RuntimeError("artifact digest mismatch")
                if subprocess.run(["/usr/bin/docker", "image", "inspect", image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                    command(["/usr/bin/docker", "load", "--input", str(archive)])
            elif subprocess.run(["/usr/bin/docker", "image", "inspect", image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                command(["/usr/bin/docker", "pull", image], env=registry_env)
            command(["/usr/bin/docker", "image", "inspect", image])
            exists = subprocess.run(["/usr/bin/docker", "container", "inspect", name],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
            if not exists:
                argv = ["/usr/bin/docker", "create", "--name", name, "--network", "host", "--read-only",
                        "--cap-drop=ALL", "--security-opt=no-new-privileges", "--pids-limit=512", "--tmpfs", "/tmp:rw,noexec,nosuid,size=64m"]
                for secret in container.get("secret_files", []):
                    argv += ["--mount", f"type=bind,source=/run/lens/secrets/{secret},target=/run/secrets/{secret},readonly"]
                argv += [image, *container.get("command", [])]
                command(argv)
            unit(name + ".service", "[Unit]\nAfter=docker.service\nRequires=docker.service\nStartLimitIntervalSec=300\nStartLimitBurst=3\n"
                 "[Service]\nExecStart=/usr/bin/docker start --attach " + name + "\nExecStop=/usr/bin/docker stop --time 30 " + name +
                 "\nRestart=on-failure\nRestartSec=10\nTimeoutStopSec=40\nStandardOutput=null\nStandardError=null\n[Install]\nWantedBy=multi-user.target\n")
    elif m["kind"] == "github-runner":
        r = m["runner"]
        directory = Path("/opt/actions-runner")
        directory.mkdir(mode=0o750, exist_ok=True)
        if (directory / ".lens-completed").exists():
            return  # A consumed ephemeral runner must never register or accept another job.
        if not (directory / "run.sh").exists():
            archive = Path(r["archive"])
            with archive.open("rb") as f:
                if hashlib.file_digest(f, "sha256").hexdigest() != r["sha256"]:
                    raise RuntimeError("runner archive digest mismatch")
            with tarfile.open(archive) as tar:
                tar.extractall(directory, filter="data")
        if subprocess.run(["/usr/bin/id", "lens-runner"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            command(["/usr/sbin/useradd", "--system", "--create-home", "--home-dir", str(directory), "--shell", "/bin/bash", "lens-runner"])
        command(["/usr/bin/chown", "-R", "lens-runner:lens-runner", str(directory)])
        if not (directory / ".runner").exists():
            token = (Path("/run/lens/secrets") / r["registration_secret"]).read_text().strip()
            env = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "HOME": str(directory), "ACTIONS_RUNNER_INPUT_TOKEN": token}
            argv = ["/usr/sbin/runuser", "--preserve-environment", "-u", "lens-runner", "--", str(directory / "config.sh"),
                     "--unattended", "--url", r["url"], "--name", m["instance"], "--labels", ",".join(r.get("labels", [])),
                     "--runnergroup", r.get("group", "Default"), "--no-default-labels", "--disableupdate"]
            if r.get("ephemeral", True):
                argv.append("--ephemeral")
            command(argv, env=env)
            del token, env
        wrapper = directory / "lens-runner.sh"
        wrapper.write_text('#!/bin/sh\n./run.sh\nresult=$?\n' +
                           ('if [ "$result" -eq 0 ]; then touch .lens-completed; fi\n' if r.get("ephemeral", True) else '') +
                           'exit "$result"\n')
        wrapper.chmod(0o755)
        unit("lens-runner.service", "[Unit]\nAfter=network-online.target\nWants=network-online.target\nStartLimitIntervalSec=300\nStartLimitBurst=3\nConditionPathExists=!/opt/actions-runner/.lens-completed\n"
             "[Service]\nUser=lens-runner\nWorkingDirectory=/opt/actions-runner\nExecStart=/opt/actions-runner/lens-runner.sh\nRestart=on-failure\nRestartSec=15\nKillMode=process\nTimeoutStopSec=infinity\nStandardOutput=null\nStandardError=null\n[Install]\nWantedBy=multi-user.target\n")
    elif m["kind"] != "appliance":
        raise RuntimeError("unsupported workload")
    Path("/run/lens/applied.json").write_text(json.dumps({k: m[k] for k in ("release", "configuration_revision", "secret_generations")}))


def status():
    m = manifest()
    if m["kind"] == "github-runner":
        # Inspect executable names, not command-line text or secret-bearing environments.
        busy = False
        listener = False
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit():
                continue
            try:
                executable = (proc / "comm").read_text().strip()
                busy = busy or executable == "Runner.Worker"
                listener = listener or executable == "Runner.Listener"
            except OSError:
                pass
        active = subprocess.run(["/usr/bin/systemctl", "is-active", "--quiet", "lens-runner.service"]).returncode == 0
        completed = (Path("/opt/actions-runner") / ".lens-completed").exists()
        return {"healthy": active and listener and not completed, "busy": busy, "accepting_jobs": active or listener, "completed": completed}
    if m["kind"] == "oci":
        return {"healthy": all(subprocess.run(["/usr/bin/systemctl", "is-active", "--quiet", "lens-" + c["name"] + ".service"]).returncode == 0 for c in m["containers"])}
    return {"healthy": True}


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["apply"]:
            apply()
            print('{"applied":true}')
        elif sys.argv[1:] == ["status"]:
            print(json.dumps(status()))
        else:
            raise RuntimeError("invalid operation")
    except Exception:
        # Do not transmit subprocess output, tokens or exception values back to the host.
        print('{"error":"guest workload operation failed"}')
        sys.exit(1)
