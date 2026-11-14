"""Mandatory offline recovery copies, durable failure and seven-day retention."""

import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

from lens_infra.engine import Engine
from lens_infra.policy import Rejected, validate
from lens_infra.resize import RETENTION_SECONDS, backup_id, expire
from lens_infra.store import Store
from lens_infra import vm
from test_infrastructure import Backend, Proxy, policy
from test_vm_lifecycle import public_key


class ResizeProtectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        p = policy(self.root)
        p['build_profiles'] = {'admin-v1': {'ssh_keys': [public_key()]}}
        p['templates']['web-v1']['build_profile'] = 'admin-v1'
        p['classes']['large-v1'] = {'vcpus': 2, 'memory_mib': 1024, 'disk_gib': 16}
        p['environments']['production']['classes'].append('large-v1')
        self.store = Store(self.root / 'state')
        self.store.apply_policy(validate(p), 0)
        self.backend = Backend()
        self.engine = Engine(self.store, self.backend, Proxy())
        build = {'operation_id': 'server-one', 'host_id': 'test-host', 'configuration_revision': 1,
                 'environment': 'production', 'name': 'server-one', 'image': 'base-v1', 'class': 'small-v1', 'template': 'web-v1', 'network': 'app', 'release': 'one'}
        self.engine.submit(vm.create_request(build), 1001)
        with patch('lens_infra.engine.time.sleep'):
            self.engine.execute('server-one')
        self.resource = self.store.operation('server-one')['resources'][0]
        self.execute('shutdown', 'shutdown-one')

    def selector(self, action, identity, **extra):
        return {'operation_id': identity, 'host_id': 'test-host', 'configuration_revision': 1,
                'instance_id': self.resource['id'], 'action': action, **extra}

    def execute(self, action, identity, **extra):
        vm.submit(self.engine, self.selector(action, identity, **extra), 1001)
        self.engine.execute(identity)
        return self.store.operation(identity)

    def resize(self, identity='resize-one'):
        return self.execute('resize', identity, new_class='large-v1')

    def test_checked_backup_precedes_resize_and_is_retained_for_seven_days(self):
        events = []
        original_backup, original_resize = self.backend.backup, self.backend.resize
        def backup(*args):
            events.append('backup')
            return original_backup(*args)
        def resize(*args):
            b = self.store.backup(backup_id('resize-one'))
            self.assertEqual(b['state'], 'ready')
            self.assertGreaterEqual(b['detail']['retain_until'] - b['detail']['ready_at'], RETENTION_SECONDS)
            events.append('resize')
            return original_resize(*args)
        with patch.object(self.backend, 'backup', side_effect=backup), patch.object(self.backend, 'resize', side_effect=resize):
            result = self.resize()
        self.assertEqual(result['state'], 'succeeded')
        self.assertEqual(events, ['backup', 'resize'])
        self.assertEqual(self.store.backup(backup_id('resize-one'))['detail']['workspace_gib'], 0)

    def test_backup_failure_prevents_any_resize_and_has_stable_failure_stage(self):
        with patch.object(self.backend, 'backup', side_effect=Rejected('backup failed')), patch.object(self.backend, 'resize') as resize:
            result = self.resize()
        self.assertFalse(resize.called)
        self.assertEqual((result['state'], result['stage']), ('failed', 'resize_failure'))
        self.assertIn('resize_failure:', result['error'])
        self.assertEqual(self.store.resource(self.resource['id'])['detail']['desired_power'], 'off')

    def test_filesystem_failure_preserves_ready_recovery_copy(self):
        with patch.object(self.backend, 'resize', side_effect=Rejected('resize_failure: incompatible_filesystem')):
            result = self.resize()
        self.assertEqual(result['stage'], 'resize_failure')
        b = self.store.backup(backup_id('resize-one'))
        self.assertEqual(b['state'], 'ready')
        self.assertTrue((self.store.root / 'backups' / b['id'] / 'root.qcow2').is_file())

    def test_resume_uses_the_original_checked_backup_without_overwriting_it(self):
        with patch.object(self.backend, 'resize', side_effect=Rejected('resize_failure: incompatible_filesystem')):
            self.resize()
        b = self.store.backup(backup_id('resize-one'))
        self.store.transition('resize-one', 'operator-checked', 'interrupted')
        with patch.object(self.backend, 'backup') as backup:
            self.engine.execute('resize-one')
        self.assertFalse(backup.called)
        self.assertEqual(self.store.operation('resize-one')['state'], 'succeeded')
        self.assertEqual(self.store.backup(b['id'])['sha256'], b['sha256'])

    def test_expiry_never_runs_early_and_never_deletes_manual_backups(self):
        self.resize()
        self.execute('backup', 'manual-backup')
        auto = self.store.backup(backup_id('resize-one'))
        deadline = auto['detail']['retain_until']
        self.assertEqual(expire(self.engine, deadline - .001), [])
        self.assertEqual(expire(self.engine, deadline + 1), [auto['id']])
        self.assertEqual(self.store.backup(auto['id'])['state'], 'expired')
        self.assertFalse((self.store.root / 'backups' / auto['id'] / 'root.qcow2').exists())
        self.assertEqual(self.store.backup('manual-backup')['state'], 'ready')
        self.assertTrue((self.store.root / 'backups/manual-backup/root.qcow2').is_file())
        self.assertEqual(expire(self.engine, deadline + 2), [])

    def test_tampered_expired_copy_is_not_deleted_or_uncharged(self):
        self.resize()
        b = self.store.backup(backup_id('resize-one'))
        path = self.store.root / 'backups' / b['id'] / 'root.qcow2'
        path.write_bytes(b'changed')
        with self.assertRaisesRegex(Rejected, 'changed'):
            expire(self.engine, b['detail']['retain_until'] + 1)
        self.assertTrue(path.exists())
        self.assertEqual(self.store.backup(b['id'])['state'], 'ready')

    def test_backup_plus_working_copy_capacity_is_reserved_before_acceptance(self):
        p = self.store.policy()
        p.update(revision=2)
        p['capacity']['disk_gib'] = 32
        p['environments']['production']['disk_gib'] = 32
        self.store.apply_policy(p, 0)
        selector = self.selector('resize', 'not-enough-working-space', new_class='large-v1') | {'configuration_revision': 2}
        # 16 target + 8 backup + 16 workspace exceeds 32; rollback the whole reservation.
        with self.assertRaisesRegex(Rejected, 'capacity exhausted'):
            vm.submit(self.engine, selector, 1001)
        self.assertEqual(self.store.resource(self.resource['id'])['disk_gib'], 8)
        self.assertEqual(self.store.backups(), [])


if __name__ == '__main__':
    unittest.main()
