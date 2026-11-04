#!/usr/bin/env python3
"""Exercise root setup in a sandbox using fake service commands; never touches /etc."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'packaging/systemd/lens-reporting-enable'


class ReportingSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.etc = self.root / 'etc'
        (self.etc / 'rsyslog.d').mkdir(parents=True)
        (self.etc / 'ssl/certs').mkdir(parents=True)
        (self.etc / 'ssl/certs/ca-certificates.crt').write_text('test CA')
        self.script = self.root / 'setup'
        self.script.write_text(SOURCE.read_text().replace('/etc/', str(self.etc)+'/').replace('/var/lib/dpagent', str(self.root/'dpagent')).replace('/opt/dataplicity/', str(self.root/'dataplicity')+'/'))
        for name, body in {
            'id': 'echo 0', 'hostname':'echo server-test', 'systemd-sysusers':':',
            'systemd-tmpfiles':':', 'systemctl':':', 'rsyslogd':':',
            'runuser': '''case "$*" in
 *syslog-config*) printf '%s\\n' '# Managed by Dataplicity Lens. Credentials are logs-only; keep this file private.' 'test-secret-not-for-output' ;;
 *configure*) printf '%s\\n' "$*" >> "$SETUP_TEST_LOG" ;;
 *) : ;;
 esac''',
        }.items():
            self.stub(name, body)
        self.log = self.root/'commands'
        self.env = {**os.environ, 'PATH':str(self.bin)+':/usr/bin:/bin', 'SETUP_TEST_LOG':str(self.log)}
        self.conf = self.etc/'rsyslog.d/60-dataplicity-lens.conf'

    def stub(self, name, body):
        path=self.bin/name
        path.write_text('#!/bin/sh\n'+body+'\n')
        path.chmod(0o755)

    def run_setup(self, *args):
        result=subprocess.run(['bash',str(self.script),*args],env=self.env,capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertNotIn('test-secret-not-for-output',result.stdout+result.stderr)
        return result

    def test_standalone_configures_one_private_destination_and_is_idempotent(self):
        self.run_setup()
        self.assertTrue(self.conf.exists())
        self.assertEqual(self.conf.stat().st_mode & 0o777,0o600)
        self.run_setup()
        self.assertEqual(len(list((self.etc/'rsyslog.d').glob('*.conf'))),1)

    def test_agent_is_not_silently_given_a_second_log_path(self):
        self.stub('prelude-agent', ':')
        result=self.run_setup()
        self.assertFalse(self.conf.exists())
        self.assertIn('existing Agent',result.stdout)
        self.assertIn('--no-syslog-upload',self.log.read_text())

    def test_existing_forwarding_is_preserved_and_lens_does_not_duplicate_it(self):
        existing=self.etc/'rsyslog.d/10-existing.conf'
        existing.write_text('target="syslog.us.dvcmgr.net"')
        self.run_setup()
        self.assertFalse(self.conf.exists())
        self.assertEqual(existing.read_text(),'target="syslog.us.dvcmgr.net"')

    def test_explicit_override_is_visible(self):
        self.stub('prelude-agent', ':')
        result=self.run_setup('--syslog-upload')
        self.assertTrue(self.conf.exists())
        self.assertIn('explicit --syslog-upload override',result.stdout)

    def test_no_syslog_removes_only_owned_destination(self):
        self.run_setup()
        self.run_setup('--no-syslog-upload')
        self.assertFalse(self.conf.exists())

    def test_failed_tls_validation_leaves_forwarding_disabled_and_hides_credentials(self):
        self.stub('rsyslogd','echo test-secret-not-for-output >&2; exit 1')
        result=self.run_setup()
        self.assertFalse(self.conf.exists())
        self.assertIn('validation failed',result.stdout)

    def test_conflicting_flags_are_rejected(self):
        result=subprocess.run(['bash',str(self.script),'--no-syslog-upload','--syslog-upload'],env=self.env,capture_output=True,text=True)
        self.assertEqual(result.returncode,2)


if __name__ == '__main__':
    unittest.main()
