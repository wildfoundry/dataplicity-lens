"""Administrator-owned policy. Deployment callers only select approved objects."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import re
import tomllib
import uuid
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


def load(path):
    raw = Path(path).read_bytes()
    require(len(raw) <= 1024 * 1024, "policy exceeds size limit")
    return validate(tomllib.loads(raw.decode()))


def validate(p):
    keys(p, ["schema_version", "revision", "host_id", "capacity", "classes", "networks",
             "environments", "templates", "principals", "secrets", "certificates", "protected_resources", "parked_resources", "runner_controller"], "policy")
    require(p.get("schema_version") == 1, "unsupported policy schema")
    integer(p.get("revision"), 1, 2**53, "policy revision")
    name(p.get("host_id"))
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
    for n, net in nets.items():
        name(n)
        keys(net, ["environment", "libvirt_network", "cidr", "mode", "bridge", "vlan", "managed", "egress"], "network")
        name(net.get("environment"))
        name(net.get("libvirt_network"))
        require(net["libvirt_network"] not in libvirt_names, "a network cannot belong to two environments")
        libvirt_names.add(net["libvirt_network"])
        subnet = ipaddress.ip_network(net.get("cidr", ""), strict=True)
        require(subnet.version == 4 and 24 <= subnet.prefixlen <= 28, "v1 networks require an IPv4 /24 through /28")
        require(not any(subnet.overlaps(other) for other in used_subnets), "environment network ranges must not overlap")
        used_subnets.append(subnet)
        require(net.get("mode") in ("isolated", "nat", "vlan"), "invalid network mode")
        require(type(net.get("managed", False)) is bool, "managed must be boolean")
        if "bridge" in net:
            require(re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_.-]{0,14}", net["bridge"]), "invalid bridge")
            require(net["bridge"] not in used_bridges, "v1 environment networks require distinct bridges")
            used_bridges.add(net["bridge"])
        for destination in net.get("egress", []):
            require(ipaddress.ip_network(destination, strict=True).version == 4, "v1 egress destinations must be IPv4 networks")
        if net.get("managed"):
            require(net["libvirt_network"].startswith("li-"), "managed networks must use the li- namespace")
        if net["mode"] == "vlan":
            integer(net.get("vlan"), 1, 4094, "VLAN")
            require(re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_.-]{0,14}", net.get("bridge", "")), "invalid bridge")
            require(not net.get("managed"), "VLAN bridges must be provisioned by a local administrator")
    for n, env in p.get("environments", {}).items():
        name(n)
        keys(env, ["networks", "classes", "templates", "max_instances", "memory_mib", "vcpus", "disk_gib"], "environment")
        for v in env.get("networks", []):
            require(v in nets and nets[v]["environment"] == n, "environment network ownership mismatch")
        require(env.get("networks"), "environment needs a network")
        require(env.get("classes") and all(v in p["classes"] for v in env["classes"]), "unknown class")
        integer(env.get("max_instances"), 1, 10000, "instance limit")
        for k in ["memory_mib", "vcpus", "disk_gib"]:
            integer(env.get(k), 1, c[k], k)
    templates = p.get("templates", {})
    for n, t in templates.items():
        name(n)
        keys(t, ["kind", "image", "image_sha256", "image_format", "secrets", "profile", "port",
                 "health_path", "startup_seconds", "drain_seconds", "containers", "runner", "certificate", "listen_port",
                 "websocket_path", "rpc_path", "rpc_health_method", "observe_seconds", "canary_percent"], "template")
        require(t.get("kind") in ("oci", "github-runner", "appliance"), "unsupported workload kind")
        absolute(t.get("image"), "VM image")
        sha256(t.get("image_sha256"))
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
        require(principal.get("role") in ("reader", "deployer", "admin"), "invalid role")
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
        if action == "deploy" and principal["role"] == "deployer" and environment in principal.get("environments", []):
            return
    raise Rejected("local identity is not authorised for this operation")


def deployment(p, request):
    keys(request, ["operation_id", "configuration_revision", "configuration_digest", "host_id", "environment", "service", "class", "template",
                   "network", "instances", "hostname", "listen_port", "release"], "deployment request")
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
    for s in t.get("secrets", []):
        require(request["environment"] in p["secrets"][s]["environments"], "secret is not available to this environment")
    if t.get("certificate"):
        require(p.get("certificates", {}).get(t["certificate"], {}).get("environment") == request["environment"], "certificate is not available to this environment")
    if "hostname" in request:
        require(re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,251}[a-z0-9]", request["hostname"]), "invalid hostname")
    if "listen_port" in request:
        integer(request["listen_port"], 1024, 65535, "listener port")
    if t.get("profile", "private") != "private":
        require(request.get("listen_port", 8080) == t.get("listen_port", 8080), "listener port is fixed by the approved template")
    return request
