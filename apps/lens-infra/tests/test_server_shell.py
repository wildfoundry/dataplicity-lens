"""Real interactive Bash behavior, including hostile path/context data."""

import os
from pathlib import Path
import pty
import select
import subprocess
import sys
import tempfile
import time
import unittest


@unittest.skipUnless(sys.platform == "linux", "server profile targets Linux Bash 4+")
class ServerShellTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.context = self.root / "context"
        self.context.write_text("environment=production\nvm=api\nidentity=12345678-1234-1234-1234-123456789abc\nconfiguration_revision=7\n")
        source = Path(__file__).parents[1] / "lens_infra/server-shell.bash"
        self.rc = self.root / "bashrc"
        self.rc.write_text(source.read_text().replace("/etc/dataplicity/server-context", str(self.context)))
        self.env = os.environ | {"TERM": "dumb", "NO_COLOR": "1", "DP_SHELL_WELCOME_SHOWN": "1", "HOME": str(self.root), "COLUMNS": "80"}

    def shell(self, commands, cwd=None, env=None):
        master, slave = pty.openpty()
        proc = subprocess.Popen(["/bin/bash", "--noprofile", "--rcfile", str(self.rc), "-i"], stdin=slave, stdout=slave, stderr=slave,
                                cwd=cwd or self.root, env=env or self.env, start_new_session=True)
        os.close(slave)
        output = bytearray()
        os.write(master, commands.encode() + b"\nexit\n")
        deadline = time.monotonic() + 8
        try:
            while time.monotonic() < deadline:
                if select.select([master], [], [], .1)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    output.extend(data)
                elif proc.poll() is not None:
                    break
            if proc.poll() is None:
                proc.terminate()
            proc.wait(timeout=3)
        finally:
            os.close(master)
        return output.decode(errors="replace")

    def test_identity_and_failed_command_status_survive_prompt_hooks(self):
        self.rc.write_text('old_hook() { DP_PREVIOUS_HOOK=ran; }; PROMPT_COMMAND=old_hook\n' + self.rc.read_text())
        output = self.shell('false\nprintf "RESULT=%s HOOK=%s\\n" "$?" "$DP_PREVIOUS_HOOK"')
        self.assertIn("[PRODUCTION", output)
        self.assertIn("@api:12345678", output)
        self.assertIn("exit 1", output)
        self.assertIn("RESULT=1 HOOK=ran", output)
        self.assertNotIn("\x1b", output)

    def test_hostile_working_directory_is_displayed_without_execution_or_controls(self):
        directory = self.root / '$(touch${IFS}shell-injected)\x1b[31m'
        directory.mkdir()
        output = self.shell("true", cwd=directory)
        self.assertFalse((directory / "shell-injected").exists())
        self.assertNotIn("\x1b", output)
        self.assertIn("touch", output)

    def test_context_is_parsed_as_data_not_sourced(self):
        self.context.write_text(self.context.read_text() + "environment=$(touch${IFS}context-injected)\n")
        output = self.shell("true")
        self.assertFalse((self.root / "context-injected").exists())
        self.assertIn("[PRODUCTION", output)

    def test_noninteractive_sourcing_has_no_output(self):
        import shlex
        result = subprocess.run(["/bin/bash", "--noprofile", "--norc", "-c", ". " + shlex.quote(str(self.rc))], env=self.env, capture_output=True, timeout=3)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"")

    def test_opt_out_keeps_the_users_prompt(self):
        self.rc.write_text("DP_SHELL=off; PS1='my-own-prompt> '\n" + self.rc.read_text())
        output = self.shell("true")
        self.assertIn("my-own-prompt>", output)
        self.assertNotIn("[PRODUCTION", output)

    def test_narrow_terminal_and_disabled_prompt_expansion_remain_safe(self):
        self.rc.write_text("shopt -u promptvars\n" + self.rc.read_text())
        directory = self.root / '$(touch${IFS}shell-injected)\\e[31m'
        directory.mkdir()
        output = self.shell("true", cwd=directory, env=self.env | {"COLUMNS": "40"})
        self.assertIn("@api:1234", output)
        self.assertFalse((directory / "shell-injected").exists())
        self.assertNotIn("\x1b", output)


if __name__ == "__main__":
    unittest.main()
