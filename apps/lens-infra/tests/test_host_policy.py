"""Administrative boundaries for human access and the software fabric."""

import copy
from pathlib import Path
import tempfile
import unittest

from lens_infra.policy import Rejected, deployment, validate
from lens_infra.engine import Engine
from lens_infra.store import Store
from lens_infra import vm
from test_infrastructure import Backend, Proxy, policy, request
from test_vm_lifecycle import public_key


class HostPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.p = policy(Path(self.temp.name))
        self.p["build_profiles"] = {"people-v1": {"users": [
            {"name": "alice", "ssh_keys": [public_key()], "sudo": True},
            {"name": "bob", "ssh_keys": [public_key()], "sudo": False}]}}
        self.p["templates"]["web-v1"]["build_profile"] = "people-v1"

    def fabric(self):
        self.p["fabric"] = {"name": "lab-fabric", "gateway_host": "test-host", "mtu": 1350,
            "protected_cidrs": ["192.168.50.0/24"], "members": [
                {"host_id": "test-host", "endpoint": "192.168.50.10", "node_id": 1},
                {"host_id": "other-host", "endpoint": "192.168.50.20", "node_id": 2}]}
        self.p["networks"]["app"].update(mode="vxlan", vni=12345)

    def test_named_admin_and_ordinary_user_are_allowed_for_server_builds(self):
        validate(self.p)
        build = vm.create_request({"operation_id": "server-one", "configuration_revision": 1,
            "host_id": "test-host", "environment": "production", "name": "customer-one",
            "class": "small-v1", "template": "web-v1", "network": "app", "release": "one", "image": "base-v1"})
        deployment(self.p, build)
        self.p["build_profiles"]["people-v1"]["users"][0]["sudo"] = False
        with self.assertRaisesRegex(Rejected, "administrator SSH keys"):
            deployment(self.p, build)

    def test_account_policy_has_no_password_shared_home_or_raw_mount_surface(self):
        for field in ("password", "homedir", "home_mount", "uid"):
            with self.subTest(field=field):
                p = copy.deepcopy(self.p)
                p["build_profiles"]["people-v1"]["users"][0][field] = "untrusted-input"
                with self.assertRaisesRegex(Rejected, "unknown fields"):
                    validate(p)

    def test_duplicate_and_root_usernames_are_rejected(self):
        for username in ("alice", "root"):
            p = copy.deepcopy(self.p)
            p["build_profiles"]["people-v1"]["users"][1]["name"] = username
            with self.assertRaisesRegex(Rejected, "reserved or duplicate"):
                validate(p)

    def test_private_keys_and_authorized_key_options_are_rejected(self):
        for key in ("-----BEGIN OPENSSH PRIVATE KEY-----", 'command="sh" ' + public_key()):
            p = copy.deepcopy(self.p)
            p["build_profiles"]["people-v1"]["users"][0]["ssh_keys"] = [key]
            with self.assertRaises(Rejected):
                validate(p)

    def test_fabric_rejects_public_and_unprotected_endpoints(self):
        self.fabric()
        validate(self.p)
        for endpoint in ("8.8.8.8", "10.50.0.2"):
            p = copy.deepcopy(self.p)
            p["fabric"]["members"][1]["endpoint"] = endpoint
            with self.assertRaises(Rejected):
                validate(p)

    def test_fabric_node_identity_and_vni_cannot_collide(self):
        self.fabric()
        p = copy.deepcopy(self.p)
        p["fabric"]["members"][1]["node_id"] = 1
        with self.assertRaisesRegex(Rejected, "unique"):
            validate(p)
        p = copy.deepcopy(self.p)
        p["networks"]["second"] = p["networks"]["app"] | {"libvirt_network": "li-second", "cidr": "10.253.241.0/24"}
        with self.assertRaisesRegex(Rejected, "network IDs must be distinct"):
            validate(p)

    def test_guest_subnet_cannot_overlap_protected_underlay(self):
        self.fabric()
        self.p["networks"]["app"]["cidr"] = "192.168.50.0/24"
        with self.assertRaisesRegex(Rejected, "overlap protected"):
            validate(self.p)

    def test_active_fabric_cannot_change_gateway_or_remove_peer(self):
        self.fabric()
        store = Store(Path(self.temp.name) / "state")
        store.apply_policy(validate(self.p), 0)
        Engine(store, Backend(), Proxy()).submit(request(), 1001)
        # Keep a reservation while allowing an explicit policy change after cancellation.
        store.cancel("deploy-one", 1001)
        for change in ("gateway", "remove-peer", "protected-ranges"):
            p = copy.deepcopy(self.p)
            p["revision"] = 2
            if change == "gateway":
                p["fabric"]["gateway_host"] = "other-host"
            elif change == "remove-peer":
                p["fabric"]["members"] = p["fabric"]["members"][:1]
            else:
                p["fabric"]["protected_cidrs"] = []
            with self.subTest(change=change), self.assertRaises(Rejected):
                store.apply_policy(p, 0)


if __name__ == "__main__":
    unittest.main()
