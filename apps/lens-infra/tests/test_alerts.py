"""Sustained local alert windows, stale state and passive-mode contracts."""

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from lens_infra import alerts
from lens_infra.daemon import Server
from lens_infra.engine import Engine
from lens_infra.policy import Rejected, validate
from lens_infra.store import Store
from test_infrastructure import Backend, Proxy, policy, request

RULE = {'name':'api-cpu','environment':'production','service':'api','container':'vm','metric':'cpu_percent',
        'above':80,'clear_below':60,'for_seconds':60,'clear_for_seconds':30}


class AlertWindowTests(unittest.TestCase):
    def step(self, previous, value, at, epoch='one', rule=None):
        return alerts.evaluate(previous, rule or RULE, value, now=1000+at, monotonic=at,
                               epoch=epoch, max_gap=20, revision=1)

    def test_single_spike_does_not_fire_but_continuous_window_does(self):
        state=self.step(None,90,0)
        self.assertEqual(state['status'],'pending')
        for at in range(10,60,10):state=self.step(state,90,at)
        self.assertFalse(state['active'])
        state=self.step(state,90,60)
        self.assertEqual((state['status'],state['active']),('firing',True))

    def test_hysteresis_and_recovery_duration_prevent_flapping(self):
        state=None
        for at in range(0,61,10):state=self.step(state,90,at)
        state=self.step(state,70,70)
        self.assertEqual(state['status'],'firing')
        state=self.step(state,50,80)
        self.assertEqual(state['status'],'recovering')
        for at in (90,100):state=self.step(state,50,at)
        self.assertTrue(state['active'])
        state=self.step(state,50,110)
        self.assertEqual((state['status'],state['active']),('resolved',False))

    def test_gap_restart_or_threshold_change_resets_pending_window(self):
        state=self.step(None,90,0)
        for at in (10,20,30,40,50):state=self.step(state,90,at)
        for kwargs in ({'at':90},{'at':60,'epoch':'new-process'},{'at':60,'rule':RULE|{'above':85}}):
            changed=self.step(state,95,**kwargs)
            self.assertEqual(changed['status'],'pending')
            self.assertFalse(changed['active'])

    def test_missing_data_is_unknown_and_does_not_resolve_existing_alert(self):
        state=None
        for at in range(0,61,10):state=self.step(state,90,at)
        state=self.step(state,None,70)
        self.assertEqual(state['status'],'unknown')
        self.assertTrue(state['active'])
        self.assertIsNone(state['value'])


class LocalAlertIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);p=policy(self.root)
        p['monitoring']={'sample_interval_seconds':10,'stale_after_seconds':30,'rules':[copy.deepcopy(RULE)]}
        self.store=Store(self.root/'state');self.store.apply_policy(validate(p),0)
        self.backend=Backend();self.engine=Engine(self.store,self.backend,Proxy())
        self.engine.submit(request(),1001)
        with patch.object(self.engine,'health',return_value=True),patch('lens_infra.engine.time.sleep'):
            self.engine.execute('deploy-one')
        self.resource=self.store.operation('deploy-one')['resources'][0]
        self.server=Server(self.engine,self.root/'unused.sock')

    def test_configuration_and_alert_queries_are_passive_and_preserve_revision(self):
        data=self.server.dispatch({'method':'thresholds'},1001)
        self.assertEqual(data['configuration_revision'],1)
        self.assertEqual(data['mode'],'local')
        self.assertFalse(data['notifications_enabled'])
        self.assertFalse(data['automatic_actions_enabled'])
        current=self.server.dispatch({'method':'alerts'},1001)
        self.assertEqual(current['alerts'][0]['status'],'unknown')
        self.assertEqual(self.server.jobs.qsize(),0)

    def test_query_cannot_change_thresholds_and_other_identities_cannot_read(self):
        with self.assertRaises(Rejected):self.server.dispatch({'method':'thresholds','payload':{'above':99}},1001)
        with self.assertRaises(Rejected):self.server.dispatch({'method':'thresholds'},99999)

    def test_real_sampling_tracks_vm_cpu_delta_and_persists_firing_without_action(self):
        self.engine._alert_epoch='fixed'
        for at in range(0,81,10):
            payload={'samples':{'vm':{'boot_id':'guest-boot','cpu_ticks':900+90*at,'total_ticks':1000+100*at,'memory_percent':25}}}
            with patch.object(self.backend,'guest_exec',return_value=json.dumps(payload).encode()),patch('lens_infra.alerts.time.monotonic',return_value=float(at)),patch('lens_infra.alerts.time.time',return_value=1000+at):
                alerts.sample(self.engine)
        with patch('lens_infra.alerts.time.time',return_value=1080):
            current=alerts.status(self.engine,1001)['alerts'][0]
        self.assertEqual(current['status'],'firing')
        self.assertAlmostEqual(current['value'],90)
        self.assertEqual(self.server.jobs.qsize(),0)
        self.assertFalse(self.backend.removed)
        with patch('lens_infra.alerts.time.time',return_value=1200):
            self.assertEqual(alerts.status(self.engine,1001)['alerts'][0]['status'],'unknown')
        self.engine._alert_epoch='restarted'
        with patch('lens_infra.alerts.time.time',return_value=1080):
            self.assertEqual(alerts.status(self.engine,1001)['alerts'][0]['status'],'unknown')

    def test_bad_monitoring_rules_fail_validation(self):
        for change in ({'above':float('nan')},{'above':-1},{'for_seconds':1},{'metric':'shell_command'},{'clear_below':99}):
            p=self.store.policy();p['monitoring']['rules'][0].update(change)
            with self.assertRaises(Rejected):validate(p)
        p=self.store.policy();p['monitoring']['rules'][0]=RULE|{'metric':'endpoint_failure','above':.5,'clear_below':0,'port':80,'path':'/health','timeout_seconds':2}
        validate(p)
        self.assertEqual(p['monitoring']['rules'][0]['port'],80)

    def test_changed_threshold_and_reused_rule_name_do_not_show_old_green_state(self):
        payload = {'samples': {'vm': {'memory_percent': 25}}}
        p = self.store.policy(); p['revision'] = 2
        p['monitoring']['rules'][0]['metric'] = 'memory_percent'
        self.store.apply_policy(validate(p), 0)
        with patch.object(self.backend, 'guest_exec', return_value=json.dumps(payload).encode()):
            alerts.sample(self.engine)
        self.assertEqual(alerts.status(self.engine,1001)['alerts'][0]['status'], 'normal')
        p = self.store.policy();p['revision'] = 3
        p['monitoring']['rules'][0]['above'] = 95
        self.store.apply_policy(validate(p), 0)
        changed = alerts.status(self.engine,1001)['alerts'][0]
        self.assertEqual((changed['status'],changed['reason']), ('unknown','threshold_changed'))
        p = self.store.policy();p['revision'] = 4
        p['monitoring']['rules'][0]['service'] = 'other-service'
        self.store.apply_policy(validate(p), 0)
        rows = alerts.status(self.engine,1001)['alerts']
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['service'], 'other-service')
        self.assertNotIn('resource_id', rows[0])
        self.assertEqual(rows[0]['reason'], 'no_current_sample')


if __name__=='__main__':unittest.main()
