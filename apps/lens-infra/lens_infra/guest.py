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
    if m.get("ssh_enabled"):
        rules += "tcp dport 22 accept\n"
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


def verified(artifact):
    path = Path(artifact["path"])
    with path.open("rb") as f:
        if hashlib.file_digest(f, "sha256").hexdigest() != artifact["sha256"]:
            raise RuntimeError("build artifact digest mismatch")
    return path


def apply_build_profile(profile):
    applied = {"name": profile["name"]}
    if profile.get("lens"):
        artifact = profile["lens"]
        package = verified(artifact)
        name = subprocess.check_output(["/usr/bin/dpkg-deb", "--field", str(package), "Package"], stderr=subprocess.DEVNULL).decode().strip()
        version = subprocess.check_output(["/usr/bin/dpkg-deb", "--field", str(package), "Version"], stderr=subprocess.DEVNULL).decode().strip()
        if name != "dataplicity-lens" or version != artifact["version"]:
            raise RuntimeError("unexpected Lens build package")
        installed = subprocess.run(["/usr/bin/dpkg-query", "-W", "-f=${Version}", "dataplicity-lens"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        if installed.returncode or installed.stdout.decode() != version:
            command(["/usr/bin/dpkg", "--install", str(package)])
        applied["lens_version"] = version
    if profile.get("logging"):
        artifact = profile["logging"]
        source = verified(artifact)
        target = Path("/etc/rsyslog.d/50-lens-managed.conf")
        previous = target.read_bytes() if target.exists() else None
        data = source.read_bytes()
        if data != previous:
            pending = target.with_suffix(".pending")
            pending.write_bytes(data)
            pending.chmod(0o600)
            os.replace(pending, target)
            try:
                command(["/usr/sbin/rsyslogd", "-N1"])
            except Exception:
                if previous is None:
                    target.unlink()
                else:
                    target.write_bytes(previous)
                raise
            command(["/usr/bin/systemctl", "restart", "rsyslog.service"])
        applied["logging_sha256"] = artifact["sha256"]
    if profile.get('files') or profile.get('run_once'):
        deadline = 60 + sum(item.get('timeout_seconds', 300) + 5 for item in profile.get('run_once', []))
        result = subprocess.run(['/usr/bin/python3', '-I', '/opt/lens/bootstrap.py'], capture_output=True, timeout=deadline)
        if result.returncode:
            raise RuntimeError('build customization failed; inspect guest bootstrap state')
        applied['bootstrap'] = json.loads(result.stdout)
    return applied


def install_server_shell(profile, metadata):
    """Install only in the selected guest, preserving later user customization."""
    import pwd
    import shutil
    import stat
    context = Path("/etc/dataplicity")
    context.mkdir(mode=0o755, exist_ok=True)
    for source, destination, mode in (("/opt/lens/server-shell.bash", context / "server-shell.bash", 0o644),
                                      ("/opt/lens/server_status.py", Path("/usr/local/bin/dp-status"), 0o755)):
        pending = destination.with_suffix(".lens-pending")
        shutil.copyfile(source, pending)
        pending.chmod(mode)
        os.replace(pending, destination)
    values = {"environment": metadata["environment"], "vm": metadata["service"], "identity": metadata["instance_id"],
              "configuration_revision": metadata["instance_configuration_revision"]}
    # This file is parsed as data by Bash. It never contains executable input.
    pending = context / "server-context.pending"
    pending.write_text("".join(str(key) + "=" + str(value) + "\n" for key, value in values.items()))
    pending.chmod(0o644)
    os.replace(pending, context / "server-context")
    state = Path("/var/lib/lens/server-shell.json")
    state.parent.mkdir(mode=0o700, exist_ok=True)
    installed = json.loads(state.read_text()) if state.exists() else []
    users = [u["name"] for u in profile.get("users", [])]
    if profile.get("ssh_keys"):
        users.append("vmadmin")
    line = '\n# Dataplicity server shell: set DP_SHELL=off above this line to opt out.\n[ ! -r /etc/dataplicity/server-shell.bash ] || . /etc/dataplicity/server-shell.bash\n'
    for name in ["root", *users]:
        if name in installed:
            continue
        user = pwd.getpwnam(name)
        home = Path("/root" if name == "root" else "/home/" + name)
        if Path(user.pw_dir) != home or home.is_symlink():
            raise RuntimeError("unexpected home directory for server shell account")
        fd = os.open(home / ".bashrc", os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o644)
        with os.fdopen(fd, "a+") as f:
            info = os.fstat(f.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, user.pw_uid):
                raise RuntimeError("unexpected shell startup file ownership")
            f.seek(0)
            startup = f.read(2 * 1024 * 1024 + 1)
            if len(startup) > 2 * 1024 * 1024:
                raise RuntimeError("shell startup file exceeds size limit")
            if line not in startup:
                f.write(line)
            f.flush()
            os.fchown(f.fileno(), user.pw_uid, user.pw_gid)
            os.fsync(f.fileno())
        installed.append(name)
        pending = state.with_suffix(".pending")
        with pending.open("w") as f:
            os.fchmod(f.fileno(), 0o600)
            f.write(json.dumps(installed))
            f.flush()
            os.fsync(f.fileno())
        os.replace(pending, state)


def apply():
    m = manifest()
    firewall(m)
    profile = apply_build_profile(m["build_profile"]) if m.get("build_profile") else None
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
    applied = {k: m[k] for k in ("release", "configuration_revision", "secret_generations")}
    if profile:
        applied["build_profile"] = profile
    for key in ("host_id", "environment", "network", "class", "instance_configuration_revision", "instance", "instance_id", "service"):
        if key in m:
            applied[key] = m[key]
    Path("/run/lens/applied.json").write_text(json.dumps(applied))
    Path("/etc/lens-instance.json").write_text(json.dumps(applied))
    if m.get("build_profile", {}).get("shell"):
        install_server_shell(m["build_profile"], applied)


def metrics():
    import math
    m = manifest()
    ticks = [int(x) for x in Path('/proc/stat').read_text().splitlines()[0].split()[1:9]]
    memory = {line.split(':', 1)[0]: int(line.split()[1]) * 1024 for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith(('MemTotal:', 'MemAvailable:'))}
    total = sum(ticks)
    idle = ticks[3] + ticks[4]
    samples = {'vm': {'cpu_ticks': total - idle, 'total_ticks': total,
                      'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                      'memory_percent': 100 * (memory['MemTotal'] - memory['MemAvailable']) / memory['MemTotal']}}
    if m['kind'] == 'oci':
        # Reuse Lens's container/runtime collection. Project only resource data;
        # no environment, command output, credentials or logs are returned.
        try:
            result = subprocess.run(['/usr/bin/lens-containers', '--json', '--fields', 'containers'], capture_output=True, timeout=15)
            if result.returncode:
                raise RuntimeError('container metrics unavailable')
            data = json.loads(result.stdout)
            rows = {r['name']: r for r in data.get('containers', []) if r.get('runtime') == 'docker'}
            for container in m['containers']:
                row = rows.get('lens-' + container['name'], {})
                resources = row.get('resources') or {}
                values = {}
                if row.get('state') != 'running':
                    samples[container['name']] = {'reason': 'container_not_running_or_not_reported'}
                    continue
                def size(value):
                    if not isinstance(value, str):
                        return None
                    units = {'B': 1, 'KB': 1000, 'MB': 1000**2, 'GB': 1000**3, 'KiB': 1024, 'MiB': 1024**2, 'GiB': 1024**3}
                    parts = value.split()
                    if len(parts) != 2 or parts[1] not in units:
                        return None
                    try:
                        return float(parts[0]) * units[parts[1]]
                    except ValueError:
                        return None
                try:
                    cpu = float(resources.get('cpu_usage_percent', '').rstrip('%'))
                    budget = os.cpu_count() or 1
                    limit = resources.get('cpu_limit', '') or ''
                    if limit.endswith(' vCPUs'):
                        budget = float(limit.split()[0])
                    if math.isfinite(cpu) and cpu >= 0 and budget > 0:
                        values['cpu_percent'] = cpu / budget
                except (ValueError, AttributeError):
                    pass
                used = resources.get('memory_usage_bytes')
                limit = size(resources.get('memory_limit')) or memory['MemTotal']
                if isinstance(used, int) and used >= 0 and limit > 0:
                    values['memory_percent'] = used / limit * 100
                samples[container['name']] = values
        except Exception:
            for container in m['containers']:
                samples[container['name']] = {'reason': 'container_metrics_unavailable'}
    return {'samples': samples}


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
        elif sys.argv[1:] == ["metrics"]:
            print(json.dumps(metrics()))
        else:
            raise RuntimeError("invalid operation")
    except Exception:
        # Do not transmit subprocess output, tokens or exception values back to the host.
        print('{"error":"guest workload operation failed"}')
        sys.exit(1)
