"""Administrator-owned policy. Deployment callers only select approved objects."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import re
import tomllib
import uuid
import base64
from pathlib import Path


class Rejected(ValueError):
    """A request was rejected before changing infrastructure."""


def require(condition, message):
    if not condition:
        raise Rejected(message)


def name(value):
    require(isinstance(value, str) and re.fullmatch(r"[a-z][a-z0-9-]{0,47}", value),
            "identifiers must be lower-case names of at most 48 characters")
    return value


def integer(value, low, high, field):
    require(type(value) is int and low <= value <= high, f"invalid {field}")
    return value


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def keys(value, allowed, context):
    require(isinstance(value, dict), f"{context} must be an object")
    require(not (value.keys() - set(allowed)), f"unknown fields in {context}: {sorted(value.keys() - set(allowed))}")


def absolute(value, field):
    require(isinstance(value, str) and value.startswith("/") and "\x00" not in value,
            f"{field} must be an absolute path")
    require(".." not in Path(value).parts, f"invalid {field}")
    return value


def sha256(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "expected SHA-256 digest")
    return value


def public_key(value):
    require(isinstance(value, str) and len(value) <= 16384 and "\n" not in value, "invalid SSH public key")
    fields = value.split()
    require(len(fields) >= 2 and fields[0] in ("ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256"), "unsupported SSH public key format")
    try:
        blob = base64.b64decode(fields[1], validate=True)
        length = int.from_bytes(blob[:4], "big")
        require(blob[4:4 + length].decode() == fields[0] and len(blob) > 4 + length + 16, "SSH public key does not match its declared type")
    except (ValueError, UnicodeError):
        raise Rejected("invalid SSH public key encoding") from None


def load(path):
    raw = Path(path).read_bytes()
    require(len(raw) <= 1024 * 1024, "policy exceeds size limit")
    return validate(tomllib.loads(raw.decode()))


def validate(p):
    keys(p, ["schema_version", "revision", "host_id", "capacity", "classes", "networks",
             "environments", "templates", "principals", "secrets", "certificates", "protected_resources", "parked_resources", "runner_controller", "build_profiles", "migration_pool", "fabric", "monitoring", "images"], "policy")
    require(p.get("schema_version") == 1, "unsupported policy schema")
    integer(p.get("revision"), 1, 2**53, "policy revision")
    name(p.get("host_id"))
    if p.get("monitoring"):
        import math
        monitoring = p["monitoring"]
        keys(monitoring, ["sample_interval_seconds", "stale_after_seconds", "rules"], "monitoring")
        interval = integer(monitoring.get("sample_interval_seconds", 10), 5, 60, "monitoring interval")
        integer(monitoring.get("stale_after_seconds", 30), interval * 2, 600, "monitoring stale window")
        require(isinstance(monitoring.get("rules", []), list) and len(monitoring.get("rules", [])) <= 128, "monitoring rules must be a bounded list")
        names = set()
        for rule in monitoring.get("rules", []):
            keys(rule, ["name", "environment", "service", "container", "metric", "above", "clear_below", "for_seconds", "clear_for_seconds", "port", "path", "timeout_seconds", "expected_status"], "monitoring rule")
            for field in ("name", "environment", "service"):
                name(rule.get(field))
            name(rule.get("container", "vm"))
            require(rule["name"] not in names, "monitoring rule names must be unique")
            names.add(rule["name"])
            require(rule["environment"] in p.get("environments", {}), "unknown monitoring environment")
            require(rule.get("metric") in ("cpu_percent", "memory_percent", "endpoint_failure"), "unknown monitoring metric")
            for field, value in (("above", rule.get("above")), ("clear_below", rule.get("clear_below", rule.get("above")))):
                require(type(value) in (int, float) and math.isfinite(value) and 0 <= value <= 100, "invalid monitoring threshold")
            require(rule.get("clear_below", rule["above"]) <= rule["above"], "recovery threshold must not exceed alert threshold")
            integer(rule.get("for_seconds", 60), interval, 86400, "alert duration")
            integer(rule.get("clear_for_seconds", 30), interval, 86400, "alert recovery duration")
            if rule["metric"] == "endpoint_failure":
                integer(rule.get("port"), 1, 65535, "probe port")
                integer(rule.get("timeout_seconds", 2), 1, 10, "probe timeout")
                integer(rule.get("expected_status", 200), 100, 599, "expected probe status")
                require(rule["above"] < 1, "endpoint failure threshold must be below one")
                require(re.fullmatch(r"/[a-zA-Z0-9/_?.=-]*", rule.get("path", "/")), "invalid probe path")
            else:
                require(not any(k in rule for k in ("port", "path", "timeout_seconds", "expected_status")), "endpoint probe settings require endpoint_failure metric")
    if p.get("fabric"):
        fabric = p["fabric"]
        keys(fabric, ["name", "members", "gateway_host", "mtu", "protected_cidrs"], "VXLAN fabric")
        name(fabric.get("name"))
        integer(fabric.get("mtu", 1350), 1280, 1450, "overlay MTU")
        require(isinstance(fabric.get("members"), list) and 2 <= len(fabric["members"]) <= 32, "VXLAN fabric requires 2 through 32 explicit members")
        hosts, endpoints, nodes = set(), set(), set()
        require(isinstance(fabric.get("protected_cidrs"), list) and fabric["protected_cidrs"], "fabric management/transport networks must be explicitly protected")
        protected = [ipaddress.ip_network(v, strict=True) for v in fabric["protected_cidrs"]]
        require(all(v.version == 4 for v in protected), "v1 protected fabric networks must be IPv4")
        for member in fabric["members"]:
            keys(member, ["host_id", "endpoint", "node_id"], "fabric member")
            name(member.get("host_id"))
            address = ipaddress.ip_address(member.get("endpoint", ""))
            require(address.version == 4 and address.is_private and not address.is_multicast and not address.is_loopback and not address.is_unspecified, "fabric endpoints must be private unicast IPv4 addresses; use an encrypted underlay between sites")
            require(any(address in subnet for subnet in protected), "fabric endpoint must belong to a protected management/transport network")
            integer(member.get("node_id"), 1, 32, "fabric node ID")
            require(member["host_id"] not in hosts and str(address) not in endpoints and member["node_id"] not in nodes, "fabric identities, endpoints and node IDs must be unique")
            hosts.add(member["host_id"]); endpoints.add(str(address)); nodes.add(member["node_id"])
        require(p["host_id"] in hosts and fabric.get("gateway_host") in hosts, "host and gateway must be explicit fabric members")
    if p.get("migration_pool"):
        pool = p["migration_pool"]
        keys(pool, ["name", "cpu_model", "machine"], "migration pool")
        name(pool.get("name"))
        require(isinstance(pool.get("cpu_model"), str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", pool["cpu_model"]), "invalid migration CPU model")
        require(isinstance(pool.get("machine"), str) and re.fullmatch(r"pc-q35-[0-9]+\.[0-9]+", pool["machine"]), "migration pools require a pinned q35 machine version")
    c = p.get("capacity", {})
    keys(c, ["vcpus", "memory_mib", "disk_gib", "host_reserve_mib", "cpu_overcommit_percent"], "capacity")
    for key, low in [("vcpus", 1), ("memory_mib", 128), ("disk_gib", 1)]:
        integer(c.get(key), low, 2**31, key)
    integer(c.get("host_reserve_mib", 1024), 128, 2**31, "host reserve")
    integer(c.get("cpu_overcommit_percent", 100), 100, 200, "CPU overcommit percentage")
    require(isinstance(p.get("parked_resources", []), list), "parked resources must be a list")
    for identity in p.get("parked_resources", []):
        require(re.fullmatch(r"wf-runner:[0-9]{2}", identity) or str(uuid.UUID(identity)) == identity, "parked resources must name exact VM or adopted slot identities")
    if p.get("runner_controller"):
        config = p["runner_controller"]
        keys(config, ["provider", "host_prefix", "max_slots", "control_slots"], "runner controller")
        require(config.get("provider") == "wildfoundry-runner-v1", "unknown runner controller provider")
        name(config.get("host_prefix"))
        integer(config.get("max_slots"), 1, 99, "runner slot limit")
        require(isinstance(config.get("control_slots"), list), "explicit adopted slots are required")
        for slot in config["control_slots"]:
            integer(slot, 1, config["max_slots"], "adopted runner slot")
    for identity, reservation in p.get("protected_resources", {}).items():
        require(isinstance(identity, str) and re.fullmatch(r"[A-Za-z0-9:_.-]{1,128}", identity), "invalid protected resource identity")
        keys(reservation, ["vcpus", "memory_mib", "disk_gib"], "protected resource reservation")
        for key in ("vcpus", "memory_mib", "disk_gib"):
            integer(reservation.get(key), 1, c[key], "protected resource " + key)
    for n, cls in p.get("classes", {}).items():
        name(n)
        keys(cls, ["vcpus", "memory_mib", "disk_gib"], "class")
        for k in ["vcpus", "memory_mib", "disk_gib"]:
            integer(cls.get(k), 1, c[k], k)
    require(p.get("classes"), "at least one class is required")
    nets = p.get("networks", {})
    libvirt_names = set()
    used_subnets = []
    used_bridges = set()
    used_vnis = set()
    for n, net in nets.items():
        name(n)
        keys(net, ["environment", "libvirt_network", "cidr", "mode", "bridge", "vlan", "managed", "egress", "vni"], "network")
        name(net.get("environment"))
        name(net.get("libvirt_network"))
        require(net["libvirt_network"] not in libvirt_names, "a network cannot belong to two environments")
        libvirt_names.add(net["libvirt_network"])
        subnet = ipaddress.ip_network(net.get("cidr", ""), strict=True)
        require(subnet.version == 4 and 16 <= subnet.prefixlen <= 28, "v1 networks require an IPv4 /16 through /28")
        require(not any(subnet.overlaps(other) for other in used_subnets), "environment network ranges must not overlap")
        used_subnets.append(subnet)
        require(net.get("mode") in ("isolated", "nat", "vlan", "vxlan"), "invalid network mode")
        require(type(net.get("managed", False)) is bool, "managed must be boolean")
        if "bridge" in net:
            require(re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_.-]{0,14}", net["bridge"]), "invalid bridge")
            require(net["bridge"] not in used_bridges, "v1 environment networks require distinct bridges")
            used_bridges.add(net["bridge"])
        for destination in net.get("egress", []):
            require(ipaddress.ip_network(destination, strict=True).version == 4, "v1 egress destinations must be IPv4 networks")
        if net.get("managed"):
            require(net["libvirt_network"].startswith("li-"), "managed networks must use the li- namespace")
            require("bridge" not in net, "managed bridge names are derived by the controller")
        if net["mode"] == "vxlan":
            require(net.get("managed") and p.get("fabric"), "VXLAN networks require managed bridges and an explicit fabric")
            require(subnet.prefixlen <= 26, "VXLAN subnets reserve addresses for fabric nodes and require /26 or larger")
            integer(net.get("vni"), 1, 16777215, "VXLAN network ID")
            require(net["vni"] not in used_vnis, "VXLAN network IDs must be distinct")
            used_vnis.add(net["vni"])
            require(max(m["node_id"] for m in p["fabric"]["members"]) + 3 < subnet.num_addresses, "subnet is too small for fabric nodes and guest addresses")
            require(not any(subnet.overlaps(ipaddress.ip_network(v)) for v in p["fabric"]["protected_cidrs"]), "guest networks must not overlap protected fabric networks")
        else:
            require("vni" not in net, "VNI is only valid for VXLAN networks")
        if net["mode"] == "vlan":
            integer(net.get("vlan"), 1, 4094, "VLAN")
            require(re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_.-]{0,14}", net.get("bridge", "")), "invalid bridge")
            require(not net.get("managed"), "VLAN bridges must be provisioned by a local administrator")
    for n, env in p.get("environments", {}).items():
        name(n)
        keys(env, ["networks", "classes", "templates", "max_instances", "memory_mib", "vcpus", "disk_gib", "images"], "environment")
        for v in env.get("networks", []):
            require(v in nets and nets[v]["environment"] == n, "environment network ownership mismatch")
        require(env.get("networks"), "environment needs a network")
        require(env.get("classes") and all(v in p["classes"] for v in env["classes"]), "unknown class")
        integer(env.get("max_instances"), 1, 10000, "instance limit")
        for k in ["memory_mib", "vcpus", "disk_gib"]:
            integer(env.get(k), 1, c[k], k)
    images = p.get("images", {})
    for image_id, image in images.items():
        name(image_id)
        keys(image, ["path", "sha256", "format", "os_family", "capabilities"], "image")
        absolute(image.get("path"), "image path")
        sha256(image.get("sha256"))
        require(image.get("format", "qcow2") in ("qcow2", "raw"), "unsupported image format")
        require(image.get("os_family") in ("debian", "linux"), "unsupported image OS family")
        require(isinstance(image.get("capabilities"), list) and all(v in ("cloud-init", "qemu-guest-agent", "python3", "nftables", "systemd", "docker", "runner-runtime", "bash") for v in image["capabilities"]), "unsupported image capabilities")
    for environment in p.get("environments", {}).values():
        require(all(v in images for v in environment.get("images", [])), "unknown approved environment image")
    templates = p.get("templates", {})
    for n, profile in p.get("build_profiles", {}).items():
        name(n)
        keys(profile, ["lens", "logging", "ssh_keys", "users", "shell", "files", "run_once"], "build profile")
        if "shell" in profile:
            require(profile["shell"] == "dataplicity-server-v1", "unknown server shell profile")
        require(profile, "build profile must select agents or administrator SSH keys")
        require(isinstance(profile.get("ssh_keys", []), list), "administrator SSH keys must be a list")
        for key in profile.get("ssh_keys", []):
            public_key(key)
        require(not (profile.get("ssh_keys") and profile.get("users")), "choose named users or one bootstrap account, not both")
        require(isinstance(profile.get("users", []), list), "build users must be a list")
        usernames = set()
        for user in profile.get("users", []):
            keys(user, ["name", "ssh_keys", "sudo"], "build user")
            require(isinstance(user.get("name"), str) and re.fullmatch(r"[a-z][a-z0-9_-]{0,24}", user["name"]), "invalid administrator username")
            require(user["name"] not in ("root", "nobody", "daemon", "syslog", "sshd", "runner") and user["name"] not in usernames, "reserved or duplicate build user")
            usernames.add(user["name"])
            require(type(user.get("sudo", False)) is bool, "sudo membership must be explicit boolean")
            require(isinstance(user.get("ssh_keys"), list) and user["ssh_keys"], "named users require SSH public keys")
            for key in user["ssh_keys"]:
                public_key(key)
        for kind in ("files", "run_once"):
            items = profile.get(kind, [])
            require(isinstance(items, list) and len(items) <= 32, "bootstrap entries must be a bounded list")
            names = set()
            for item in items:
                allowed = ["name", "path", "sha256", "destination", "mode", "owner", "group"] if kind == "files" else ["name", "path", "sha256", "interpreter", "timeout_seconds"]
                keys(item, allowed, "bootstrap " + kind)
                name(item.get("name"))
                require(item["name"] not in names, "bootstrap names must be unique")
                names.add(item["name"])
                absolute(item.get("path"), "bootstrap artifact")
                sha256(item.get("sha256"))
                if kind == "files":
                    absolute(item.get("destination"), "injection destination")
                    forbidden = ("/proc", "/sys", "/dev", "/run/lens", "/opt/lens", "/var/lib/lens", "/var/cache/lens")
                    require(not any(item["destination"] == x or item["destination"].startswith(x + "/") for x in forbidden), "injection destination is reserved")
                    require(re.fullmatch(r"0[0-7]{3}", item.get("mode", "0644")), "file mode must have four octal digits")
                    require(int(item.get("mode", "0644"), 8) & 0o022 == 0, "injected files cannot be group/world writable")
                    for field in ("owner", "group"):
                        require(re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", item.get(field, "root")), "invalid guest file ownership")
                else:
                    require(item.get("interpreter", "/bin/sh") in ("/bin/sh", "/bin/bash", "/usr/bin/python3"), "unsupported bootstrap interpreter")
                    integer(item.get("timeout_seconds", 300), 1, 3600, "bootstrap timeout")
        for component, artifact in profile.items():
            if component in ("ssh_keys", "users", "shell", "files", "run_once"):
                continue
            keys(artifact, ["path", "sha256", "version"] if component == "lens" else ["path", "sha256"], "build artifact")
            absolute(artifact.get("path"), "build artifact")
            sha256(artifact.get("sha256"))
            if component == "lens":
                require(isinstance(artifact.get("version"), str) and re.fullmatch(r"[0-9][0-9A-Za-z.+~-]{0,63}", artifact["version"]), "Lens package version is required")
    for n, t in templates.items():
        name(n)
        keys(t, ["kind", "image", "image_sha256", "image_format", "secrets", "profile", "port",
                 "health_path", "startup_seconds", "drain_seconds", "containers", "runner", "certificate", "listen_port",
                 "websocket_path", "rpc_path", "rpc_health_method", "observe_seconds", "canary_percent", "build_profile"], "template")
        if t.get("build_profile"):
            require(t["build_profile"] in p.get("build_profiles", {}), "unknown build profile")
        require(t.get("kind") in ("oci", "github-runner", "appliance"), "unsupported workload kind")
        if t.get("image"):
            absolute(t.get("image"), "VM image")
            sha256(t.get("image_sha256"))
        else:
            require(images, "template requires an approved image catalogue")
            require("image_sha256" not in t and "image_format" not in t, "image checksums/formats belong to the image catalogue")
        require(t.get("image_format", "qcow2") in ("qcow2", "raw"), "invalid image format")
        require(t.get("profile", "private") in ("private", "fast-web", "websocket", "jsonrpc"), "invalid endpoint profile")
        integer(t.get("startup_seconds", 180), 5, 1800, "startup deadline")
        integer(t.get("drain_seconds", 30), 0, 86400, "drain deadline")
        integer(t.get("observe_seconds", 10), 0, 300, "observation duration")
        integer(t.get("canary_percent", 5), 0, 50, "canary percentage")
        if t.get("profile", "private") != "private":
            integer(t.get("port"), 1, 65535, "service port")
            integer(t.get("listen_port", 8080), 1024, 65535, "approved listener port")
            require(re.fullmatch(r"/[a-zA-Z0-9/_?.=-]*", t.get("health_path", "/health")), "invalid health path")
            for field in ("websocket_path", "rpc_path"):
                require(re.fullmatch(r"/[a-zA-Z0-9/_?.=-]*", t.get(field, "/")), "invalid protocol health path")
            if t["profile"] == "jsonrpc":
                require(re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_.]{0,63}", t.get("rpc_health_method", "lens.health")), "invalid RPC health method")
        for secret in t.get("secrets", []):
            name(secret)
            require(secret in p.get("secrets", {}), "unknown local secret binding")
        containers = t.get("containers", [])
        if t["kind"] == "oci":
            require(isinstance(containers, list) and 1 <= len(containers) <= 2, "OCI templates require an application and at most one shim")
            require(len({c.get("name") for c in containers}) == len(containers), "container names must be distinct")
        else:
            require(not containers, "only OCI templates may declare containers")
        for container in t.get("containers", []):
            keys(container, ["name", "image", "port", "command", "secret_files", "archive", "archive_sha256", "registry_secret", "image_id"], "container")
            name(container.get("name"))
            require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._:/-]*@sha256:[0-9a-f]{64}", container.get("image", "")), "OCI images must be pinned by digest; any registry is supported")
            if "port" in container:
                integer(container["port"], 1, 65535, "container port")
            require(all(isinstance(v, str) and "\x00" not in v for v in container.get("command", [])), "invalid container arguments")
            require(all(v in t.get("secrets", []) for v in container.get("secret_files", [])), "undeclared container credential")
            if container.get("registry_secret"):
                require(container["registry_secret"] in t.get("secrets", []), "registry credential must be declared")
            if container.get("archive"):
                absolute(container["archive"], "OCI archive")
                sha256(container.get("archive_sha256"))
                require(re.fullmatch(r"sha256:[0-9a-f]{64}", container.get("image_id", "")), "local OCI archives require their pinned Docker image ID")
        if t["kind"] == "github-runner":
            r = t.get("runner", {})
            keys(r, ["archive", "sha256", "url", "labels", "group", "registration_secret", "ephemeral"], "runner")
            require(type(r.get("ephemeral", True)) is bool, "ephemeral must be boolean")
            absolute(r.get("archive"), "runner archive")
            sha256(r.get("sha256"))
            require(re.fullmatch(r"https://github\.com/[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)?", r.get("url", "")), "runner target must be a GitHub organisation or repository")
            require(r.get("registration_secret") in t.get("secrets", []), "runner registration credential must be declared")
            require(isinstance(r.get("labels"), list) and r["labels"], "runner labels must be explicitly declared")
            for label in r.get("labels", []):
                require(re.fullmatch(r"[A-Za-z0-9_.-]{1,63}", label), "invalid runner label")
            require(re.fullmatch(r"[A-Za-z0-9_. -]{1,100}", r.get("group", "Default")), "invalid runner group")
            require(t.get("profile", "private") == "private", "runners do not expose application ingress")
    for env in p.get("environments", {}).values():
        require(env.get("templates") and all(v in templates for v in env["templates"]), "unknown template")
    for n, binding in p.get("secrets", {}).items():
        name(n)
        keys(binding, ["path", "encrypted", "generation", "environments"], "secret binding")
        absolute(binding.get("path"), "secret path")
        integer(binding.get("generation"), 1, 2**53, "secret generation")
        require(type(binding.get("encrypted", True)) is bool, "encrypted must be boolean")
        require(binding.get("environments") and all(e in p.get("environments", {}) for e in binding["environments"]), "invalid secret environment binding")
    for n, cert in p.get("certificates", {}).items():
        name(n)
        keys(cert, ["path", "environment"], "certificate")
        absolute(cert.get("path"), "certificate path")
        require(cert.get("environment") in p.get("environments", {}), "invalid certificate environment")
    for principal in p.get("principals", []):
        keys(principal, ["uid", "role", "environments"], "principal")
        integer(principal.get("uid"), 0, 2**32 - 2, "principal uid")
        require(principal.get("role") in ("reader", "deployer", "operator", "admin"), "invalid role")
        require(principal.get("role") != "admin" or principal["uid"] == 0, "administrator authority belongs to the local root identity")
        require(all(e in p.get("environments", {}) for e in principal.get("environments", [])), "unknown principal environment")
    return p


def authorize(p, uid, action, environment=None):
    if uid == 0:
        return
    for principal in p.get("principals", []):
        if principal["uid"] != uid:
            continue
        if principal["role"] == "admin":
            return
        if action == "read" and (environment is None or environment in principal.get("environments", [])):
            return
        if action == "deploy" and principal["role"] in ("deployer", "operator") and environment in principal.get("environments", []):
            return
        if action == "recover" and principal["role"] == "operator" and environment in principal.get("environments", []):
            return
    raise Rejected("local identity is not authorised for this operation")


def deployment(p, request):
    keys(request, ["operation_id", "configuration_revision", "configuration_digest", "host_id", "environment", "service", "class", "template",
                   "network", "instances", "hostname", "listen_port", "release", "mode", "restore_backup", "start", "image"], "deployment request")
    require(request.get("mode", "service") in ("service", "server"), "invalid workload mode")
    if request.get("mode") == "server":
        require(request.get("instances") == 1, "persistent servers have one instance; use explicit VM lifecycle operations")
    if "start" in request:
        require(type(request["start"]) is bool and request.get("mode") == "server", "initial power state is only available for a persistent server")
    if request.get("restore_backup"):
        name(request["restore_backup"])
        require(request.get("mode") == "server", "backups restore to a new private server")
        require(request.get("start") is False, "restored servers remain shut down until explicitly booted")
    for k in ["operation_id", "environment", "service", "class", "template", "network", "release"]:
        name(request.get(k))
    require(request.get("configuration_revision") == p["revision"], "requested configuration revision is not active locally")
    require(request.get("host_id") == p["host_id"], "request targets a different host")
    if "configuration_digest" in request:
        require(request["configuration_digest"] == digest(p), "requested configuration digest does not match the accepted policy")
    e = p["environments"].get(request["environment"])
    require(e is not None, "unknown environment")
    require(request["class"] in e["classes"], "class is not available in this environment")
    require(request["template"] in e["templates"], "template is not available in this environment")
    require(request["network"] in e["networks"], "network is not owned by this environment")
    integer(request.get("instances"), 0, e["max_instances"], "instance count")
    t = p["templates"][request["template"]]
    require(request.get("image") or t.get("image"), "select an explicit approved local VM image")
    if request.get("image"):
        image_id = name(request["image"])
        require(image_id in p.get("images", {}) and image_id in e.get("images", []), "image is not approved for this environment")
        image = p["images"][image_id]
        required = {"cloud-init", "qemu-guest-agent", "python3", "nftables", "systemd"}
        if t["kind"] == "oci":
            required.add("docker")
        if t["kind"] == "github-runner":
            required.add("runner-runtime")
        require(required <= set(image["capabilities"]), "image lacks required workload capabilities")
        profile = p.get("build_profiles", {}).get(t.get("build_profile"), {})
        require(not (profile.get("lens") or profile.get("logging")) or image["os_family"] == "debian", "selected build profile requires a Debian-family image")
    if request.get("mode") == "server":
        profile = p.get("build_profiles", {}).get(t.get("build_profile"), {})
        require(profile.get("ssh_keys") or any(u.get("sudo") for u in profile.get("users", [])), "persistent server builds require administrator SSH keys in an approved build profile")
    for s in t.get("secrets", []):
        require(request["environment"] in p["secrets"][s]["environments"], "secret is not available to this environment")
    if t.get("certificate"):
        require(p.get("certificates", {}).get(t["certificate"], {}).get("environment") == request["environment"], "certificate is not available to this environment")
    if "hostname" in request:
        require(re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,251}[a-z0-9]", request["hostname"]), "invalid hostname")
    if "listen_port" in request:
        integer(request["listen_port"], 1024, 65535, "listener port")
    if request.get("mode") != "server" and t.get("profile", "private") != "private":
        require(request.get("listen_port", 8080) == t.get("listen_port", 8080), "listener port is fixed by the approved template")
    return request
