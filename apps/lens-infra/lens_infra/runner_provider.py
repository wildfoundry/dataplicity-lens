"""Explicit adapter for our existing runner controller; other VMs stay protected."""

import contextlib
import fcntl
import os
import re
from pathlib import Path

from .backend import run
from .policy import require

BOOT_LOCK = Path("/etc/wildfoundry-runners/boot.lock")


def canonical_inventory(policy, resources):
    config = policy.get("runner_controller") if policy else None
    if not config:
        return resources
    prefix = config["host_prefix"]
    for resource in resources:
        match = re.fullmatch(re.escape(prefix) + r"-([0-9]{2})", resource["name"])
        if resource["kind"] != "vm" or not match or not 1 <= int(match[1]) <= config["max_slots"]:
            continue
        expected = "/data/github-runner/vms/" + resource["name"] + "/disk.qcow2"
        require([d.get("file") for d in resource["detail"]["disks"]] == [expected], "runner slot has an unexpected storage binding")
        resource["detail"].update(provider="wildfoundry-runner-v1", slot=int(match[1]), vm_uuid=resource["id"])
        resource["id"] = "wf-runner:" + match[1]
    return resources


@contextlib.contextmanager
def admission_lock(policy):
    if not policy.get("runner_controller"):
        yield
        return
    # The adopted and old runner controllers already use this lock. Hold it until
    # a Lens VM is running and visible to their physical-memory admission check.
    # Linux flock does not require a writable descriptor. Keep /etc read-only
    # under the controller's systemd sandbox while sharing the original inode.
    fd = os.open(BOOT_LOCK, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        st = os.fstat(fd)
        require(st.st_uid == 0 and st.st_mode & 0o077 == 0, "runner boot lock is not trusted")
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def control(policy, payload, uid):
    from . import runner_state
    from .policy import keys, integer
    require(uid == 0, "adopting or controlling existing runner slots requires the local administrator")
    keys(payload, ["host_id", "configuration_revision", "slot", "vm_uuid", "action"], "runner selector")
    require(payload.get("host_id") == policy["host_id"] and payload.get("configuration_revision") == policy["revision"], "runner control requires the active host and configuration revision")
    config = policy.get("runner_controller")
    require(config and payload.get("slot") in config["control_slots"], "runner slot is not explicitly adopted for control")
    slot = integer(payload["slot"], 1, config["max_slots"], "runner slot")
    name = config["host_prefix"] + f"-{slot:02}"
    actual = run(["/usr/bin/virsh", "--connect", "qemu:///system", "domuuid", name]).decode().strip()
    require(actual == payload.get("vm_uuid"), "runner VM generation changed; inspect the slot before changing it")
    require(payload.get("action") in ("hold", "resume"), "invalid runner action")
    return runner_state.request(slot, payload["action"], actual, actor=uid)
