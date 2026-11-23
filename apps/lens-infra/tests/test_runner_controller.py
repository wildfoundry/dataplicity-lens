"""Safety extensions to the adopted per-job VM controller."""

import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from lens_infra import runner_controller as controller, runner_state
from lens_infra.policy import Rejected
from lens_infra.runner_provider import canonical_inventory


class RunnerControllerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.state_patch = patch.object(runner_state, "ROOT", self.root / "control")
        self.state_patch.start()
        self.identity = "11111111-1111-4111-8111-111111111111"

    def tearDown(self):
        self.state_patch.stop()
        self.temp.cleanup()

    def test_hold_persists_across_process_restart_and_blocks_recycling(self):
        runner_state.request(6, "hold", self.identity)
        self.assertTrue(runner_state.held(6))
        with patch.object(controller.sp, "run") as command:
            with self.assertRaisesRegex(Rejected, "must be preserved"):
                controller.destroy(6)
            command.assert_not_called()
        with patch.object(controller, "boot_lock") as lock, patch.object(controller.os.path, "ismount", return_value=True), patch.object(controller.shutil, "disk_usage", return_value=SimpleNamespace(free=100 * 2**30)):
            lock.return_value.__enter__.return_value = None
            with patch.object(controller, "destroy") as destroy:
                with self.assertRaisesRegex(Rejected, "held"):
                    controller.prepare(6)
                destroy.assert_not_called()

    def test_resume_requires_exact_held_generation(self):
        intent = runner_state.request(6, "hold", self.identity)
        with self.assertRaisesRegex(Rejected, "exact held"):
            runner_state.request(6, "resume", self.identity)
        runner_state.update(6, intent, "held")
        resumed = runner_state.request(6, "resume", self.identity)
        self.assertEqual(resumed["generation"], 2)
        with self.assertRaisesRegex(Rejected, "intent changed"):
            runner_state.update(6, intent, "held")

    def test_busy_runner_is_never_shut_down_or_destroyed(self):
        intent = runner_state.request(6, "hold", self.identity)
        with patch.object(controller, "slot_info", return_value=("lab-06", self.root, "10.0.0.16")), patch.object(controller.sp, "check_output", side_effect=[self.identity, "running"]), patch.object(controller, "stop_idle_admission", return_value={"busy": True}), patch.object(controller, "run") as command:
            self.assertFalse(controller.park(6))
            command.assert_not_called()
        self.assertEqual(runner_state.status(6)["stage"], "waiting-for-job")

    def test_already_stopped_runner_is_held_without_disk_mutation(self):
        runner_state.request(6, "hold", self.identity)
        disk = self.root / "disk.qcow2"
        disk.write_bytes(b"preserved-existing-disk")
        with patch.object(controller, "slot_info", return_value=("lab-06", self.root, "10.0.0.16")), patch.object(controller.sp, "check_output", side_effect=[self.identity, "shut off"]), patch.object(controller, "run") as command:
            self.assertTrue(controller.park(6))
            command.assert_not_called()
        self.assertEqual(disk.read_bytes(), b"preserved-existing-disk")
        self.assertEqual(runner_state.status(6)["stage"], "held")

    def test_stable_slot_inventory_retains_runtime_uuid_and_requires_storage_binding(self):
        p = {"runner_controller": {"host_prefix": "lab", "max_slots": 14, "control_slots": [6]}}
        item = {"id": self.identity, "name": "lab-06", "kind": "vm", "detail": {"disks": [{"file": "/data/github-runner/vms/lab-06/disk.qcow2"}]}}
        result = canonical_inventory(p, [item])[0]
        self.assertEqual(result["id"], "wf-runner:06")
        self.assertEqual(result["detail"]["vm_uuid"], self.identity)
        result["detail"]["disks"][0]["file"] = "/someone-elses-disk"
        with self.assertRaisesRegex(Rejected, "storage binding"):
            canonical_inventory(p, [result])

    def test_preserved_resume_starts_same_vm_without_preparing_new_disk(self):
        intent = runner_state.request(6, "hold", self.identity)
        runner_state.update(6, intent, "held")
        intent = runner_state.request(6, "resume", self.identity)
        (self.root / "generation.json").write_text(json.dumps({"generation": "existing-generation"}))
        xml = '<domain><memory unit="KiB">10485760</memory></domain>'
        with patch.object(controller, "slot_info", return_value=("lab-06", self.root, "10.0.0.16")), patch.object(controller, "boot_lock"), patch.object(controller.sp, "check_output", side_effect=[self.identity, xml, "shut off"]), patch.object(controller.sp, "run", return_value=SimpleNamespace(returncode=0)), patch.object(controller, "ssh_args", return_value=["ssh"]), patch.object(controller, "admission") as admission, patch.object(controller, "run") as command:
            self.assertEqual(controller.resume_existing(6, intent), "existing-generation")
            admission.assert_called_once_with(10240, replacing_slot=6)
            command.assert_called_once_with(["virsh", "start", "lab-06"], stdout=controller.sp.DEVNULL)


if __name__ == "__main__":
    unittest.main()
