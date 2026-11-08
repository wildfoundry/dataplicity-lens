"""Safety boundaries: existing inventory, authority, reservations and operation recovery."""

import copy
import hashlib
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import patch

from lens_infra.backend import Libvirt, allocations, parse_domain
from lens_infra.daemon import Server, read_frame
from lens_infra.engine import Engine
from lens_infra.ingress import Ingress, render
from lens_infra.policy import Rejected, authorize, deployment, validate
from lens_infra.store import Store


def policy(root):
    image = root / "base.qcow2"
    image.write_bytes(b"qualification fixture; not a bootable image")
    secret = root / "credential"
    secret.write_bytes(b"sentinel-do-not-report")
    secret.chmod(0o600)
    p = {"schema_version": 1, "revision": 1, "host_id": "test-host",
         "capacity": {"vcpus": 16, "memory_mib": 16384, "disk_gib": 256, "host_reserve_mib": 1024},
         "classes": {"small-v1": {"vcpus": 2, "memory_mib": 512, "disk_gib": 8}},
         "networks": {"app": {"environment": "production", "libvirt_network": "li-production", "cidr": "10.253.240.0/24", "mode": "isolated", "managed": True}},
         "environments": {"production": {"networks": ["app"], "classes": ["small-v1"], "templates": ["web-v1"],
                                         "max_instances": 8, "vcpus": 16, "memory_mib": 16384, "disk_gib": 256}},
         "templates": {"web-v1": {"kind": "appliance", "image": str(image), "image_sha256": hashlib.sha256(image.read_bytes()).hexdigest(),
                                   "profile": "fast-web", "port": 8000, "listen_port": 8080, "health_path": "/health", "startup_seconds": 5, "drain_seconds": 0,
                                   "secrets": ["db"]}},
         "secrets": {"db": {"path": str(secret), "encrypted": False, "generation": 1, "environments": ["production"]}},
         "principals": [{"uid": 1001, "role": "deployer", "environments": ["production"]},
                        {"uid": 1002, "role": "reader", "environments": ["production"]}]}
    return validate(p)


def request(operation="deploy-one", **changes):
    return {"operation_id": operation, "configuration_revision": 1, "host_id": "test-host", "environment": "production",
            "service": "api", "class": "small-v1", "template": "web-v1", "network": "app", "instances": 1,
            "release": "v-one", **changes}


class Backend:
    def __init__(self):
        self.observed = []
        self.created = []
        self.removed = []
        self.files = {}
        self.fail = False
        self.power = {}
        self.booted = []
        self.resized = []

    def inventory(self, policy=None):
        return self.observed

    def verify_network(self, network, policy=None):
        return {"verified": True}

    def assert_isolation(self, policy):
        pass

    def check_host_capacity(self, policy, resources):
        pass

    def prepare(self, template, profile=None):
        return {"verified": True}

    def create(self, resource, request, policy, start=True):
        if self.fail:
            raise Rejected("fixture VM failure")
        self.created.append(resource["id"])
        self.power[resource["id"]] = start

    def wait_agent(self, *args):
        pass

    def ensure_running(self, resource):
        self.power[resource["id"]] = True
        self.booted.append(resource["id"])

    def assert_owned(self, resource):
        if resource["owner"] != "lens" or resource["protected"]:
            raise Rejected("protected")

    def shutdown(self, resource):
        self.assert_owned(resource)
        self.power[resource["id"]] = False

    def require_stopped(self, resource):
        self.assert_owned(resource)
        if self.power.get(resource["id"], False):
            raise Rejected("VM must be shut down")

    def resize(self, resource, cls):
        self.require_stopped(resource)
        self.resized.append((resource["id"], cls))

    def backup(self, resource, destination):
        self.require_stopped(resource)
        destination.parent.mkdir(parents=True)
        destination.write_bytes(b"offline-disk-fixture")
        return hashlib.sha256(destination.read_bytes()).hexdigest()

    def guest_exec(self, *args, **kwargs):
        return b'{"healthy":true,"busy":false}'

    def guest_file(self, resource, destination, data):
        self.files[(resource["id"], destination)] = data

    def guest_artifact(self, resource, destination, source, checksum):
        data = Path(source).read_bytes()
        if hashlib.sha256(data).hexdigest() != checksum:
            raise Rejected("artifact digest mismatch")
        self.guest_file(resource, destination, data)

    def address(self, *args):
        return "10.253.240.10"

    def remove(self, resource):
        if resource["owner"] != "lens" or resource["protected"]:
            raise Rejected("protected")
        self.removed.append(resource["id"])


class Proxy:
    def __init__(self):
        self.publications = []

    def publish(self, request, template, backends, certificate=None):
        self.publications.append((request["operation_id"], backends))
        return {"published": True}

    def connections(self, *args, **kwargs):
        return 0


class InfrastructureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.policy = policy(self.root)
        self.store = Store(self.root / "state")
        self.store.apply_policy(self.policy, 0)
        self.backend = Backend()
        self.proxy = Proxy()
        self.engine = Engine(self.store, self.backend, self.proxy)

    def tearDown(self):
        self.temporary.cleanup()

    def test_unknown_vm_is_first_class_and_protected(self):
        self.backend.observed = [{"id": "existing", "name": "live-runner", "kind": "vm", "state": "running", "vcpus": 12, "memory_mib": 16000, "disk_gib": 100}]
        self.engine.inventory()
        row = self.store.inventory()[0]
        self.assertEqual((row["owner"], row["protected"]), ("external", 1))
        with self.assertRaisesRegex(Rejected, "capacity exhausted"):
            self.engine.submit(request(), 1001)
        self.assertFalse(self.backend.created)

    def test_unknown_container_is_protected_even_with_fake_metadata(self):
        self.store.observe([{"id": "docker:abc", "name": "customer-container", "kind": "container", "state": "running", "operation_id": "deploy-one"}])
        row = self.store.inventory()[0]
        self.assertEqual(row["protected"], 1)
        with self.assertRaises(Rejected):
            self.store.resource_state(row["id"], "removed")

    def test_absent_sample_does_not_release_protected_capacity(self):
        self.store.observe([{"id": "existing", "name": "live-runner", "kind": "vm", "state": "running", "vcpus": 16}])
        self.store.observe([])
        with self.assertRaises(Rejected):
            self.engine.submit(request(), 1001)

    def test_peer_cannot_claim_root_in_payload(self):
        server = Server(self.engine, self.root / "sock")
        with self.assertRaises(Rejected):
            server.dispatch({"method": "config.apply", "payload": self.policy}, 1001)
        with self.assertRaises(Rejected):
            server.dispatch({"method": "status", "uid": 0}, 9999)

    def test_reader_cannot_deploy(self):
        with self.assertRaises(Rejected):
            self.engine.submit(request(), 1002)

    def test_cross_environment_access_is_denied(self):
        p = copy.deepcopy(self.policy)
        p["environments"]["staging"] = copy.deepcopy(p["environments"]["production"])
        p["environments"]["staging"]["networks"] = ["app"]
        with self.assertRaisesRegex(Rejected, "ownership"):
            validate(p)
        with self.assertRaises(Rejected):
            authorize(self.policy, 1001, "deploy", "staging")

    def test_raw_resources_and_host_mounts_are_not_a_deployment_surface(self):
        for extra in ({"vcpus": 200}, {"mount": "/"}, {"privileged": True}, {"command": "cat /etc/shadow"}):
            with self.assertRaises(Rejected):
                deployment(self.policy, request(**extra))

    def test_config_revision_is_exact(self):
        with self.assertRaisesRegex(Rejected, "revision"):
            self.engine.submit(request(configuration_revision=2), 1001)

    def test_wrong_host_and_same_revision_different_digest_are_rejected(self):
        with self.assertRaisesRegex(Rejected, "different host"):
            self.engine.submit(request(host_id="another-host"), 1001)
        with self.assertRaisesRegex(Rejected, "digest"):
            self.engine.submit(request(configuration_digest="0" * 64), 1001)

    def test_prepared_artifact_uses_verified_snapshot_after_source_changes(self):
        backend = Libvirt(self.root / "backend-state")
        source = self.root / "artifact"
        source.write_bytes(b"approved-version")
        checksum = hashlib.sha256(source.read_bytes()).hexdigest()
        prepared = backend.artifact(str(source), checksum)
        source.write_bytes(b"different-version")
        self.assertEqual(prepared.read_bytes(), b"approved-version")
        self.assertEqual(backend.artifact(str(source), checksum).read_bytes(), b"approved-version")

    def test_wrong_artifact_digest_never_becomes_prepared(self):
        backend = Libvirt(self.root / "backend-state")
        with self.assertRaisesRegex(Rejected, "digest"):
            backend.artifact(self.policy["templates"]["web-v1"]["image"], "0" * 64)
        self.assertFalse((backend.root / "artifacts" / ("0" * 64)).exists())

    def test_idempotent_replay_does_not_reserve_twice(self):
        self.assertFalse(self.engine.submit(request(), 1001)["duplicate"])
        self.assertTrue(self.engine.submit(request(), 1001)["duplicate"])
        self.assertEqual(len(self.store.inventory()), 1)
        with self.assertRaisesRegex(Rejected, "different request"):
            self.engine.submit(request(instances=2), 1001)

    def test_atomic_service_lock(self):
        self.engine.submit(request(), 1001)
        with self.assertRaisesRegex(Rejected, "owns this service"):
            self.engine.submit(request("deploy-two"), 1001)

    def test_concurrent_reservation_cannot_exceed_host_budget(self):
        p = copy.deepcopy(self.policy)
        p["capacity"]["memory_mib"] = 512
        p["environments"]["production"]["memory_mib"] = 512
        p["revision"] = 2
        self.store.apply_policy(p, 0)
        # Exercise the transaction itself; each contender has already passed preflight.
        barrier = threading.Barrier(2)
        results = []
        def contender(n):
            r = request("deploy-" + n, service="service-" + n)
            barrier.wait()
            try:
                self.store.reserve(p, r, allocations(p, r), 1001)
                results.append("accepted")
            except Rejected:
                results.append("rejected")
        threads = [threading.Thread(target=contender, args=(v,)) for v in ("one", "two")]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(sorted(results), ["accepted", "rejected"])

    def test_duplicate_listener_between_services_is_rejected(self):
        self.engine.submit(request(), 1001)
        with self.assertRaisesRegex(Rejected, "listener"):
            self.engine.submit(request("another", service="another"), 1001)

    def test_ci_cannot_choose_arbitrary_listener(self):
        with self.assertRaisesRegex(Rejected, "listener"):
            self.engine.submit(request(listen_port=9090), 1001)

    def test_applied_definitions_are_immutable(self):
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        p["classes"]["small-v1"]["memory_mib"] = 1024
        with self.assertRaisesRegex(Rejected, "immutable"):
            self.store.apply_policy(p, 0)

    def test_restart_retains_reservations_and_marks_interrupted(self):
        self.engine.submit(request(), 1001)
        self.store.transition("deploy-one", "creating")
        restarted = Store(self.root / "state")
        self.assertEqual(restarted.interrupted(), ["deploy-one"])
        self.assertEqual(restarted.operation("deploy-one")["state"], "interrupted")
        self.assertEqual(len(restarted.inventory()), 1)

    def test_executor_lock_is_kernel_released_after_close(self):
        with self.store.exclusive():
            with self.assertRaises(Rejected):
                with self.store.exclusive():
                    self.fail("second executor obtained the lock")
        with self.store.exclusive():
            pass

    def test_failed_creation_keeps_old_inventory_and_reservations(self):
        self.backend.fail = True
        self.engine.submit(request(), 1001)
        self.engine.execute("deploy-one")
        self.assertEqual(self.store.operation("deploy-one")["state"], "failed")
        self.assertFalse(self.backend.removed)
        self.assertEqual(len(self.store.inventory()), 1)

    def test_local_delivery_has_no_plaintext_in_status_or_journal(self):
        self.engine.submit(request(), 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute("deploy-one")
        self.assertEqual(self.store.operation("deploy-one")["state"], "succeeded")
        delivered = next(v for (i, path), v in self.backend.files.items() if path.endswith("/secrets/db"))
        self.assertEqual(delivered, b"sentinel-do-not-report")
        public = json.dumps([self.store.operation("deploy-one"), self.store.events(), self.store.inventory()])
        self.assertNotIn("sentinel-do-not-report", public)
        self.assertNotIn("sentinel-do-not-report", (self.root / "state/state.sqlite3").read_bytes().decode(errors="ignore"))

    def test_secret_source_permissions_and_symlinks_are_rejected(self):
        path = self.root / "credential"
        path.chmod(0o644)
        with self.assertRaises(Rejected):
            self.engine.prepare(request(), 1001)
        path.chmod(0o600)
        link = self.root / "alias"
        link.symlink_to(path)
        with self.assertRaises(OSError):
            self.engine._trusted_file(str(link), 100)

    def test_unknown_vm_with_copied_metadata_is_not_adopted(self):
        self.store.observe([{"id": "not-reserved", "name": "li-forged", "kind": "vm", "state": "running", "operation_id": "deploy-one"}])
        self.assertEqual(self.store.inventory()[0]["owner"], "external")

    def test_domain_memory_counts_configured_reservation(self):
        xml = '<domain><name>live</name><uuid>1234</uuid><memory unit="KiB">8388608</memory><currentMemory unit="KiB">1048576</currentMemory><vcpu>6</vcpu><devices/></domain>'
        parsed = parse_domain(xml, "running")
        self.assertEqual((parsed["memory_mib"], parsed["vcpus"]), (8192, 6))

    def test_backend_refuses_removal_before_touching_external_vm(self):
        backend = Libvirt(self.root)
        with patch.object(backend, "virsh") as native:
            with self.assertRaisesRegex(Rejected, "protected"):
                backend.remove({"id": "existing", "owner": "external", "protected": 1})
            native.assert_not_called()

    def test_guest_secret_content_is_not_a_native_process_argument(self):
        backend = Libvirt(self.root)
        with patch.object(backend, "assert_owned"), patch("lens_infra.backend.native_agent", return_value='{"return":1}') as agent, patch.object(backend, "virsh") as native:
            backend.guest({"id": "1234"}, {"execute": "guest-file-write", "arguments": {"buf-b64": "sentinel"}})
            native.assert_not_called()
            self.assertIn("sentinel", agent.call_args.args[2])

    def test_proxy_profiles_have_distinct_tunnel_behavior_and_no_retries(self):
        t = self.policy["templates"]["web-v1"]
        fast = render(request(), t, [{"address": "10.253.240.2"}], "/run/control.sock")
        ws = render(request(), t | {"profile": "websocket"}, [{"address": "10.253.240.2"}], "/run/control.sock")
        self.assertNotIn("timeout tunnel", fast)
        self.assertIn("timeout tunnel 1h", ws)
        self.assertIn("retries 0", fast)

    def test_multi_registry_and_pinned_local_runner_sources(self):
        p = copy.deepcopy(self.policy)
        t = p["templates"]["web-v1"]
        t["kind"] = "oci"
        t["containers"] = [{"name": "app", "image": "ghcr.io/example/app@sha256:" + "a" * 64}]
        validate(p)
        t["containers"][0]["image"] = "docker.io/library/nginx:latest"
        with self.assertRaisesRegex(Rejected, "digest"):
            validate(p)

    def test_rpc_frames_reject_trailing_commands(self):
        a, b = socket.socketpair()
        with a, b:
            a.sendall(b'{"method":"status"}\n{"method":"config.apply"}\n')
            with self.assertRaises(Rejected):
                read_frame(b)

    def test_environment_service_names_cannot_alias_an_ingress_directory(self):
        proxy = Ingress(self.root)
        self.assertNotEqual(proxy.paths("a-b/c")[0], proxy.paths("a/b-c")[0])

    def test_secret_rotation_preserves_prior_generation_file(self):
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        p["secrets"]["db"]["generation"] = 2
        with self.assertRaisesRegex(Rejected, "versioned"):
            self.store.apply_policy(p, 0)

    def test_recovery_restores_only_recorded_managed_service(self):
        self.engine.submit(request(), 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute("deploy-one")
        self.store.observe([{"id": "existing", "name": "live-runner", "kind": "vm", "state": "running", "vcpus": 1}])
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.assertEqual(self.engine.recover(), ["deploy-one"])
        self.assertNotIn("existing", self.backend.created)
        self.assertFalse(self.backend.removed)

    def test_health_checks_remove_failed_backend_without_deleting_vm(self):
        self.engine.submit(request(), 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute("deploy-one")
        with patch.object(self.engine, "health", return_value=False):
            for _ in range(3):
                self.engine.health_tick()
        self.assertEqual(self.store.inventory()[0]["state"], "unready")
        self.assertEqual(self.proxy.publications[-1][1], [])
        self.assertFalse(self.backend.removed)

    def complete(self, req):
        self.engine.submit(req, 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute(req["operation_id"])
        self.assertEqual(self.store.operation(req["operation_id"])["state"], "succeeded")

    def test_inventory_preserves_managed_state_and_private_delivery_metadata(self):
        self.complete(request())
        resource = self.store.operation("deploy-one")["resources"][0]
        self.store.observe([resource | {"state": "running", "detail": {"networks": []}}])
        refreshed = self.store.operation("deploy-one")["resources"][0]
        self.assertEqual(refreshed["state"], "ready")
        self.assertEqual(refreshed["detail"]["address"], "10.253.240.10")

    def test_old_cleanup_failure_keeps_healthy_new_release(self):
        self.complete(request())
        with patch.object(self.backend, "remove", side_effect=Rejected("fixture cleanup unavailable")):
            self.complete(request("deploy-two", release="v-two"))
        self.assertEqual(self.store.service("production", "api")["current_id"], "deploy-two")
        self.assertEqual(self.store.operation("deploy-two")["stage"], "cleanup-pending")
        self.assertEqual(self.proxy.publications[-1][0], "deploy-two")

    def test_scale_drain_and_rollback_are_new_releases_with_a_current_release_guard(self):
        self.complete(request())
        selector = {"operation_id": "scale-two", "configuration_revision": 1, "host_id": "test-host", "environment": "production", "service": "api", "from_operation_id": "deploy-one", "instances": 2}
        plan = self.engine.service_change("scale", selector, 1001, plan=True)
        self.assertEqual(len(plan["resources"]), 2)
        self.engine.service_change("scale", selector, 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute("scale-two")
        with self.assertRaisesRegex(Rejected, "active release changed"):
            self.engine.service_change("drain", selector | {"operation_id": "stale-drain"}, 1001)
        result = self.engine.service_change("rollback", selector | {"operation_id": "rollback-one", "from_operation_id": "scale-two"}, 1001, plan=True)
        self.assertEqual(result["request"]["instances"], 1)
        result = self.engine.service_change("drain", selector | {"operation_id": "drain-all", "from_operation_id": "scale-two"}, 1001, plan=True)
        self.assertEqual(result["resources"], [])

    def test_cancelling_queued_operation_retains_every_reservation(self):
        self.engine.submit(request(), 1001)
        self.store.cancel("deploy-one", 1001)
        self.engine.execute("deploy-one")
        self.assertEqual(len(self.store.inventory()), 1)
        self.assertFalse(self.backend.created)
        self.assertFalse(self.backend.removed)

    def test_policy_change_between_plan_and_reservation_is_rejected(self):
        before = copy.deepcopy(self.policy)
        self.store.apply_policy(self.policy | {"revision": 2}, 0)
        with self.assertRaisesRegex(Rejected, "policy changed"):
            self.store.reserve(before, request(), allocations(before, request()), 1001)

    def test_unknown_container_capacity_fails_closed_until_reserved_by_administrator(self):
        self.store.observe([{"id": "docker:abc", "name": "live-build", "kind": "container", "state": "running"}])
        with self.assertRaisesRegex(Rejected, "explicit administrator capacity"):
            self.engine.submit(request(), 1001)
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        p["protected_resources"] = {"docker:abc": {"vcpus": 2, "memory_mib": 512, "disk_gib": 8}}
        self.store.apply_policy(validate(p), 0)
        self.engine.submit(request(configuration_revision=2), 1001)
        self.assertEqual(self.store.inventory()[0]["protected"], 1)

    def test_runner_cleanup_never_treats_idle_as_a_safe_removal_signal(self):
        self.complete(request())
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        runner = p["templates"]["runner-v1"] = {
            "kind": "github-runner", "image": p["templates"]["web-v1"]["image"], "image_sha256": p["templates"]["web-v1"]["image_sha256"], "secrets": ["db"],
            "runner": {"archive": "/approved/runner.tar.gz", "sha256": "a" * 64, "url": "https://github.com/example", "labels": ["lens-test"], "registration_secret": "db"}}
        p["environments"]["production"]["templates"].append("runner-v1")
        self.store.apply_policy(validate(p), 0)
        with self.store.connection() as c:
            original = request(template="runner-v1", configuration_revision=2)
            c.execute("UPDATE operations SET request=? WHERE id='deploy-one'", (json.dumps(original),))
            c.execute("DELETE FROM services")
        with patch.object(self.backend, "guest_exec", return_value=b'{"healthy":true,"busy":false,"accepting_jobs":true}'):
            self.engine.cleanup("deploy-one", 1001)
        self.assertFalse(self.backend.removed)
        self.assertEqual(self.store.operation("deploy-one")["resources"][0]["state"], "drain-pending")
        with patch.object(self.backend, "guest_exec", return_value=b'{"healthy":false,"busy":false,"accepting_jobs":false}'):
            self.engine.cleanup("deploy-one", 1001)
        self.assertEqual(len(self.backend.removed), 1)

    def test_failed_canary_restores_old_release_without_removing_either_vm(self):
        self.complete(request())
        self.engine.submit(request("deploy-two", release="v-two"), 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"), patch.object(self.engine, "observe", side_effect=Rejected("fixture failed observation")):
            self.engine.execute("deploy-two")
        self.assertEqual(self.store.operation("deploy-two")["state"], "failed")
        self.assertEqual(self.store.service("production", "api")["current_id"], "deploy-one")
        self.assertEqual(self.proxy.publications[-1][0], "deploy-one")
        self.assertFalse(self.backend.removed)

    def test_failed_initial_observation_closes_ingress(self):
        self.engine.submit(request(), 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"), patch.object(self.engine, "observe", side_effect=Rejected("fixture failed observation")):
            self.engine.execute("deploy-one")
        self.assertEqual(self.proxy.publications[-1][1], [])
        self.assertFalse(self.store.current_operations())

    def test_explicitly_parked_vm_releases_compute_only_while_freshly_verified_off(self):
        identity = "11111111-1111-4111-8111-111111111111"
        existing = {"id": identity, "name": "parked-build", "kind": "vm", "state": "shut off", "vcpus": 16, "memory_mib": 16384, "disk_gib": 200}
        self.backend.observed = [existing]
        with self.assertRaisesRegex(Rejected, "capacity exhausted"):
            self.engine.plan(request(), 1001)
        p = self.policy | {"revision": 2, "parked_resources": [identity]}
        self.store.apply_policy(validate(p), 0)
        self.engine.plan(request(configuration_revision=2), 1001)
        self.backend.observed = [existing | {"state": "running"}]
        with self.assertRaisesRegex(Rejected, "parked VM"):
            self.engine.plan(request(configuration_revision=2), 1001)
        self.assertFalse(self.backend.removed)

    def test_owned_disk_check_tolerates_libvirt_generated_index_but_rejects_other_files(self):
        backend = Libvirt(self.root)
        resource = {"id": "test", "name": "li-test", "operation_id": "deploy-one", "owner": "lens", "protected": False}
        xml = '<domain xmlns:lens="https://lens.dataplicity.com/infra/1"><name>li-test</name><uuid>test</uuid><metadata><lens:instance operation="deploy-one"/></metadata><devices><disk device="disk"><source file="/var/lib/libvirt/images/lens-infra/test/root.qcow2" index="2"/></disk></devices></domain>'
        with patch.object(backend, "virsh", return_value=xml.encode()):
            backend.assert_owned(resource)
        with patch.object(backend, "virsh", return_value=xml.replace("/test/root.qcow2", "/another/root.qcow2").encode()):
            with self.assertRaisesRegex(Rejected, "storage binding"):
                backend.assert_owned(resource)

    def test_versioned_build_profile_is_planned_and_delivered_from_local_artifacts(self):
        package = self.root / "lens.deb"
        package.write_bytes(b"verified-package-fixture")
        logging = self.root / "logging.conf"
        logging.write_bytes(b"# approved logging fixture\n")
        p = copy.deepcopy(self.policy)
        p["revision"] = 2
        p["build_profiles"] = {"agents-v1": {"lens": {"path": str(package), "sha256": hashlib.sha256(package.read_bytes()).hexdigest(), "version": "0.3.11"}, "logging": {"path": str(logging), "sha256": hashlib.sha256(logging.read_bytes()).hexdigest()}}}
        p["templates"]["agents-web-v1"] = p["templates"]["web-v1"] | {"build_profile": "agents-v1"}
        p["environments"]["production"]["templates"].append("agents-web-v1")
        self.store.apply_policy(validate(p), 0)
        req = request(configuration_revision=2, template="agents-web-v1")
        plan = self.engine.plan(req, 1001)
        self.assertEqual(plan["build_profile"]["lens_version"], "0.3.11")
        self.engine.submit(req, 1001)
        with patch.object(self.engine, "health", return_value=True), patch("lens_infra.engine.time.sleep"):
            self.engine.execute("deploy-one")
        self.assertEqual(self.store.operation("deploy-one")["state"], "succeeded")
        manifest = next(json.loads(v) for (_, path), v in self.backend.files.items() if path == "/run/lens/manifest.json")
        self.assertEqual(manifest["build_profile"]["name"], "agents-v1")
        self.assertEqual(manifest["build_profile"]["lens"]["path"], "/var/cache/lens/artifacts/lens.deb")
        p["revision"] = 3
        p["build_profiles"]["agents-v1"]["lens"]["version"] = "0.3.12"
        with self.assertRaisesRegex(Rejected, "immutable"):
            self.store.apply_policy(p, 0)


if __name__ == "__main__":
    unittest.main()
