"""Class catalogue reports blocked states before offering builds; admission still wins."""

import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from lens_infra.capacity import classes
from lens_infra.daemon import Server
from lens_infra.engine import Engine
from lens_infra.policy import Rejected, validate
from lens_infra.store import Store
from test_infrastructure import Backend, Proxy, policy, request
from test_vm_lifecycle import public_key


class ClassAvailabilityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.policy = policy(self.root)
        self.policy['classes']['large-v1'] = {'vcpus': 8, 'memory_mib': 8192, 'disk_gib': 64}
        self.policy['environments']['production']['classes'].append('large-v1')
        self.policy['build_profiles'] = {'admin-v1': {'ssh_keys': [public_key()]}}
        self.policy['templates']['web-v1']['build_profile'] = 'admin-v1'
        self.store = Store(self.root / 'state')
        self.store.apply_policy(validate(self.policy), 0)
        self.backend = Backend()
        self.engine = Engine(self.store, self.backend, Proxy())

    def query(self, uid=1001, **selector):
        return classes(self.engine, selector, uid)

    def observe(self, **values):
        self.backend.observed = [{'id': 'protected-build', 'kind': 'vm', 'name': 'build', 'state': 'running',
                                  'vcpus': 10, 'memory_mib': 12000, 'disk_gib': 100, **values}]

    def test_configured_class_can_be_unavailable_and_reports_concrete_shortfalls(self):
        self.observe()
        data = self.query()
        small, large = data['classes']
        self.assertTrue(small['available'])
        self.assertFalse(large['available'])
        reasons = {r['code']: r for r in large['reasons']}
        self.assertEqual(reasons['host_memory_mib_exhausted']['available'], 4384)
        self.assertEqual(reasons['host_memory_mib_exhausted']['shortfall'], 3808)
        self.assertIn('host_vcpus_exhausted', reasons)
        self.assertTrue(data['advisory'])
        self.assertFalse(self.backend.created)
        with self.store.connection() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM operations').fetchone()[0], 0)

    def test_stopped_vms_still_consume_reserved_capacity(self):
        self.observe(state='shut off')
        self.assertFalse(self.query()['classes'][1]['available'])

    def test_environment_quota_and_pending_builds_block_catalogue_and_admission(self):
        p = copy.deepcopy(self.policy)
        p.update(revision=2)
        p['environments']['production']['max_instances'] = 1
        self.store.apply_policy(validate(p), 0)
        self.engine.submit(request(configuration_revision=2), 1001)
        data = self.query()
        self.assertTrue(all(not c['available'] for c in data['classes']))
        self.assertTrue(any(r['code'] == 'environment_instances_exhausted' for r in data['classes'][0]['reasons']))
        with self.assertRaisesRegex(Rejected, 'instance quota exhausted'):
            self.engine.submit(request('another', configuration_revision=2, service='other'), 1001)

    def test_current_network_readiness_and_local_artifact_presence_are_checked(self):
        with patch.object(self.backend, 'verify_network', side_effect=Rejected('network unavailable')):
            self.assertTrue(all(any(r['code'] == 'network_not_ready' for r in c['reasons']) for c in self.query()['classes']))
        Path(self.policy['templates']['web-v1']['image']).unlink()
        self.assertTrue(all(any(r['code'] == 'artifact_unavailable' for r in c['reasons']) for c in self.query()['classes']))

    def test_reader_is_not_offered_builds_and_foreign_environment_is_rejected(self):
        self.assertTrue(all(not c['available'] for c in self.query(uid=1002)['classes']))
        self.assertFalse(self.query(uid=1002)['can_deploy'])
        with self.assertRaises(Rejected):
            self.query(environment='unowned')

    def test_missing_bootstrap_artifact_is_not_offered_as_a_buildable_class(self):
        p = copy.deepcopy(self.policy)
        p['revision'] = 2
        p['build_profiles']['with-files-v1'] = p['build_profiles']['admin-v1'] | {
            'files': [{'name': 'settings-v1', 'path': str(self.root/'missing.conf'), 'sha256': 'a'*64,
                       'destination': '/etc/example.conf'}]}
        p['templates']['with-files-v1'] = p['templates']['web-v1'] | {'build_profile': 'with-files-v1'}
        p['environments']['production']['templates'].append('with-files-v1')
        self.store.apply_policy(validate(p), 0)
        rows = self.query(template='with-files-v1')['classes']
        self.assertTrue(all(not row['available'] for row in rows))
        self.assertTrue(all(any(reason['code'] == 'artifact_unavailable' and 'bootstrap' in reason['message']
                                for reason in row['reasons']) for row in rows))

    def test_api_dispatch_and_repeat_queries_never_reserve_resources(self):
        server = Server(self.engine, self.root / 'unused.sock')
        one = server.dispatch({'method': 'classes'}, 1001)
        two = server.dispatch({'method': 'classes', 'payload': {'workload': 'service'}}, 1001)
        self.assertEqual(one['host_id'], 'test-host')
        self.assertTrue(all(c['available'] for c in two['classes']))
        self.assertEqual(self.store.inventory(), [])

    def test_green_snapshot_does_not_bypass_later_admission(self):
        self.assertTrue(self.query()['classes'][1]['available'])
        self.observe()
        with self.assertRaisesRegex(Rejected, 'capacity exhausted'):
            self.engine.submit(request(**{'class': 'large-v1'}), 1001)


if __name__ == '__main__':
    unittest.main()
