"""Generate bounded HAProxy configurations; maintain disk and runtime generations."""

from __future__ import annotations

import json
import hashlib
import os
import socket
import time
from pathlib import Path

from .backend import run
from .policy import Rejected, require


def render(request, template, backends, socket_path, certificate=None):
    profile = template.get("profile", "private")
    require(profile != "private", "private workloads have no ingress")
    long_lived = profile == "websocket"
    lines = ["global", f"  stats socket {socket_path} mode 600 level admin", "  maxconn 8192",
             f"  hard-stop-after {max(1, template.get('drain_seconds', 30))}s",
             "defaults", "  mode http", "  timeout connect 5s", "  timeout client 30s", "  timeout server 30s",
             "  timeout http-request 10s", "  timeout queue 5s", "  retries 0"]
    if long_lived:
        lines += ["  timeout tunnel 1h", "  timeout client-fin 30s", "  timeout server-fin 30s"]
    port = request.get("listen_port", 8080)
    bind = f"  bind 127.0.0.1:{port}"
    if certificate:
        require(" " not in certificate and "\n" not in certificate, "invalid certificate path")
        bind += f" ssl crt {certificate}"
    lines += ["frontend service", bind, "  default_backend instances", "backend instances",
              "  balance leastconn" if long_lived else "  balance roundrobin",
              "  option httpchk", f"  http-check send meth GET uri {template.get('health_path', '/health')} ver HTTP/1.1 hdr Host localhost",
              "  http-check expect status 200"]
    for index, backend in enumerate(backends):
        address, service_port = backend["address"], template["port"]
        # Addresses are parsed by the engine; never interpolate caller-supplied proxy directives.
        import ipaddress
        ipaddress.ip_address(address)
        weight = backend.get("weight", 100)
        require(type(weight) is int and 1 <= weight <= 25600, "invalid backend weight")
        lines.append(f"  server b{index} {address}:{service_port} weight {weight} check inter 2s fall 3 rise 2")
    return "\n".join(lines) + "\n"


class Ingress:
    """A local process per service for the first slice; no public management listener."""

    def __init__(self, directory, binary="/usr/sbin/haproxy", supervised=True):
        self.root = Path(directory) / "ingress"
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.binary = binary
        self.supervised = supervised

    def paths(self, service):
        # Hyphen concatenation is ambiguous across environment/service boundaries.
        directory = self.root / ("s-" + hashlib.sha256(service.encode()).hexdigest()[:24])
        directory.mkdir(mode=0o700, exist_ok=True)
        return directory, directory / "haproxy.cfg", directory / "haproxy.pid", directory / "admin.sock"

    def publish(self, request, template, backends, certificate=None):
        service = request["environment"] + "/" + request["service"]
        directory, config, pidfile, control = self.paths(service)
        data = render(request, template, backends, control, certificate)
        pending = directory / "pending.cfg"
        pending.write_text(data)
        os.chmod(pending, 0o600)
        run([self.binary, "-c", "-f", str(pending)])
        previous = config.read_bytes() if config.exists() else None
        old_pid = None
        if pidfile.exists():
            value = pidfile.read_text().strip()
            require(value.isdigit(), "invalid ingress PID record")
            # Never signal a recycled PID based only on a stale pidfile.
            proc = Path("/proc") / value
            if proc.exists():
                require((proc / "comm").read_text().strip() == "haproxy", "ingress PID belongs to another process")
                arguments = (proc / "cmdline").read_bytes().split(b"\0")
                require(str(config).encode() in arguments, "ingress process identity mismatch")
                old_pid = value
        retired_socket = None
        if old_pid and control.exists():
            retired_socket = directory / (old_pid + ".retired.sock")
            os.replace(control, retired_socket)
        os.replace(pending, config)
        args = [self.binary, "-D", "-f", str(config), "-p", str(pidfile)]
        if old_pid:
            args += ["-sf", old_pid]
        try:
            if self.supervised:
                require(self.root.parent == Path("/var/lib/lens-infra"), "supervised ingress requires the installed state directory")
                unit = "lens-ingress@" + directory.name + ".service"
                active = run(["/usr/bin/systemctl", "is-active", "--quiet", unit], check=False) is not None
                run(["/usr/bin/systemctl", "reload" if active else "start", unit])
            else:
                run(args)
        except Rejected:
            if previous is not None:
                config.write_bytes(previous)
            if retired_socket and retired_socket.exists() and not control.exists():
                os.replace(retired_socket, control)
            raise
        state = directory / "generation.json"
        tmp = directory / "generation.pending"
        with tmp.open("w") as f:
            json.dump({"request": request, "backends": backends, "certificate": certificate}, f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, state)
        return {"service": service, "endpoint": f"{'https' if certificate else 'http'}://127.0.0.1:{request.get('listen_port', 8080)}"}

    def connections(self, service, retired=False):
        directory, _, _, path = self.paths(service)
        if retired:
            return sum(self._connections(p) for p in directory.glob("*.retired.sock"))
        return self._connections(path)

    def _connections(self, path):
        if not path.exists():
            return 0
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(2)
            try:
                sock.connect(str(path))
            except (FileNotFoundError, ConnectionRefusedError):
                if path.name.endswith(".retired.sock"):
                    pid = path.name.split(".", 1)[0]
                    proc = Path("/proc") / pid
                    try:
                        # Soft-stopped HAProxy workers may close their admin listener while
                        # existing tunnels stay alive. Unknown means keep draining, not zero.
                        if (proc / "comm").read_text().strip() == "haproxy":
                            return 1
                    except FileNotFoundError:
                        pass
                return 0
            sock.sendall(b"show stat\n")
            sock.shutdown(socket.SHUT_WR)
            data = bytearray()
            while len(data) < 1024 * 1024:
                b = sock.recv(65536)
                if not b:
                    break
                data.extend(b)
        lines = data.decode().splitlines()
        if not lines:
            return 0
        headers = lines[0].lstrip("# ").split(",")
        count = 0
        for line in lines[1:]:
            values = dict(zip(headers, line.split(",")))
            if values.get("pxname") == "instances" and values.get("svname", "").startswith("b"):
                count += int(values.get("scur") or 0)
        return count

    def restore(self, policies):
        restored = []
        for state in self.root.glob("*/generation.json"):
            data = json.loads(state.read_text())
            req = data["request"]
            template = policies["templates"][req["template"]]
            restored.append(self.publish(req, template, data["backends"], data.get("certificate")))
        return restored
