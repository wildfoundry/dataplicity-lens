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
        self.script.write_text(SOURCE.read_text().replace('/etc/', str(self.etc)+'/').replace('/var/lib/dpagent', str(self.root/'dpagent')).replace('/opt/dataplicity/', str(self.root/'dataplicity')+'/').replace('/sys/class/dmi/id/product_serial', str(self.root/'product_serial')))
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

    def test_fixed_system_serial_is_captured_only_by_the_unprivileged_writer(self):
        serial = 'EXAMPLE-SYSTEM'
        (self.root/'product_serial').write_text(serial + '\n')
        self.stub('runuser', '''case "$*" in
 *cache-system-serial*) test "$*" = '-u lens-collector -- /usr/bin/lens-hardware --cache-system-serial'; IFS= read -r value; printf '%s' "$value" > "$SETUP_TEST_SERIAL" ;;
 *) : ;;
 esac''')
        self.env['SETUP_TEST_SERIAL'] = str(self.root/'captured')
        result = self.run_setup('--no-syslog-upload')
        self.assertEqual((self.root/'captured').read_text(), serial)
        self.assertNotIn(serial, result.stdout + result.stderr)

    def test_invalid_or_unavailable_capture_does_not_disable_reporting_or_print_input(self):
        (self.root/'product_serial').write_text('EXAMPLE-PRIVATE\n')
        self.stub('runuser', 'case "$*" in *cache-system-serial*) cat >/dev/null; exit 1 ;; *) : ;; esac')
        result = self.run_setup('--no-syslog-upload')
        self.assertIn('system serial was not saved', result.stderr)
        self.assertNotIn('EXAMPLE-PRIVATE', result.stdout + result.stderr)
        self.assertIn('Lens reporting enabled.', result.stdout)

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

    def test_missing_tls_module_is_repaired_then_validated_without_exposing_output(self):
        marker=self.root/'tls-installed'
        self.env['SETUP_TEST_TLS_MARKER']=str(marker)
        self.stub('rsyslogd','test -f "$SETUP_TEST_TLS_MARKER" || { echo "could not load lmnsd_gtls test-secret-not-for-output" >&2; exit 1; }')
        self.stub('apt-get','test "$NEEDRESTART_MODE" = l; test "$DEBIAN_FRONTEND" = noninteractive; echo test-secret-not-for-output; touch "$SETUP_TEST_TLS_MARKER"')
        result=self.run_setup()
        self.assertTrue(marker.exists());self.assertTrue(self.conf.exists())
        self.assertIn('installing the system rsyslog TLS module',result.stdout)

    def test_dependency_failure_is_actionable_and_does_not_claim_logs_are_enabled(self):
        self.stub('rsyslogd','echo "could not load lmnsd_gtls test-secret-not-for-output" >&2; exit 1')
        self.stub('apt-get','echo test-secret-not-for-output >&2; exit 1')
        result=self.run_setup()
        self.assertFalse(self.conf.exists())
        self.assertIn('Install rsyslog-gnutls',result.stderr)
        self.assertIn('Lens logs are disabled',result.stderr)
        self.assertNotIn('TLS forwarding configured',result.stdout)

    def test_no_syslog_and_existing_forwarding_never_install_dependencies(self):
        marker=self.root/'unexpected-package-change'
        self.stub('apt-get',f'touch "{marker}"; exit 1')
        self.stub('rsyslogd','echo lmnsd_gtls >&2; exit 1')
        self.run_setup('--no-syslog-upload')
        self.stub('prelude-agent',':')
        self.run_setup()
        self.assertFalse(marker.exists())

    def test_failed_registration_restores_an_already_running_publisher(self):
        self.stub('systemctl','printf "%s\\n" "$*" >> "$SETUP_TEST_LOG"')
        self.stub('runuser','case "$*" in *status*) exit 1 ;; *register*) exit 1 ;; *) : ;; esac')
        result=subprocess.run(['bash',str(self.script)],env=self.env,input='example-code\n',capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('start lens-publisher.service',self.log.read_text())
        self.assertIn('previous publisher was restarted',result.stderr)

    def test_inactive_service_after_start_is_reported_as_failure(self):
        self.stub('systemctl','case "$*" in *is-active*lens-daemon*) exit 1 ;; *) : ;; esac')
        result=subprocess.run(['bash',str(self.script),'--no-syslog-upload'],env=self.env,capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertIn('lens-daemon.service is not active',result.stderr)
        self.assertNotIn('Lens reporting enabled.',result.stdout)

    def test_conflicting_flags_are_rejected(self):
        result=subprocess.run(['bash',str(self.script),'--no-syslog-upload','--syslog-upload'],env=self.env,capture_output=True,text=True)
        self.assertEqual(result.returncode,2)


if __name__ == '__main__':
    unittest.main()
