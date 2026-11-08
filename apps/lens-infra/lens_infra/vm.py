"""Typed VM and disk lifecycle. No raw host paths or force-stop operations."""

import json
import time

from .policy import Rejected, authorize, keys, name, require


def create_request(payload):
    keys(payload, ["operation_id", "configuration_revision", "host_id", "environment", "name", "class", "template", "network", "release", "start"], "VM build")
    request = dict(payload)
    request["service"] = request.pop("name")
    request.update(instances=1, mode="server")
    return request


def restore_request(engine, payload, uid):
    keys(payload, ["operation_id", "configuration_revision", "host_id", "backup_id", "name", "class", "network"], "VM restore")
    backup = engine.store.backup(payload["backup_id"])
    authorize(engine.policy(), uid, "recover", backup["environment"])
    request = dict(backup["detail"]["source_request"])
    request.pop("configuration_digest", None)
    request.update(operation_id=payload["operation_id"], configuration_revision=payload["configuration_revision"], host_id=payload["host_id"],
                   service=payload["name"], **{"class": payload["class"]}, network=payload["network"], instances=1, mode="server",
                   restore_backup=backup["id"], release=("restore-" + backup["id"])[:48], start=False)
    return request


def plan(engine, payload, uid):
    keys(payload, ["operation_id", "host_id", "configuration_revision", "instance_id", "action", "new_class"], "VM action")
    name(payload.get("operation_id"))
    policy = engine.policy()
    require(payload.get("host_id") == policy["host_id"] and payload.get("configuration_revision") == policy["revision"], "VM operation requires the active host and configuration revision")
    resource = engine.store.resource(payload["instance_id"])
    authorize(policy, uid, "deploy", resource["environment"])
    require(resource["owner"] == "lens" and not resource["protected"] and resource["state"] != "removed", "existing/imported resources are protected")
    require(payload.get("action") in ("boot", "shutdown", "resize", "backup", "delete"), "unsupported VM operation")
    engine.backend.assert_owned(resource)
    source = engine.store.operation(resource["operation_id"])["request"]
    if source.get("restore_backup"):
        authorize(policy, uid, "recover", resource["environment"])
    template = engine.template(engine.store.policy_at(source["configuration_revision"]), source)
    require(template["kind"] != "github-runner", "runner VM power changes require its job-aware runner controller")
    if payload["action"] in ("resize", "backup", "delete"):
        require(resource["detail"].get("desired_power") == "off", "explicitly shut down the VM before this disk operation")
        engine.backend.require_stopped(resource)
    if payload["action"] == "resize":
        cls = payload.get("new_class")
        require(cls in policy["environments"][resource["environment"]]["classes"], "class is not available in this environment")
        require(policy["classes"][cls]["disk_gib"] >= resource["disk_gib"], "disk shrinking is not supported")
    else:
        require("new_class" not in payload, "new class is only valid for a resize")
    return {"request": payload, "instance_id": resource["id"], "environment": resource["environment"], "action": payload["action"],
            "planned_shutdown_required": payload["action"] in ("resize", "backup", "delete"),
            "disks_retained": payload["action"] == "delete", "backup_consistency": "offline" if payload["action"] == "backup" else None}


def submit(engine, payload, uid):
    plan(engine, payload, uid)
    return engine.store.reserve_action(engine.policy(), payload, uid)


def republish(engine, source):
    if source["request"].get("mode") == "server":
        return
    p = engine.store.policy_at(source["request"]["configuration_revision"])
    template = engine.template(p, source["request"])
    if template.get("profile", "private") == "private":
        return
    backends = [{"id": r["id"], "address": r["detail"]["address"]} for r in engine.store.operation(source["id"])["resources"]
                if r["state"] == "ready" and r["detail"].get("desired_power") != "off" and r["detail"].get("address")]
    cert = p.get("certificates", {}).get(template.get("certificate"), {}).get("path")
    engine.ingress.publish(source["request"], template, backends, cert)


def execute(engine, operation):
    request = operation["request"]
    identity = request["instance_id"]
    resource = engine.store.resource(identity)
    source = engine.store.operation(resource["operation_id"])
    p = engine.store.policy_at(request["configuration_revision"])
    old_policy = engine.store.policy_at(source["request"]["configuration_revision"])
    template = engine.template(old_policy, source["request"])
    try:
        engine.store.transition(operation["id"], request["action"])
        require(resource["owner"] == "lens" and not resource["protected"], "VM is protected")
        if request["action"] == "shutdown":
            engine.store.resource_detail(identity, resource["detail"] | {"desired_power": "off"})
            republish(engine, source)
            if template.get("profile", "private") != "private":
                deadline = time.monotonic() + template.get("drain_seconds", 30)
                while time.monotonic() < deadline and engine.ingress.connections(resource["environment"] + "/" + resource["service"], retired=True):
                    time.sleep(1)
            engine.backend.shutdown(resource)
            engine.store.resource_state(identity, "stopped")
        elif request["action"] == "boot":
            from .runner_provider import admission_lock
            engine.backend.assert_isolation(p)
            with admission_lock(p):
                engine.inventory()
                engine.store.check_capacity(p, source["request"], [])
                engine.backend.check_host_capacity(p, [resource])
                engine.store.resource_detail(identity, resource["detail"] | {"desired_power": "on"})
                engine.backend.ensure_running(resource)
            engine.backend.wait_agent(resource, template.get("startup_seconds", 180))
            if not resource["detail"].get("provisioned"):
                engine.backend.guest_exec(resource, ["/usr/bin/cloud-init", "status", "--wait"], timeout=template.get("startup_seconds", 180))
            generations = engine.deliver(resource, source["request"], old_policy)
            address = engine.backend.address(resource, old_policy["networks"][source["request"]["network"]])
            engine.wait_ready(resource, address, template)
            current = engine.store.resource(identity)
            root_bytes = engine.backend.guest_exec(resource, ["/usr/bin/python3", "-I", "-c", 'import os; s=os.statvfs("/"); print(s.f_blocks*s.f_frsize)'])
            # Older approved workload adapters may not yet return this optional metric.
            root_bytes = int(root_bytes.strip()) if root_bytes.strip().isdigit() else None
            engine.store.resource_detail(identity, current["detail"] | {"address": address, "secret_generations": generations, "desired_power": "on", "guest_root_bytes": root_bytes, "provisioned": True})
            engine.store.resource_state(identity, "ready")
            republish(engine, source)
        elif request["action"] == "resize":
            cls = p["classes"][request["new_class"]]
            engine.backend.resize(resource, cls)
            engine.store.resized(identity, cls, request["new_class"], p["revision"])
            engine.store.resource_state(identity, "stopped")
        elif request["action"] == "backup":
            destination = engine.store.root / "backups" / operation["id"] / "root.qcow2"
            backup = engine.store.backup(operation["id"])
            if backup["state"] != "ready":
                checksum = engine.backend.backup(resource, destination)
                engine.store.backup_ready(operation["id"], checksum)
        elif request["action"] == "delete":
            engine.backend.require_stopped(resource)
            engine.backend.remove(resource)
            engine.store.resource_state(identity, "removed")
            engine.store.detach_service(resource["operation_id"])
        engine.store.transition(operation["id"], "complete", "succeeded")
    except Exception as exc:
        error = str(exc) if isinstance(exc, Rejected) else "local VM operation failed; inspect administrator diagnostics"
        engine.store.transition(operation["id"], "failed", "failed", error)
