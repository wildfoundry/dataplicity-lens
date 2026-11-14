#!/usr/bin/env python3
"""Real nftables isolation tests. Refuses to run in the host network namespace.

Run: sudo unshare --net env PYTHONPATH=apps/lens-infra python3 scripts/test-infra-network-linux.py
Creates no containers, persistent network namespaces, host bridges or host firewall rules.
"""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

from lens_infra.network import apply_firewall, verify_firewall
from lens_infra.guest import firewall_rules


def call(*argv, data=None):
    return subprocess.run(argv, check=True, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10).stdout


def client(pid, address, port):
    code = "import socket,sys; s=socket.socket(socket.AF_INET6 if ':' in sys.argv[1] else socket.AF_INET); s.settimeout(1.5); sys.exit(s.connect_ex((sys.argv[1],int(sys.argv[2]))))"
    result = subprocess.run(["/usr/bin/nsenter", "-t", str(pid), "-n", "/usr/bin/python3", "-c", code, address, str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3)
    return result.returncode == 0


def main():
    if sys.platform != "linux" or os.geteuid() != 0:
        raise SystemExit("requires root inside a disposable Linux network namespace")
    if Path("/proc/self/ns/net").stat().st_ino == Path("/proc/1/ns/net").stat().st_ino:
        raise SystemExit("REFUSED: this is the host network namespace; use unshare --net")
    children = []
    listeners = []
    try:
        call("/usr/sbin/ip", "link", "set", "lo", "up")
        call("/usr/sbin/sysctl", "-q", "-w", "net.ipv4.ip_forward=1")
        call("/usr/sbin/sysctl", "-q", "-w", "net.ipv6.conf.all.forwarding=1")
        call("/usr/sbin/sysctl", "-q", "-w", "net.ipv6.conf.default.forwarding=1")
        for bridge, subnet, ipv6 in [("liqa", "10.253.240.1/24", "fd00:1234:1::1/64"),
                                     ("liqb", "10.253.241.1/24", "fd00:1234:2::1/64"),
                                     ("outside", "172.31.254.1/24", None)]:
            call("/usr/sbin/ip", "link", "add", bridge, "type", "bridge")
            call("/usr/sbin/ip", "address", "add", subnet, "dev", bridge)
            if ipv6:
                call("/usr/sbin/ip", "-6", "address", "add", ipv6, "dev", bridge, "nodad")
            call("/usr/sbin/ip", "link", "set", bridge, "up")
        guests = {}
        for ident, bridge, address, gateway, ipv6, gateway6 in [
            ("a", "liqa", "10.253.240.2/24", "10.253.240.1", "fd00:1234:1::2/64", "fd00:1234:1::1"),
            ("b", "liqb", "10.253.241.2/24", "10.253.241.1", "fd00:1234:2::2/64", "fd00:1234:2::1"),
            ("c", "liqa", "10.253.240.3/24", "10.253.240.1", None, None),
            ("d", "outside", "172.31.254.2/24", "172.31.254.1", None, None)]:
            proc = subprocess.Popen(["/usr/bin/unshare", "--net", "/usr/bin/sleep", "120"])
            children.append(proc)
            parent_ino = Path("/proc/self/ns/net").stat().st_ino
            deadline = time.monotonic() + 3
            while Path(f"/proc/{proc.pid}/ns/net").stat().st_ino == parent_ino and time.monotonic() < deadline:
                time.sleep(.01)
            host, peer = "vnetq" + ident, "guest" + ident
            call("/usr/sbin/ip", "link", "add", host, "type", "veth", "peer", "name", peer)
            call("/usr/sbin/ip", "link", "set", host, "master", bridge)
            call("/usr/sbin/ip", "link", "set", host, "up")
            call("/usr/sbin/ip", "link", "set", peer, "netns", str(proc.pid))
            prefix = ["/usr/bin/nsenter", "-t", str(proc.pid), "-n", "/usr/sbin/ip"]
            call(*prefix, "link", "set", "lo", "up")
            call(*prefix, "address", "add", address, "dev", peer)
            call(*prefix, "link", "set", peer, "up")
            call(*prefix, "route", "add", "default", "via", gateway)
            if ipv6:
                call(*prefix, "-6", "address", "add", ipv6, "dev", peer, "nodad")
                call(*prefix, "-6", "route", "add", "default", "via", gateway6)
            guests[ident] = proc.pid
            # The listener process enters only its disposable guest network namespace.
            server = "import socket,threading,time;\n" \
                     "def listen(family,address):\n s=socket.socket(family); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind((address,18293)); s.listen();\n while True:\n  c,_=s.accept(); c.close()\n" \
                     f"threading.Thread(target=listen,args=(socket.AF_INET,{address.split('/')[0]!r}),daemon=True).start();\n"
            if ipv6:
                server += f"threading.Thread(target=listen,args=(socket.AF_INET6,{ipv6.split('/')[0]!r}),daemon=True).start();\n"
            server += "time.sleep(120)\n"
            children.append(subprocess.Popen(["/usr/bin/nsenter", "-t", str(proc.pid), "-n", "/usr/bin/python3", "-c", server]))
            if ident == "a":
                children.append(subprocess.Popen(["/usr/bin/nsenter", "-t", str(proc.pid), "-n", "/usr/bin/python3", "-c", server.replace("18293", "18294")]))
        host = socket.create_server(("10.253.240.1", 18293))
        listeners.append(host)
        def accept_host():
            while True:
                try:
                    c, _ = host.accept()
                    c.close()
                except OSError:
                    return
        threading.Thread(target=accept_host, daemon=True).start()
        time.sleep(1.2)
        assert client(guests["a"], "10.253.241.2", 18293), "baseline cross-network IPv4 is not connected"
        if not client(guests["a"], "fd00:1234:2::2", 18293):
            print("fixture IPv6 parent routes:", call("/usr/sbin/ip", "-6", "route", "show", "table", "all").decode(), file=sys.stderr)
            for ident in ("a", "b"):
                print("fixture IPv6", ident, "routes:", call("/usr/bin/nsenter", "-t", str(guests[ident]), "-n", "/usr/sbin/ip", "-6", "route", "show", "table", "all").decode(), file=sys.stderr)
                print("fixture IPv6", ident, "neighbors:", call("/usr/bin/nsenter", "-t", str(guests[ident]), "-n", "/usr/sbin/ip", "-6", "neighbor").decode(), file=sys.stderr)
            print("fixture parent neighbors:", call("/usr/sbin/ip", "-6", "neighbor").decode(), file=sys.stderr)
            print("fixture parent forwarding:", call("/usr/sbin/sysctl", "net.ipv6.conf.all.forwarding", "net.ipv6.conf.liqa.forwarding", "net.ipv6.conf.liqb.forwarding").decode(), file=sys.stderr)
            raise AssertionError("baseline cross-network IPv6 is not connected")
        assert client(guests["a"], "10.253.240.3", 18293), "baseline same-network traffic is not connected"
        assert client(guests["a"], "10.253.240.1", 18293), "baseline host access is not connected"
        assert client(guests["a"], "172.31.254.2", 18293), "baseline outside network is not connected"
        call("/usr/sbin/nft", "add", "table", "inet", "existing_fixture")
        before = call("/usr/sbin/nft", "-j", "list", "table", "inet", "existing_fixture")
        policy = {"host_id": "namespace-qualification", "networks": {
            "a": {"environment": "production", "bridge": "liqa", "cidr": "10.253.240.0/24", "egress": ["0.0.0.0/0"]},
            "b": {"environment": "staging", "bridge": "liqb", "cidr": "10.253.241.0/24"}}}
        apply_firewall(policy)
        verify_firewall(policy)
        assert not client(guests["a"], "10.253.241.2", 18293), "cross-environment IPv4 leaked"
        assert not client(guests["a"], "fd00:1234:2::2", 18293), "cross-environment IPv6 leaked"
        assert not client(guests["a"], "10.253.240.1", 18293), "host management remained reachable"
        assert client(guests["a"], "10.253.240.3", 18293), "same-environment traffic was broken"
        assert client(guests["a"], "172.31.254.2", 18293), "approved egress was broken"
        policy["networks"]["a"]["egress"] = []
        apply_firewall(policy)
        verify_firewall(policy)
        assert not client(guests["a"], "172.31.254.2", 18293), "default-deny egress leaked"
        after = call("/usr/sbin/nft", "-j", "list", "table", "inet", "existing_fixture")
        assert before == after, "an unrelated firewall table changed"
        assert client(guests["c"], "10.253.240.2", 18294), "baseline application port is unreachable"
        call("/usr/bin/nsenter", "-t", str(guests["a"]), "-n", "/usr/sbin/nft", "-f", "-", data=firewall_rules({"allowed_port": 18293}).encode())
        assert client(guests["c"], "10.253.240.2", 18293), "declared guest service port was broken"
        assert not client(guests["c"], "10.253.240.2", 18294), "application could bypass its declared shim port"
        print(json.dumps({"qualified": True, "namespace": "isolated", "checks": ["baseline-connectivity", "cross-environment-ipv4", "cross-environment-ipv6", "host-management", "same-environment", "approved-egress", "default-deny-egress", "unrelated-firewall-preserved", "declared-guest-port", "shim-bypass-denied"]}))
    finally:
        for listener in listeners:
            listener.close()
        for child in reversed(children):
            child.terminate()
        for child in children:
            child.wait(timeout=5)


if __name__ == "__main__":
    main()
