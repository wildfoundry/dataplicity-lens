#!/usr/bin/env python3
"""Real VXLAN packets in disposable host/guest network namespaces only."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

from lens_infra.network import bridge_name
from lens_infra.overlay import alias, vxlan_name


def command(*argv, data=None):
    result = subprocess.run(argv, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
    if result.returncode:
        raise RuntimeError("isolated fixture command failed: " + result.stderr.decode())
    return result.stdout


def enter(pid, *argv):
    return command("/usr/bin/nsenter", "-t", str(pid), "-n", *argv)


def probe(pid, address, udp=False):
    code = 'import socket,sys; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM if sys.argv[2]=="udp" else socket.SOCK_STREAM);s.settimeout(1.5)\n'
    if udp:
        code += 's.bind(("",67));s.sendto(b"probe",(sys.argv[1],68));assert s.recv(16)==b"probe"\n'
    else:
        code += 's.connect((sys.argv[1],18299));s.sendall(b"lens");assert s.recv(4)==b"lens"\n'
    return subprocess.run(["/usr/bin/nsenter", "-t", str(pid), "-n", "/usr/bin/python3", "-I", "-c", code, address, "udp" if udp else "tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3).returncode == 0


def main():
    if sys.platform != "linux" or os.geteuid() != 0:
        raise SystemExit("requires root inside a disposable network namespace")
    if Path("/proc/self/ns/net").stat().st_ino == Path("/proc/1/ns/net").stat().st_ino:
        raise SystemExit("REFUSED: do not run in the host network namespace")
    children = []
    package = str(Path(__file__).resolve().parents[1] / "apps/lens-infra")
    def namespace():
        proc = subprocess.Popen(["/usr/bin/unshare", "--net", "/usr/bin/sleep", "180"])
        children.append(proc)
        original = Path("/proc/self/ns/net").stat().st_ino
        deadline = time.monotonic() + 3
        while Path(f"/proc/{proc.pid}/ns/net").stat().st_ino == original:
            if time.monotonic() > deadline:
                raise RuntimeError("private namespace did not start")
            time.sleep(.01)
        enter(proc.pid, "/usr/sbin/ip", "link", "set", "lo", "up")
        return proc.pid
    def python(pid, code, argument=None):
        argv = ["/usr/bin/python3", "-c", "import sys;sys.path.insert(0," + repr(package) + ");" + code]
        if argument is not None:
            argv.append(json.dumps(argument))
        return enter(pid, *argv)
    def server(pid, address, udp=False):
        code = 'import socket\ns=socket.socket(socket.AF_INET,' + ('socket.SOCK_DGRAM' if udp else 'socket.SOCK_STREAM') + f');s.bind(({address!r},{68 if udp else 18299}))\n'
        code += 'while True:\n b,a=s.recvfrom(64);s.sendto(b,a)\n' if udp else 's.listen()\nwhile True:\n c,a=s.accept();b=c.recv(4);c.sendall(b);c.close()\n'
        children.append(subprocess.Popen(["/usr/bin/nsenter", "-t", str(pid), "-n", "/usr/bin/python3", "-I", "-c", code]))
    try:
        command("/usr/sbin/ip", "link", "add", "fabric", "type", "bridge")
        command("/usr/sbin/ip", "link", "set", "fabric", "up")
        hosts = {}
        for label, address in (("a", "192.0.2.10/24"), ("b", "192.0.2.20/24"), ("rogue", "192.0.2.30/24")):
            pid = hosts[label] = namespace()
            outer, inner = "outer" + label, "inner" + label
            command("/usr/sbin/ip", "link", "add", outer, "type", "veth", "peer", "name", inner)
            command("/usr/sbin/ip", "link", "set", outer, "master", "fabric")
            command("/usr/sbin/ip", "link", "set", outer, "up")
            command("/usr/sbin/ip", "link", "set", inner, "netns", str(pid))
            enter(pid, "/usr/sbin/ip", "address", "add", address, "dev", inner)
            enter(pid, "/usr/sbin/ip", "link", "set", inner, "up")
            enter(pid, "/usr/sbin/sysctl", "-q", "-w", "net.ipv4.ip_forward=1")
        fabric = {"name": "fixture-fabric", "gateway_host": "host-a", "mtu": 1350, "protected_cidrs": ["192.0.2.0/24"],
                  "members": [{"host_id": "host-a", "endpoint": "192.0.2.10", "node_id": 1}, {"host_id": "host-b", "endpoint": "192.0.2.20", "node_id": 2}]}
        nets = {"production": {"environment": "production", "libvirt_network": "li-overlay-production", "cidr": "10.253.240.0/24", "mode": "vxlan", "managed": True, "vni": 24001, "egress": ["0.0.0.0/0"]},
                "staging": {"environment": "staging", "libvirt_network": "li-overlay-staging", "cidr": "10.253.241.0/24", "mode": "vxlan", "managed": True, "vni": 24002}}
        policies = {}
        for label in ("a", "b"):
            p = policies[label] = {"host_id": "host-" + label, "fabric": fabric, "networks": nets}
            python(hosts[label], "import json;from lens_infra.overlay import preflight;preflight(json.loads(sys.argv[1]))", p)
            for key, net in nets.items():
                if label == "a":
                    bridge = bridge_name(net)
                    enter(hosts[label], "/usr/sbin/ip", "link", "add", bridge, "type", "bridge")
                    gateway = net["cidr"].replace("0/24", "1/24")
                    enter(hosts[label], "/usr/sbin/ip", "address", "add", gateway, "dev", bridge)
                python(hosts[label], f"import json;from lens_infra.overlay import ensure,verify;p=json.loads(sys.argv[1]);ensure(p,{key!r},primary_verified=True);verify(p,{key!r})", p)
        guests = {}
        for label, host, key, address in (("ap", "a", "production", "10.253.240.10/24"), ("bp", "b", "production", "10.253.240.20/24"),
                                           ("as", "a", "staging", "10.253.241.10/24"), ("bs", "b", "staging", "10.253.241.20/24")):
            pid = guests[label] = namespace()
            outer, inner = "vnet" + label, "guest" + label
            enter(hosts[host], "/usr/sbin/ip", "link", "add", outer, "type", "veth", "peer", "name", inner)
            enter(hosts[host], "/usr/sbin/ip", "link", "set", outer, "mtu", "1350", "master", bridge_name(nets[key]), "up")
            enter(hosts[host], "/usr/sbin/ip", "link", "set", inner, "netns", str(pid))
            enter(pid, "/usr/sbin/ip", "address", "add", address, "dev", inner)
            enter(pid, "/usr/sbin/ip", "link", "set", inner, "mtu", "1350", "up")
            enter(pid, "/usr/sbin/ip", "route", "add", "default", "via", nets[key]["cidr"].replace("0/24", "1"))
            server(pid, address.split("/")[0])
        server(guests["bp"], "10.253.240.20", udp=True)
        server(hosts["b"], "192.0.2.20")
        # An unapproved physical peer can use the same VNI unless transport ACLs stop it.
        rogue = hosts["rogue"]
        enter(rogue, "/usr/sbin/ip", "link", "add", "roguebr", "type", "bridge")
        enter(rogue, "/usr/sbin/ip", "address", "add", "10.253.240.30/24", "dev", "roguebr")
        enter(rogue, "/usr/sbin/ip", "link", "set", "roguebr", "mtu", "1350", "up")
        enter(rogue, "/usr/sbin/ip", "link", "add", "roguevx", "type", "vxlan", "id", "24001", "local", "192.0.2.30", "dstport", "4789")
        enter(rogue, "/usr/sbin/ip", "link", "set", "roguevx", "mtu", "1350", "master", "roguebr", "up")
        enter(rogue, "/usr/sbin/bridge", "fdb", "append", "00:00:00:00:00:00", "dev", "roguevx", "dst", "192.0.2.10")
        time.sleep(1)
        assert probe(guests["ap"], "10.253.240.20"), "baseline VXLAN did not carry same-network traffic"
        assert probe(guests["ap"], "10.253.241.20"), "baseline routed cross-environment fixture is not connected"
        assert probe(guests["ap"], "192.0.2.20"), "baseline management route is not connected"
        assert probe(hosts["rogue"], "10.253.240.10"), "baseline rogue VNI fixture is not connected"
        assert probe(guests["ap"], "10.253.240.20", udp=True), "baseline DHCP-spoof fixture is not connected"
        for label in ("a", "b"):
            python(hosts[label], "import json;from lens_infra.network import apply_firewall,verify_firewall;p=json.loads(sys.argv[1]);apply_firewall(p);verify_firewall(p)", policies[label])
        assert probe(guests["ap"], "10.253.240.20"), "same-environment cross-host traffic failed"
        def mtu_probe(size):
            return subprocess.run(["/usr/bin/nsenter", "-t", str(guests["ap"]), "-n", "/usr/bin/ping", "-4", "-c", "1", "-W", "2", "-M", "do", "-s", str(size), "10.253.240.20"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=4).returncode == 0
        assert mtu_probe(1322), "MTU-sized nonfragmenting VXLAN packet failed"
        assert not mtu_probe(1323), "guest could exceed its declared overlay MTU"
        assert probe(guests["as"], "10.253.241.20"), "second VNI did not remain usable"
        assert not probe(guests["ap"], "10.253.241.20"), "cross-environment traffic leaked"
        assert not probe(guests["ap"], "192.0.2.20"), "guest could reach the physical management fabric"
        assert not probe(hosts["rogue"], "10.253.240.10"), "unapproved VTEP injected private traffic"
        assert not probe(guests["ap"], "10.253.240.20", udp=True), "guest DHCP server spoof reached a peer"
        print(json.dumps({"qualified": True, "scope": "private-network-namespaces", "checks": ["real-cross-host-VXLAN", "two-independent-VNIs", "MTU-1350", "cross-environment-denied", "management-fabric-denied", "unapproved-VTEP-denied", "guest-DHCP-spoof-denied"]}))
    finally:
        for child in reversed(children):
            child.terminate()
        for child in children:
            child.wait(timeout=5)


if __name__ == "__main__":
    main()
