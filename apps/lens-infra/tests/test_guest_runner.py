"""Exercise runner setup in a private filesystem with native commands intercepted."""

import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from lens_infra import guest


class GuestRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for directory in ("run/lens/secrets", "etc/systemd/system", "opt", "proc"):
            (self.root / directory).mkdir(parents=True, exist_ok=True)
        (self.root / "run/lens/secrets/registration").write_text("registration-sentinel")
        archive = self.root / "runner.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for filename in ("run.sh", "config.sh"):
                content = b"#!/bin/sh\nexit 0\n"
                info = tarfile.TarInfo(filename)
                info.size, info.mode = len(content), 0o755
                tar.addfile(info, io.BytesIO(content))
        self.manifest = {"kind": "github-runner", "instance": "li-test", "release": "one", "configuration_revision": 1,
                         "secret_generations": {"registration": 1}, "runner": {
                             "archive": str(archive), "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
                             "url": "https://github.com/example", "registration_secret": "registration", "labels": ["dedicated-build"], "group": "Build"}}
        self.registrations = []

    def tearDown(self):
        self.temp.cleanup()

    def mapped_path(self, value):
        path = Path(value)
        return path if path.is_relative_to(self.root) else self.root / str(path).lstrip("/")

    def command(self, argv, *, env=None, **kwargs):
        self.assertNotIn("registration-sentinel", " ".join(argv))
        if "/usr/sbin/runuser" == argv[0]:
            self.assertEqual(env["ACTIONS_RUNNER_INPUT_TOKEN"], "registration-sentinel")
            self.assertIn("--ephemeral", argv)
            self.assertIn("--no-default-labels", argv)
            self.assertIn("--disableupdate", argv)
            self.registrations.append(list(argv))
            (self.root / "opt/actions-runner/.runner").write_text("registered")

    def apply(self):
        with patch.object(guest, "Path", side_effect=self.mapped_path), patch.object(guest, "manifest", return_value=self.manifest), patch.object(guest, "firewall"), patch.object(guest, "command", side_effect=self.command), patch.object(guest.subprocess, "run", return_value=SimpleNamespace(returncode=0)):
            guest.apply()

    def test_registration_uses_environment_and_is_not_repeated_on_recovery(self):
        self.apply()
        self.apply()
        self.assertEqual(len(self.registrations), 1)
        unit = (self.root / "etc/systemd/system/lens-runner.service").read_text()
        self.assertNotIn("registration-sentinel", unit)
        self.assertIn("ConditionPathExists=!/opt/actions-runner/.lens-completed", unit)

    def test_completed_ephemeral_runner_cannot_register_again(self):
        self.apply()
        directory = self.root / "opt/actions-runner"
        (directory / ".lens-completed").touch()
        (directory / ".runner").unlink()
        self.apply()
        self.assertEqual(len(self.registrations), 1)

    def test_orphaned_listener_still_counts_as_accepting_jobs(self):
        proc = self.root / "proc/123"
        proc.mkdir()
        (proc / "comm").write_text("Runner.Listener\n")
        with patch.object(guest, "Path", side_effect=self.mapped_path), patch.object(guest, "manifest", return_value=self.manifest), patch.object(guest.subprocess, "run", return_value=SimpleNamespace(returncode=3)):
            status = guest.status()
        self.assertFalse(status["busy"])
        self.assertTrue(status["accepting_jobs"])


if __name__ == "__main__":
    unittest.main()
