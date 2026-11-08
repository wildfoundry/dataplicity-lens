"""Narrow libvirt adapter. Existing VMs/containers are always inventory-only."""

from __future__ import annotations

import base64
import hashlib
import ipaddress
import json
import io
import math
import os
import shutil
import signal
import shlex
import subprocess
import tempfile
import time
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path

from .policy import Rejected, require

NS = "https://lens.dataplicity.com/infra/1"
ET.register_namespace("lens", NS)


def run(args, *, data=None, timeout=30, check=True):
    """Never invoke a shell or return raw subprocess failures to deployment callers."""
    env = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C", "HOME": "/root"}
    p = subprocess.Popen([str(v) for v in args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, env=env, start_new_session=True)
    try:
        stdout, _stderr = p.communicate(data, timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.communicate()
        raise Rejected(f"{Path(args[0]).name} operation exceeded its deadline") from None
    require(len(stdout) <= 16 * 1024 * 1024, "native command response exceeded limit")
    if check:
        require(p.returncode == 0, f"{Path(args[0]).name} operation failed (exit {p.returncode}); inspect locally")
    return stdout if p.returncode == 0 else None


def file_digest(path):
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    with os.fdopen(fd, "rb") as f:
        require(f.seekable(), "artifact must be a regular file")
        return hashlib.file_digest(f, "sha256").hexdigest()


def native_agent(uri, identity, payload):
    # The binding keeps credential data out of argv, virsh's interactive echo/history
    # and on-disk command files. Read-only inventory does not require this dependency.
    try:
        import libvirt
        import libvirt_qemu
    except ImportError:
        raise Rejected("VM execution requires the distribution's python3-libvirt package") from None
    libvirt.registerErrorHandler(lambda _context, _error: None, None)
    connection = None
    try:
        connection = libvirt.open(uri)
        domain = connection.lookupByUUIDString(identity)
        return libvirt_qemu.qemuAgentCommand(domain, payload, 10, 0)
    except Exception:
        raise Rejected("native guest agent request failed; inspect locally") from None
    finally:
        if connection is not None:
            connection.close()


def memory_mib(node):
    if node is None:
        return 0
    units = {"b": 1 / 1048576, "bytes": 1 / 1048576, "k": 1 / 1024, "kb": 1 / 1024,
             "kib": 1 / 1024, "m": 1, "mb": 1, "mib": 1, "g": 1024, "gb": 1024, "gib": 1024}
    return math.ceil(int(node.text) * units[node.get("unit", "KiB").lower()])


def parse_domain(xml, state="unknown"):
    x = ET.fromstring(xml)
    identity = x.findtext("uuid")
    require(identity is not None, "VM identity unavailable")
    metadata = x.find(f"metadata/{{{NS}}}instance")
    result = {"id": identity, "name": x.findtext("name"), "kind": "vm", "state": state.strip(),
              "vcpus": int(x.findtext("vcpu", "0")), "memory_mib": memory_mib(x.find("memory")),
              "disk_gib": 0, "detail": {"networks": [], "disks": []}}
    for nic in x.findall("devices/interface"):
        source = nic.find("source")
        result["detail"]["networks"].append(dict(source.attrib) if source is not None else {})
    for disk in x.findall("devices/disk"):
        source = disk.find("source")
        if disk.get("device") == "disk" and source is not None:
            result["detail"]["disks"].append(dict(source.attrib))
    if metadata is not None:
        result["operation_id"] = metadata.get("operation")
    return result


def mac_address(identity):
    # A locally administered unicast MAC follows the VM UUID across placement.
    value = hashlib.sha256(uuid.UUID(identity).bytes).digest()[:5]
    return "02:" + ":".join(f"{part:02x}" for part in value)


class Libvirt:
    def __init__(self, directory, uri="qemu:///system", storage_directory="/var/lib/libvirt/images/lens-infra"):
        require(uri == "qemu:///system" or uri.startswith("qemu+unix:///system?socket=/"), "only local libvirt connections are supported")
        self.root = Path(directory)
        self.storage = Path(storage_directory)
        self.uri = uri

    def virsh(self, *args, **kwargs):
        return run(["/usr/bin/virsh", "--connect", self.uri, *args], **kwargs)

    def inventory(self, policy=None):
        resources = []
        # UUIDs, not names, address live resources; never execute collected strings.
        ids = self.virsh("list", "--all", "--uuid").decode().split()
        require(len(ids) <= 4096, "inventory exceeds supported size")
        for identity in ids:
            uuid.UUID(identity)
            x = parse_domain(self.virsh("dumpxml", identity), self.virsh("domstate", identity).decode())
            for disk in x["detail"]["disks"]:
                path = disk.get("file", disk.get("dev"))
                if path:
                    info = self.virsh("domblkinfo", identity, path, check=False)
                    if info:
                        fields = dict(line.split(":", 1) for line in info.decode().splitlines() if ":" in line)
                        x["disk_gib"] += math.ceil(int(fields.get("Capacity", "0")) / 2**30)
                    else:
                        x["detail"]["capacity_unknown"] = True
            resources.append(x)
        # Runtime inventories deliberately exclude config, environment, labels and command lines.
        if shutil.which("lxc"):
            output = run([shutil.which("lxc"), "list", "--format=json"], check=False)
            if output:
                for item in json.loads(output):
                    typ = "lxd-vm" if item.get("type") == "virtual-machine" else "container"
                    resources.append({"id": "lxd:" + item["name"], "name": item["name"], "kind": typ,
                                      "state": item.get("status", "unknown"), "detail": {"runtime": "lxd"}})
        if shutil.which("docker"):
            output = run([shutil.which("docker"), "ps", "--all", "--format", '{{json .ID}}|{{json .Names}}|{{json .Image}}|{{json .State}}'], check=False)
            if output:
                for line in output.decode().splitlines():
                    ident, n, image, state = [json.loads(v) for v in line.split("|")]
                    resources.append({"id": "docker:" + ident, "name": n, "kind": "container", "state": state,
                                      "detail": {"runtime": "docker", "image": image}})
        from .runner_provider import canonical_inventory
        return canonical_inventory(policy, resources)

    def prepare(self, template, profile=None):
        image = self.artifact(template["image"], template["image_sha256"])
        info = json.loads(run(["/usr/bin/qemu-img", "info", "--output=json", "--backing-chain", str(image)]))
        require(len(info) == 1 and not info[0].get("backing-filename"), "approved VM images must be self-contained without backing files")
        require(info[0]["format"] == template.get("image_format", "qcow2"), "VM image format does not match policy")
        require(not info[0].get("format-specific", {}).get("data", {}).get("data-file"), "external image data files are not supported")
        if template["kind"] == "github-runner":
            r = template["runner"]
            self.artifact(r["archive"], r["sha256"])
        for container in template.get("containers", []):
            if container.get("archive"):
                self.artifact(container["archive"], container["archive_sha256"])
        for component in ("lens", "logging"):
            artifact = (profile or {}).get(component)
            if artifact:
                self.artifact(artifact["path"], artifact["sha256"])
        require(shutil.which("qemu-img") and shutil.which("cloud-localds"), "qemu-img and cloud-localds are required")
        return {"vm_image": "verified", "artifacts": "verified"}

    def artifact(self, source, expected):
        """Snapshot the exact validated bytes so mutable source paths cannot race use."""
        cache = self.root / "artifacts"
        cache.mkdir(parents=True, mode=0o700, exist_ok=True)
        destination = cache / expected
        if destination.exists():
            require(not destination.is_symlink() and file_digest(destination) == expected, "prepared artifact integrity check failed")
            return destination
        fd = os.open(source, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | os.O_NONBLOCK)
        import stat
        with os.fdopen(fd, "rb") as f, tempfile.NamedTemporaryFile(dir=cache, delete=False) as out:
            pending = Path(out.name)
            try:
                st = os.fstat(f.fileno())
                require(stat.S_ISREG(st.st_mode), "artifact must be a regular file")
                require(shutil.disk_usage(cache).free > st.st_size + 512 * 1024 * 1024, "insufficient space to prepare artifact safely")
                checksum = hashlib.sha256()
                while block := f.read(1024 * 1024):
                    out.write(block)
                    checksum.update(block)
                out.flush()
                os.fsync(out.fileno())
                require(checksum.hexdigest() == expected, "artifact digest mismatch")
                os.replace(pending, destination)
            finally:
                if pending.exists():
                    pending.unlink()
        return destination

    def verify_network(self, network, policy=None):
        data = self.virsh("net-dumpxml", network["libvirt_network"], check=False)
        require(data is not None, "network is absent; a local administrator must provision it first")
        x = ET.fromstring(data)
        from .network import bridge_name
        bridge = x.find("bridge")
        require(bridge is not None and bridge.get("name") == bridge_name(network), "network bridge does not match the approved binding")
        if network["mode"] == "isolated":
            require(x.find("forward") is None, "isolated network unexpectedly forwards traffic")
        elif network["mode"] == "nat":
            require(x.find("forward") is not None and x.find("forward").get("mode") == "nat", "NAT network mismatch")
        elif network["mode"] == "vlan":
            bridge = x.find("bridge")
            require(bridge is not None and bridge.get("name") == network["bridge"], "VLAN bridge mismatch")
            tag = x.find("vlan/tag")
            require(tag is not None and tag.get("id") == str(network["vlan"]), "VLAN tag mismatch")
        elif network["mode"] == "vxlan":
            require(policy and policy.get("fabric"), "VXLAN verification requires its fabric policy")
            forward = x.find("forward")
            gateway = policy["host_id"] == policy["fabric"]["gateway_host"]
            require(forward is not None and forward.get("mode") == ("nat" if gateway else "bridge"), "overlay gateway/bridge mode mismatch")
            mtu = x.find("mtu")
            require(mtu is not None and mtu.get("size") == str(policy["fabric"].get("mtu", 1350)), "overlay network MTU mismatch")
        subnet = ipaddress.ip_network(network["cidr"])
        if network["mode"] != "vlan" and not (network["mode"] == "vxlan" and policy["host_id"] != policy["fabric"]["gateway_host"]):
            ip = x.find("ip")
            require(ip is not None, "network address unavailable")
            prefix = ip.get("netmask", ip.get("prefix"))
            require(prefix is not None and ipaddress.ip_network(ip.get("address") + "/" + prefix, strict=False) == subnet, "network address or prefix mismatch")
        return {"network": network["libvirt_network"], "mode": network["mode"], "verified": True}

    def assert_isolation(self, policy):
        from .network import verify_firewall
        verify_firewall(policy)
        from .overlay import verify
        for key in policy["networks"]:
            verify(policy, key)

    def check_host_capacity(self, policy, resources):
        total = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemTotal:"))) // 1024
        available = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:"))) // 1024
        capacity = policy["capacity"]
        reserve = capacity.get("host_reserve_mib", 1024)
        require(capacity["memory_mib"] + reserve <= total, "configured guest budget leaves insufficient physical host memory reserve")
        require(capacity["vcpus"] <= (os.cpu_count() or 1) * capacity.get("cpu_overcommit_percent", 100) // 100, "configured vCPU budget exceeds the administrator's explicit CPU overcommit limit")
        require(sum(r["memory_mib"] for r in resources) + reserve <= available, "insufficient currently available memory for the approved build and host reserve")
        storage = self.storage if self.storage.exists() else self.storage.parent
        require(shutil.disk_usage(storage).free >= (sum(r["disk_gib"] for r in resources) + 2) * 2**30, "insufficient physical free storage including build overhead")
        pool = policy.get("migration_pool")
        if pool:
            capabilities = ET.fromstring(self.virsh("domcapabilities", "--virttype", "kvm", "--arch", "x86_64", "--machine", pool["machine"]))
            models = capabilities.findall("cpu/mode[@name='custom']/model")
            require(any(m.text == pool["cpu_model"] and m.get("usable") == "yes" for m in models), "host cannot provide the approved migration CPU model")

    def instance_directory(self, resource):
        directory = self.storage / resource["id"]
        for folder in (self.storage, directory):
            folder.mkdir(parents=True, mode=0o711, exist_ok=True)
            st = folder.lstat()
            require(folder.is_dir() and not folder.is_symlink() and st.st_uid == os.geteuid() and st.st_mode & 0o022 == 0,
                    "managed VM storage directory is not trusted")
            # mkdir's mode is filtered by UMask=0077 in the installed service.
            # QEMU needs traversal; individual disks/metadata keep private modes.
            os.chmod(folder, 0o711)
        return directory

    def create(self, resource, request, policy, start=True):
        # The caller's resource UUID was reserved transactionally before any VM exists.
        t = policy["templates"][request["template"]]
        net = policy["networks"][request["network"]]
        self.verify_network(net, policy)
        existing = self.virsh("dumpxml", resource["id"], check=False)
        if existing:
            self.assert_owned(resource)
            self.instance_directory(resource)
            if start:
                self.ensure_running(resource)
            return
        directory = self.instance_directory(resource)
        disk = directory / "root.qcow2"
        if not disk.exists():
            image = self.artifact(t["image"], t["image_sha256"])
            run(["/usr/bin/qemu-img", "convert", "-f", t.get("image_format", "qcow2"), "-O", "qcow2", str(image), str(disk)], timeout=300)
            run(["/usr/bin/qemu-img", "resize", str(disk), str(resource["disk_gib"]) + "G"])
            os.chmod(disk, 0o600)
        user_data = directory / "user-data"
        user_data.write_text("#cloud-config\n" + json.dumps({"preserve_hostname": False, "hostname": resource["name"],
            "ssh_pwauth": False, "disable_root": True,
            "write_files": [{"path": "/etc/lens-release.json", "permissions": "0644", "content": json.dumps({"release": request["release"], "configuration_revision": policy["revision"],
                "host_id": policy["host_id"], "environment": request["environment"], "network": request["network"], "class": request["class"], "template": request["template"], "build_profile": t.get("build_profile")})}]}))
        metadata = directory / "meta-data"
        metadata.write_text(json.dumps({"instance-id": resource["id"], "local-hostname": resource["name"]}))
        profile = policy.get("build_profiles", {}).get(t.get("build_profile"), {})
        users = profile.get("users", [])
        if profile.get("ssh_keys"):
            users = [{"name": "vmadmin", "ssh_keys": profile["ssh_keys"], "sudo": True}]
        if users:
            config = json.loads(user_data.read_text().split("\n", 1)[1])
            if not request.get("restore_backup"):
                config["groups"] = ["lensu-" + u["name"] for u in users]
            config["users"] = [{"name": u["name"], "primary_group": "lensu-" + u["name"], **({"groups": ["sudo"]} if u.get("sudo") else {}),
                                "homedir": "/home/" + u["name"], "shell": "/bin/bash", "lock_passwd": True,
                                "sudo": ["ALL=(ALL) NOPASSWD:ALL"] if u.get("sudo") else None, "ssh_authorized_keys": u["ssh_keys"]} for u in users]
            config["write_files"].append({"path": "/etc/ssh/sshd_config.d/10-lens-admin.conf", "permissions": "0644",
                                           "content": "PermitRootLogin no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"})
            config["runcmd"] = [["/usr/bin/chmod", "0700", "/home/" + u["name"]] for u in users]
            config["runcmd"].append(["/usr/bin/systemctl", "try-reload-or-restart", "ssh.service"])
            user_data.write_text("#cloud-config\n" + json.dumps(config))
        seed = directory / "seed.iso"
        mac = mac_address(resource["id"])
        network_data = directory / "network-data"
        network_data.write_text(json.dumps({"version": 2, "ethernets": {"lens-primary": {"match": {"macaddress": mac},
            "dhcp4": True, "dhcp6": False, "accept-ra": False, "dhcp-identifier": "mac", "mtu": policy.get("fabric", {}).get("mtu", 1350) if net["mode"] == "vxlan" else 1500}}}))
        run(["/usr/bin/cloud-localds", "--network-config", str(network_data), str(seed), str(user_data), str(metadata)])
        domain = ET.Element("domain", {"type": "kvm"})
        for key, value in [("name", resource["name"]), ("uuid", resource["id"])]:
            ET.SubElement(domain, key).text = value
        ET.SubElement(domain, "memory", {"unit": "MiB"}).text = str(resource["memory_mib"])
        ET.SubElement(domain, "vcpu").text = str(resource["vcpus"])
        md = ET.SubElement(domain, "metadata")
        ET.SubElement(md, f"{{{NS}}}instance", {"operation": request["operation_id"], "host": policy["host_id"], "environment": request["environment"], "service": request["service"]})
        os_node = ET.SubElement(domain, "os")
        ET.SubElement(os_node, "type", {"arch": "x86_64", "machine": policy.get("migration_pool", {}).get("machine", "q35")}).text = "hvm"
        ET.SubElement(os_node, "boot", {"dev": "hd"})
        features = ET.SubElement(domain, "features")
        ET.SubElement(features, "acpi")
        ET.SubElement(features, "apic")
        if policy.get("migration_pool"):
            # Let libvirt expand a named model before selecting its runtime check.
            # Forcing full on an unexpanded model rejects QEMU's implicit features.
            # https://libvirt.org/formatdomain.html#cpu-model-and-topology
            cpu = ET.SubElement(domain, "cpu", {"mode": "custom", "match": "exact"})
            ET.SubElement(cpu, "model", {"fallback": "forbid"}).text = policy["migration_pool"]["cpu_model"]
        else:
            ET.SubElement(domain, "cpu", {"mode": "host-model"})
        # qemu is unprivileged and the dedicated storage tree is accessible via its security driver.
        devices = ET.SubElement(domain, "devices")
        for source, target, kind in [(disk, "vda", "disk"), (seed, "sda", "cdrom")]:
            d = ET.SubElement(devices, "disk", {"type": "file", "device": kind})
            ET.SubElement(d, "driver", {"name": "qemu", "type": "qcow2" if kind == "disk" else "raw"})
            ET.SubElement(d, "source", {"file": str(source)})
            ET.SubElement(d, "target", {"dev": target, "bus": "virtio" if kind == "disk" else "sata"})
            if kind == "cdrom":
                ET.SubElement(d, "readonly")
        nic = ET.SubElement(devices, "interface", {"type": "network"})
        ET.SubElement(nic, "mac", {"address": mac})
        ET.SubElement(nic, "source", {"network": net["libvirt_network"]})
        ET.SubElement(nic, "model", {"type": "virtio"})
        ET.SubElement(nic, "rom", {"enabled": "no"})
        ET.SubElement(nic, "filterref", {"filter": "clean-traffic"})
        channel = ET.SubElement(devices, "channel", {"type": "unix"})
        ET.SubElement(channel, "target", {"type": "virtio", "name": "org.qemu.guest_agent.0"})
        serial = ET.SubElement(devices, "serial", {"type": "pty"})
        ET.SubElement(serial, "target", {"port": "0"})
        definition = directory / "domain.xml"
        definition.write_bytes(ET.tostring(domain))
        self.virsh("define", str(definition))
        self.assert_owned(resource)
        if start:
            self.virsh("start", resource["id"], timeout=60)

    def assert_owned(self, resource):
        require(resource.get("owner", "lens") == "lens" and not resource.get("protected"), "existing resources are protected")
        data = self.virsh("dumpxml", resource["id"])
        x = parse_domain(data)
        require(x["id"] == resource["id"] and x.get("operation_id") == resource["operation_id"], "VM ownership or identity changed")
        require(x["name"] == resource["name"] and [d.get("file") for d in x["detail"]["disks"]] == [str(self.storage / resource["id"] / "root.qcow2")], "managed VM name or storage binding changed; inspect locally")
        return x

    def ensure_running(self, resource):
        self.assert_owned(resource)
        self.instance_directory(resource)
        state = self.virsh("domstate", resource["id"]).decode().strip()
        if state == "shut off":
            self.virsh("start", resource["id"], timeout=60)
        else:
            require(state == "running", "managed VM needs explicit recovery from its current state")

    def shutdown(self, resource, seconds=120):
        self.assert_owned(resource)
        if self.virsh("domstate", resource["id"]).decode().strip() == "shut off":
            return
        self.virsh("shutdown", resource["id"])
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.virsh("domstate", resource["id"]).decode().strip() == "shut off":
                return
            time.sleep(1)
        raise Rejected("graceful shutdown timed out; no force was attempted")

    def require_stopped(self, resource):
        self.assert_owned(resource)
        require(self.virsh("domstate", resource["id"]).decode().strip() == "shut off", "VM must be shut down for this disk operation")

    def resize(self, resource, cls):
        self.require_stopped(resource)
        disk = self.storage / resource["id"] / "root.qcow2"
        info = json.loads(run(["/usr/bin/qemu-img", "info", "--output=json", str(disk)]))
        require(cls["disk_gib"] * 2**30 >= info["virtual-size"], "disk shrinking is not supported")
        if cls["disk_gib"] * 2**30 > info["virtual-size"]:
            run(["/usr/bin/qemu-img", "resize", str(disk), str(cls["disk_gib"]) + "G"])
        xml = ET.fromstring(self.virsh("dumpxml", resource["id"], "--inactive"))
        xml.find("memory").set("unit", "MiB")
        xml.find("memory").text = str(cls["memory_mib"])
        current = xml.find("currentMemory")
        if current is not None:
            current.set("unit", "MiB")
            current.text = str(cls["memory_mib"])
        xml.find("vcpu").text = str(cls["vcpus"])
        cpu = xml.find("cpu")
        if cpu is not None and cpu.find("topology") is not None:
            cpu.remove(cpu.find("topology"))
        if xml.find("vcpus") is not None:
            xml.remove(xml.find("vcpus"))
        path = self.storage / resource["id"] / "resize.xml"
        path.write_bytes(ET.tostring(xml))
        self.virsh("define", str(path))
        self.assert_owned(resource)

    def backup(self, resource, destination):
        self.require_stopped(resource)
        destination = Path(destination)
        destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        require(not destination.exists(), "backup destination already exists; inspect the interrupted operation")
        require(shutil.disk_usage(destination.parent).free >= (resource["disk_gib"] + 2) * 2**30, "insufficient free storage for a complete backup")
        source = self.storage / resource["id"] / "root.qcow2"
        pending = destination.with_suffix(".pending")
        require(not pending.exists(), "interrupted backup remains; inspect it locally before retrying")
        run(["/usr/bin/qemu-img", "convert", "-f", "qcow2", "-O", "qcow2", str(source), str(pending)], timeout=1800)
        os.chmod(pending, 0o600)
        run(["/usr/bin/qemu-img", "check", str(pending)], timeout=1800)
        with pending.open("rb") as f:
            os.fsync(f.fileno())
        os.replace(pending, destination)
        return file_digest(destination)

    def guest(self, resource, command):
        self.assert_owned(resource)
        result = native_agent(self.uri, resource["id"], json.dumps(command))
        parsed = json.loads(result)
        require("return" in parsed, "guest agent request failed")
        return parsed["return"]

    def guest_file(self, resource, destination, data):
        self.guest_stream(resource, destination, io.BytesIO(data))

    def guest_artifact(self, resource, destination, source, checksum):
        artifact = self.artifact(source, checksum)
        # Reuse verified guest-local bytes on reboot/restore instead of retransmitting
        # a large package. This cache never holds decrypted credentials.
        cached = self.guest_exec(resource, ["/usr/bin/python3", "-I", "-c",
            'import hashlib,pathlib,sys; p=pathlib.Path(sys.argv[1]); ok=False\n'
            'try:\n'
            ' if p.is_file() and not p.is_symlink():\n'
            '  with p.open("rb") as f: ok=hashlib.file_digest(f,"sha256").hexdigest()==sys.argv[2]\n'
            'except OSError: pass\n'
            'print("verified" if ok else "missing")', destination, checksum])
        if cached.strip() == b"verified":
            return
        with artifact.open("rb") as f:
            self.guest_stream(resource, destination, f)

    def guest_stream(self, resource, destination, reader):
        require(destination.startswith(("/run/lens/", "/opt/lens/", "/var/cache/lens/artifacts/")), "invalid guest delivery path")
        handle = self.guest(resource, {"execute": "guest-file-open", "arguments": {"path": destination, "mode": "wb"}})
        try:
            # Keep JSON frames small enough for guest-agent/virtio channel buffers.
            # A timed-out write is never retried, because its completion is unknown.
            while chunk := reader.read(8 * 1024):
                result = self.guest(resource, {"execute": "guest-file-write", "arguments": {"handle": handle, "buf-b64": base64.b64encode(chunk).decode()}})
                require(result.get("count") == len(chunk), "guest credential/artifact delivery was incomplete")
        finally:
            self.guest(resource, {"execute": "guest-file-close", "arguments": {"handle": handle}})

    def guest_exec(self, resource, argv, timeout=60):
        result = self.guest(resource, {"execute": "guest-exec", "arguments": {"path": argv[0], "arg": argv[1:], "capture-output": True}})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            status = self.guest(resource, {"execute": "guest-exec-status", "arguments": {"pid": result["pid"]}})
            if status.get("exited"):
                require(status.get("exitcode") == 0, "guest workload operation failed; inspect locally")
                return base64.b64decode(status.get("out-data", ""))
            time.sleep(0.5)
        raise Rejected("guest workload operation exceeded deadline")

    def wait_agent(self, resource, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                self.guest(resource, {"execute": "guest-ping"})
                return
            except Rejected:
                time.sleep(2)
        raise Rejected("guest agent did not become ready; use an approved image containing qemu-guest-agent")

    def address(self, resource, network):
        data = self.virsh("domifaddr", resource["id"], "--source", "agent").decode()
        subnet = ipaddress.ip_network(network["cidr"])
        for word in data.split():
            if "/" not in word:
                continue
            try:
                ip = ipaddress.ip_interface(word).ip
            except ValueError:
                continue
            if ip in subnet:
                return str(ip)
        raise Rejected("guest has no address in its authorised network")

    def remove(self, resource):
        """Only a recorded Lens-created VM can be removed. Never touches containers."""
        self.assert_owned(resource)
        state = self.virsh("domstate", resource["id"]).decode().strip()
        if state != "shut off":
            self.virsh("shutdown", resource["id"])
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if self.virsh("domstate", resource["id"]).decode().strip() == "shut off":
                    break
                time.sleep(1)
            else:
                raise Rejected("guest did not stop; forced destruction requires a separate local administrator action")
        self.assert_owned(resource)
        self.virsh("undefine", resource["id"])
        # Disk reclamation is deliberately separate; preserve files for incident recovery.


def allocations(policy, request):
    cls = policy["classes"][request["class"]]
    result = []
    for index in range(request["instances"]):
        identity = str(uuid.uuid5(uuid.NAMESPACE_URL, "/".join([policy["host_id"], request["operation_id"], str(index)])))
        result.append({"id": identity, "name": "li-" + identity.replace("-", "")[:24], "owner": "lens", "protected": 0,
                       "operation_id": request["operation_id"], "mac": mac_address(identity), **cls})
    return result
