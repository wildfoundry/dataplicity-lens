"""Human views and the CLI's separate machine contracts, including hostile data."""

import contextlib
import io
import json
import os
import unittest
from unittest.mock import patch

from lens_infra.__main__ import cli
from lens_infra.display import View, cells, render
from lens_infra.policy import Rejected


INSTANCE = "d03cde77-d33f-5bfd-bbf8-4b1a07e91105"
RESOURCE = {"id": INSTANCE, "name": "li-fixture", "service": "api", "state": "ready", "protected": False,
            "vcpus": 2, "memory_mib": 1536, "disk_gib": 12, "detail": {"address": "10.1.2.3", "secret": "must-not-render"}}
STATUS = {"host_id": "test-host", "configuration_revision": 7, "resources": [RESOURCE, RESOURCE | {"id": "external", "protected": True}],
          "services": [{"id": "production/api", "applied_release": "release-23", "applied_configuration_revision": 7, "desired_instances": 1, "current_id": "release-api-23"}]}


class DisplayTests(unittest.TestCase):
    def test_status_is_readable_and_distinguishes_protected_workloads(self):
        text = render("status", None, STATUS, width=100, unicode=True, color=False)
        for value in ("DATAPLICITY / LENS", "test-host", "configuration 7", "production/api", "release-23", "1.5 GiB", INSTANCE, "1 managed", "1 protected"):
            self.assertIn(value, text)
        self.assertNotIn("must-not-render", text)
        self.assertNotIn('"schema_version"', text)

    def test_narrow_wide_and_ascii_views_fit_and_preserve_selectors(self):
        long_name = "server-" + "界" * 25
        data = STATUS | {"resources": [RESOURCE | {"service": long_name}]}
        for width in (24, 40, 64, 80, 100, 160):
            text = render("status", None, data, width=width, unicode=True, color=False)
            self.assertTrue(all(cells(line) <= width for line in text.splitlines()), (width, text))
            compact = "".join(text.split())
            self.assertIn(INSTANCE, compact)
            self.assertIn(long_name, compact)
        text = render("status", None, data, width=80, unicode=False, color=False)
        self.assertTrue(text.isascii())

    def test_untrusted_output_cannot_write_terminal_controls(self):
        hostile = "api\x1b]52;c;ZXZpbA==\x07\r\n\u202ehidden"
        text = render("operation", "status", {"id": "one", "state": "failed", "stage": "failed", "error": hostile,
                     "request": {"operation_id": "one", "service": hostile}}, width=100, color=False)
        for c in ("\x1b", "\x07", "\r", "\u202e"):
            self.assertNotIn(c, text)
        self.assertIn("FAILED", text)
        self.assertIn("Needs attention", text)

    def test_no_color_and_pipes_have_no_ansi_while_tty_uses_lens_palette(self):
        stream = io.StringIO()
        with patch.dict(os.environ, {"TERM": "xterm-256color"}, clear=True):
            self.assertFalse(View(stream).color)
            with patch.object(stream, "isatty", return_value=True):
                self.assertTrue(View(stream).color)
                self.assertIn("\x1b[1;35m", render("status", None, STATUS, stream=stream))
                with patch.dict(os.environ, {"NO_COLOR": ""}):
                    self.assertNotIn("\x1b", render("status", None, STATUS, stream=stream))
        with patch.dict(os.environ, {"TERM": "dumb"}):
            text = render("status", None, STATUS, stream=stream)
            self.assertTrue(text.isascii())

    def test_disk_action_plan_identifies_exact_target_and_offline_requirement(self):
        plan = {"request": {"operation_id": "resize-one", "instance_id": INSTANCE, "new_class": "large-v2"},
                "instance_id": INSTANCE, "action": "resize", "planned_shutdown_required": True}
        text = render("vm", "resize", plan, planned=True, width=100, color=False)
        self.assertIn(INSTANCE, text)
        self.assertIn("large-v2", text)
        self.assertIn("explicitly shut down", text)
        self.assertIn("no changes applied", text)

    def test_supported_result_shapes_show_relevant_information_without_raw_payloads(self):
        cases = [
            ("operation", "resume", {"id": "resume-one", "queued": True}, "Follow:"),
            ("vm", "backup", {"id": "backup-one", "state": "accepted", "duplicate": False}, "execution continues"),
            ("vm", "backups", [{"id": "backup-one", "state": "ready", "resource_id": INSTANCE, "disk_gib": 12, "created_at": 0}], "1970-01-01"),
            ("runner", "status", {"slot": 6, "mode": "hold", "stage": "held", "expected_uuid": INSTANCE}, "held"),
            ("runner", "status", None, "No local intent"),
            ("config", "validate", {"valid": True, "revision": 7, "host_id": "test-host"}, "Valid"),
            ("network", "plan", {"networks": [{"network": "app", "action": "create", "bridge": "bridge1", "cidr": "10.1.2.0/24"}], "firewall": "DO-NOT-DUMP"}, "10.1.2.0/24"),
            ("events", None, [{"timestamp": 0, "actor": 65001, "operation_id": "one", "event": "accepted", "detail": {"private": "DO-NOT-DUMP"}}], "65001"),
            ("deploy", "plan", {"request": {"operation_id": "build-one", "host_id": "test-host", "service": "api", "template": "ubuntu-v1"}, "resources": [RESOURCE], "strategy": "replacement"}, "Plan only"),
        ]
        for command, action, data, expected in cases:
            with self.subTest(command=command, action=action):
                text = render(command, action, data, width=100, color=False, unicode=True)
                self.assertIn(expected, text)
                self.assertNotIn("DO-NOT-DUMP", text)
                self.assertNotIn("must-not-render", text)


class CliOutputTests(unittest.TestCase):
    def run_cli(self, argv, result=None, error=None, stdin=None):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), patch("lens_infra.__main__.request", return_value=result, side_effect=error):
            if stdin is not None:
                with patch("sys.stdin", io.TextIOWrapper(io.BytesIO(stdin.encode()))):
                    code = cli(argv)
            else:
                code = cli(argv)
        return code, out.getvalue(), err.getvalue()

    def test_default_is_human_json_is_opt_in_in_either_position(self):
        code, text, err = self.run_cli(["status"], STATUS)
        self.assertEqual(code, 0)
        self.assertIn("DATAPLICITY / LENS", text)
        self.assertEqual(err, "")
        for argv in (["--json", "status"], ["status", "--json"], ["operation", "--json", "status", "one"], ["operation", "status", "one", "--json"]):
            code, text, err = self.run_cli(argv, STATUS)
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(text), {"schema_version": 1, "result": STATUS})
            self.assertNotIn("\x1b", text)

    def test_rpc_always_preserves_json_success_and_error(self):
        code, text, err = self.run_cli(["rpc"], STATUS, stdin='{"method":"status"}\n')
        self.assertEqual(json.loads(text), {"schema_version": 1, "result": STATUS})
        self.assertEqual(code, 0)
        code, text, err = self.run_cli(["rpc"], error=Rejected("not authorized"), stdin='{"method":"status"}\n')
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(err), {"schema_version": 1, "error": "not authorized"})
        self.assertEqual(text, "")

    def test_human_errors_and_explicit_json_errors_keep_failure_exit_status(self):
        for flags in ([], ["--json"]):
            code, text, err = self.run_cli(["status", *flags], error=Rejected("quota exhausted"))
            self.assertEqual((code, text), (1, ""))
            if flags:
                self.assertEqual(json.loads(err), {"schema_version": 1, "error": "quota exhausted"})
            else:
                self.assertIn("Request could not be completed", err)
                self.assertIn("quota exhausted", err)

    def test_metrics_remain_prometheus_text_and_empty_lists_are_clear(self):
        metric = "# TYPE lens_infra_configuration_revision gauge\nlens_infra_configuration_revision 1\n"
        for flags in ([], ["--json"]):
            code, text, err = self.run_cli(["metrics", *flags], metric)
            self.assertEqual(text, metric)
        for command, action in (("vm", "backups"), ("inventory", None), ("events", None)):
            self.assertIn("None.", render(command, action, [], color=False))


if __name__ == "__main__":
    unittest.main()
