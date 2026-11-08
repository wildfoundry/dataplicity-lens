"""Persistent VM controls, disk reservation and restore boundaries."""

import base64
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from lens_infra.engine import Engine
from lens_infra.policy import Rejected, deployment, validate
from lens_infra.store import Store
from lens_infra import vm
from test_infrastructure import Backend, Proxy, policy


def public_key():
    blob = b"\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20" + b"x" * 32
    return "ssh-ed25519 " + base64.b64encode(blob).decode() + " fixture"


class VmLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.policy = policy(self.root)
        self.policy["build_profiles"] = {"owner-v1": {"ssh_keys": [public_key()]}}
        self.policy["principals"].append({"uid": 1003, "role": "operator", "environments": ["production"]})
        self.policy["templates"]["web-v1"]["build_profile"] = "owner-v1"
        self.policy["classes"]["larger-v1"] = {"vcpus": 2, "memory_mib": 1024, "disk_gib": 16}
        self.policy["classes"]["too-small-v1"] = {"vcpus": 1, "memory_mib": 512, "disk_gib": 4}
        self.policy["environments"]["production"]["classes"] += ["larger-v1", "too-small-v1"]
        self.store = Store(self.root / "state")
        self.store.apply_policy(validate(self.policy), 0)
        self.backend, self.proxy = Backend(), Proxy()
        self.engine = Engine(self.store, self.backend, self.proxy)
        self.build = {"operation_id": "server-one", "host_id": "test-host", "configuration_revision": 1, "environment": "production", "name": "customer-one", "image": "base-v1", "class": "small-v1", "template": "web-v1", "network": "app", "release": "one"}
        self.engine.submit(vm.create_request(self.build), 1001)
        with patch("lens_infra.engine.time.sleep"):
            self.engine.execute("server-one")
        self.assertEqual(self.store.operation("server-one")["state"], "succeeded")
        self.resource = self.store.operation("server-one")["resources"][0]

    def tearDown(self):
        self.temp.cleanup()

    def selector(self, action, **extra):
        return {"operation_id": "vm-" + action, "instance_id": self.resource["id"], "host_id": "test-host", "configuration_revision": 1, "action": action, **extra}

    def action(self, action, **extra):
        selector = self.selector(action, **extra)
        vm.submit(self.engine, selector, 1001)
        with patch("lens_infra.engine.time.sleep"):
            self.engine.execute(selector["operation_id"])
        self.assertEqual(self.store.operation(selector["operation_id"])["state"], "succeeded")
        return selector

    def test_server_is_private_and_cannot_be_replaced_as_a_stateless_service(self):
        self.assertFalse(self.proxy.publications)
        req = vm.create_request(self.build | {"operation_id": "overwrite"})
        req.pop("mode")
        with self.assertRaisesRegex(Rejected, "persistent servers"):
            self.engine.submit(req, 1001)
        self.assertFalse(self.backend.removed)

    def test_shutdown_is_durable_and_recovery_does_not_boot_it(self):
        self.action("shutdown")
        self.engine.recover()
        self.assertFalse(self.backend.booted)
        self.assertEqual(self.store.resource(self.resource["id"])["detail"]["desired_power"], "off")

    def test_resize_requires_shutdown_and_supports_approved_smaller_class(self):
        with self.assertRaisesRegex(Rejected, "shut down"):
            vm.plan(self.engine, self.selector("resize", new_class="larger-v1"), 1001)
        self.action("shutdown")
        planned = vm.plan(self.engine, self.selector("resize", new_class="too-small-v1"), 1001)
        self.assertEqual(planned["resize_backup_retention_days"], 7)
        self.assertTrue(planned["filesystem_compatibility_checked_on_apply"])
        self.action("resize", new_class="larger-v1")
        row = self.store.resource(self.resource["id"])
        self.assertEqual((row["memory_mib"], row["disk_gib"]), (1024, 16))
        self.assertEqual(row["state"], "stopped")

    def test_lifecycle_refreshes_parked_capacity_and_shutdown_still_works_when_full(self):
        parked = {"id": "wf-runner:06", "name": "build", "kind": "vm", "state": "shut off", "vcpus": 16, "memory_mib": 16384, "disk_gib": 8}
        self.backend.observed = [parked]
        self.policy.update(revision=2, parked_resources=["wf-runner:06"])
        self.store.apply_policy(validate(self.policy), 0)
        self.engine.inventory()
        self.build["configuration_revision"] = 2
        original_selector = self.selector
        self.selector = lambda action, **extra: original_selector(action, **extra) | {"configuration_revision": 2}
        with self.store.connection() as connection:
            connection.execute("UPDATE resources SET observed_at=0 WHERE id='wf-runner:06'")
        self.action("shutdown")
        with self.store.connection() as connection:
            connection.execute("UPDATE resources SET observed_at=0 WHERE id='wf-runner:06'")
        self.action("resize", new_class="larger-v1")
        parked["state"] = "running"
        with self.assertRaisesRegex(Rejected, "parked VM"):
            vm.submit(self.engine, self.selector("boot"), 1001)
        # An over-budget observation must never prevent a graceful power-off.
        self.action("shutdown", operation_id="shutdown-again")

    def test_offline_backup_and_restore_create_a_different_powered_off_vm(self):
        with self.assertRaisesRegex(Rejected, "shut down"):
            vm.plan(self.engine, self.selector("backup"), 1001)
        self.action("shutdown")
        self.action("backup")
        backup = self.store.backup("vm-backup")
        self.assertEqual(backup["state"], "ready")
        selector = {"operation_id": "restored-one", "backup_id": backup["id"], "name": "restored-copy", "class": "small-v1", "network": "app", "host_id": "test-host", "configuration_revision": 1}
        with self.assertRaisesRegex(Rejected, "not authorised"):
            vm.restore_request(self.engine, selector, 1001)
        request = vm.restore_request(self.engine, selector, 1003)
        with self.assertRaisesRegex(Rejected, "not authorised"):
            self.engine.submit(request, 1001)
        self.engine.submit(request, 1003)
        self.engine.execute("restored-one")
        restored = self.store.operation("restored-one")["resources"][0]
        self.assertNotEqual(restored["id"], self.resource["id"])
        self.assertEqual(restored["state"], "stopped")
        self.assertFalse(self.backend.power[restored["id"]])
        self.assertFalse(self.backend.removed)

    def test_delete_requires_shutdown_and_keeps_disk_reservation(self):
        self.action("shutdown")
        self.action("delete")
        row = self.store.resource(self.resource["id"])
        self.assertEqual(row["state"], "removed")
        self.assertEqual(row["disk_gib"], 8)
        self.assertFalse(self.store.current_operations())

    def test_backups_continue_to_count_against_disk_budget(self):
        self.action("shutdown")
        self.action("backup")
        self.assertEqual(self.store.backup("vm-backup")["disk_gib"], 8)
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        p["capacity"]["disk_gib"] = 16
        p["environments"]["production"]["disk_gib"] = 16
        self.store.apply_policy(p, 0)
        with self.assertRaisesRegex(Rejected, "disk_gib capacity"):
            self.engine.submit(vm.create_request(self.build | {"operation_id": "another", "name": "another", "configuration_revision": 2}), 1001)

    def test_passwords_are_not_a_build_request_surface(self):
        with self.assertRaisesRegex(Rejected, "unknown fields"):
            vm.create_request(self.build | {"root_password": "do-not-store"})
        p = copy.deepcopy(self.policy)
        p["build_profiles"]["owner-v1"]["ssh_keys"] = []
        with self.assertRaisesRegex(Rejected, "administrator SSH keys"):
            deployment(p, vm.create_request(self.build))


if __name__ == "__main__":
    unittest.main()
