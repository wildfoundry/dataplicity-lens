"""Explicit OS selection and non-repeating VM-only customization."""

import copy
import grp
import hashlib
import io
import json
import os
from pathlib import Path
import pwd
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
import unittest
from unittest.mock import patch

from lens_infra import bootstrap, vm
from lens_infra.__main__ import main
from lens_infra.engine import Engine
from lens_infra.daemon import Server
from lens_infra.policy import Rejected, deployment, validate
from lens_infra.store import Store
from test_infrastructure import Backend, Proxy, policy
from test_vm_lifecycle import public_key


class ImageSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.p = policy(self.root)
        self.p['build_profiles'] = {'admin-v1': {'ssh_keys': [public_key()]}}
        self.p['templates']['web-v1']['build_profile'] = 'admin-v1'
        self.build = {'operation_id':'explicit-one','host_id':'test-host','configuration_revision':1,'environment':'production',
                      'name':'generic-server','class':'small-v1','template':'web-v1','network':'app','release':'one'}

    def test_image_is_required_by_cli_and_api(self):
        with self.assertRaisesRegex(Rejected, 'explicit approved image'):
            vm.create_request(self.build)
        with patch('sys.stderr', io.StringIO()), self.assertRaises(SystemExit) as error:
            main(['vm','create','unused.toml','--plan'])
        self.assertEqual(error.exception.code, 2)

    def test_non_ubuntu_image_selected_by_id_resolves_its_own_pinned_bytes(self):
        image = self.root / 'other-linux.qcow2'; image.write_bytes(b'other approved image')
        self.p['images']['other-linux-v1'] = self.p['images']['base-v1'] | {'path':str(image),'sha256':hashlib.sha256(image.read_bytes()).hexdigest()}
        self.p['environments']['production']['images'].append('other-linux-v1')
        validate(self.p)
        req = vm.create_request(self.build | {'image':'other-linux-v1'})
        deployment(self.p, req)
        store = Store(self.root/'state'); store.apply_policy(self.p, 0)
        selected = Engine(store, Backend(), Proxy()).template(self.p, req)
        self.assertEqual(selected['image'], str(image))
        self.assertEqual(selected['image_sha256'], hashlib.sha256(image.read_bytes()).hexdigest())
        self.assertEqual(selected['profile'], 'private')

    def test_paths_unapproved_images_and_missing_capabilities_are_rejected(self):
        with self.assertRaises(Rejected): vm.create_request(self.build | {'image':'/etc/shadow'})
        with self.assertRaisesRegex(Rejected, 'not approved'):
            deployment(self.p, vm.create_request(self.build | {'image':'unknown-image'}))
        self.p['images']['base-v1']['capabilities'].remove('qemu-guest-agent')
        with self.assertRaisesRegex(Rejected, 'capabilities'):
            deployment(self.p, vm.create_request(self.build | {'image':'base-v1'}))

    def test_plain_server_uses_catalogue_without_application_or_web_endpoint(self):
        self.p['templates']['server-v1'] = {'kind': 'appliance', 'profile': 'private', 'build_profile': 'admin-v1'}
        self.p['environments']['production']['templates'].append('server-v1')
        validate(self.p)
        req = vm.create_request(self.build | {'image': 'base-v1', 'template': 'server-v1'})
        deployment(self.p, req)
        store = Store(self.root/'state');store.apply_policy(self.p, 0)
        engine = Engine(store, Backend(), Proxy())
        selected = engine.template(self.p, req)
        self.assertNotIn('containers', selected)
        self.assertNotIn('port', selected)
        self.assertNotIn('health_path', selected)
        self.assertTrue(engine.health('unused-private-address', selected))
        data = Server(engine, self.root/'unused.sock').dispatch({'method': 'images'}, 1001)
        self.assertEqual(data['images'][0]['image'], 'base-v1')
        self.assertNotIn('path', data['images'][0])
        with self.assertRaises(Rejected):
            vm.create_request(self.build | {'image': 'registry/example@sha256:' + 'a'*64})

    def test_catalogue_definitions_are_immutable_but_old_operations_do_not_require_new_fields(self):
        store = Store(self.root/'state');store.apply_policy(self.p,0)
        changed = copy.deepcopy(self.p);changed['revision']=2;changed['images']['base-v1']['sha256']='f'*64
        with self.assertRaisesRegex(Rejected,'immutable'): store.apply_policy(changed,0)
        # Legacy service history still resolves its original explicitly pinned disk.
        old = {'template':'web-v1'}
        self.assertEqual(Engine(store,Backend(),Proxy()).template(self.p,old)['image'],self.p['templates']['web-v1']['image'])


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.user = pwd.getpwuid(os.getuid()).pw_name
        self.group = grp.getgrgid(os.getgid()).gr_name

    def item(self,name,data):
        checksum=hashlib.sha256(data).hexdigest()
        source=self.root/'var/cache/lens/artifacts'/checksum;source.parent.mkdir(parents=True,exist_ok=True);source.write_bytes(data)
        return {'name':name,'path':'/var/cache/lens/artifacts/'+checksum,'sha256':checksum}

    def test_file_injection_checks_digest_permissions_and_drift(self):
        entry=self.item('config',b'nonsecret-config\n') | {'destination':'/etc/example.conf','owner':self.user,'group':self.group,'mode':'0600'}
        profile={'files':[entry]}
        self.assertEqual(bootstrap.apply(profile,self.root)['files'][0]['state'],'completed')
        target=self.root/'etc/example.conf'
        self.assertEqual(target.read_bytes(),b'nonsecret-config\n')
        self.assertEqual(target.stat().st_mode & 0o777,0o600)
        bootstrap.apply(profile,self.root)
        target.write_bytes(b'user edit')
        with self.assertRaisesRegex(RuntimeError,'drift'):bootstrap.apply(profile,self.root)
        self.assertEqual(target.read_bytes(),b'user edit')

    def test_symlink_in_destination_is_rejected_before_writing(self):
        elsewhere=self.root/'elsewhere';elsewhere.mkdir()
        (self.root/'etc').symlink_to(elsewhere,target_is_directory=True)
        entry=self.item('config',b'test')|{'destination':'/etc/example.conf','owner':self.user,'group':self.group}
        with self.assertRaisesRegex(RuntimeError,'symlink'):bootstrap.apply({'files':[entry]},self.root)
        self.assertFalse((elsewhere/'example.conf').exists())

    def test_successful_run_once_does_not_repeat_on_recovery(self):
        counter=self.root/'counter'
        script=('printf x >> '+str(counter)+'\n').encode()
        entry=self.item('initialise',script)
        one=bootstrap.apply({'run_once':[entry]},self.root)
        bootstrap.apply({'run_once':[entry]},self.root)
        self.assertEqual(counter.read_bytes(),b'x')
        self.assertEqual(one['run_once'][0]['exit_code'],0)
        marker=self.root/'var/lib/lens/bootstrap/steps/initialise.json'
        self.assertEqual(json.loads(marker.read_text())['state'],'completed')

    def test_failed_and_interrupted_attempts_never_silently_repeat(self):
        counter=self.root/'counter'
        entry=self.item('fails',('printf secret-for-private-log; printf x >> '+str(counter)+'; exit 7\n').encode())
        with self.assertRaisesRegex(RuntimeError,'bootstrap failed'):bootstrap.apply({'run_once':[entry]},self.root)
        with self.assertRaisesRegex(RuntimeError,'operator recovery'):bootstrap.apply({'run_once':[entry]},self.root)
        self.assertEqual(counter.read_bytes(),b'x')
        marker=self.root/'var/lib/lens/bootstrap/steps/fails.json'
        record=json.loads(marker.read_text());self.assertEqual(record['exit_code'],7)
        self.assertNotIn('secret-for-private-log',marker.read_text())
        log=self.root/'var/log/lens-bootstrap/fails.log'
        self.assertEqual(log.stat().st_mode & 0o777,0o600)
        self.assertIn(b'secret-for-private-log',log.read_bytes())
        record['state']='running';marker.write_text(json.dumps(record))
        with self.assertRaisesRegex(RuntimeError,'operator recovery'):bootstrap.apply({'run_once':[entry]},self.root)
        self.assertEqual(counter.read_bytes(),b'x')

    def test_bad_artifact_never_runs(self):
        entry=self.item('wrong',b'exit 0\n');entry['sha256']='a'*64
        with self.assertRaisesRegex(RuntimeError,'verification'):bootstrap.apply({'run_once':[entry]},self.root)
        self.assertFalse((self.root/'var/lib/lens/bootstrap/steps/wrong.json').exists())

    def test_concurrent_recovery_cannot_start_a_second_bootstrap_attempt(self):
        started, release, count = (self.root/name for name in ('started', 'release', 'count'))
        script = ('printf x >> '+str(count)+'\ntouch '+str(started)+'\nwhile [ ! -f '+str(release)+' ]; do sleep 0.05; done\n').encode()
        entry = self.item('concurrent', script) | {'timeout_seconds': 5}
        with ThreadPoolExecutor(max_workers=1) as pool:
            first = pool.submit(bootstrap.apply, {'run_once': [entry]}, self.root)
            try:
                deadline = time.monotonic() + 3
                while not started.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(started.exists(), 'first bootstrap did not start')
                with self.assertRaisesRegex(RuntimeError, 'already running'):
                    bootstrap.apply({'run_once': [entry]}, self.root)
            finally:
                release.touch()
            first.result(timeout=3)
        self.assertEqual(count.read_bytes(), b'x')


if __name__=='__main__':unittest.main()
