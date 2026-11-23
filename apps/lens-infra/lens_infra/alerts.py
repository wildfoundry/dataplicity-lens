"""Local observation only: durable threshold state, never scaling or notification."""

import json
import math
import time
import uuid

from .policy import authorize, digest, require


def schema(store):
    with store.connection() as c:
        c.execute('CREATE TABLE IF NOT EXISTS alert_states(id TEXT PRIMARY KEY, environment TEXT NOT NULL, resource_id TEXT NOT NULL, rule_id TEXT NOT NULL, document TEXT NOT NULL)')


def evaluate(previous, rule, value, *, now, monotonic, epoch, max_gap, revision, reason=None):
    """Continuous windows use monotonic time and never bridge gaps/restarts."""
    previous = previous or {}
    state = dict(previous)
    fingerprint = digest(rule)
    continuous = (previous.get('epoch') == epoch and previous.get('rule_digest') == fingerprint
                  and 0 <= monotonic - previous.get('monotonic', -1e20) <= max_gap)
    if not continuous:
        state.update(pending_since=None, clear_since=None, pending_monotonic=None, clear_monotonic=None)
    state.update(rule_digest=fingerprint, epoch=epoch, monotonic=monotonic, observed_at=now,
                 threshold_revision=revision, value=value, threshold=rule['above'], reason=reason)
    active = bool(previous.get('active'))
    if value is None or not isinstance(value, (float, int)) or not math.isfinite(value):
        state.update(status='unknown', active=active, value=None, pending_since=None, clear_since=None,
                     pending_monotonic=None, clear_monotonic=None)
        return state
    if value > rule['above']:
        state.update(clear_since=None, clear_monotonic=None)
        if active:
            state.update(status='firing', active=True)
        else:
            if state.get('pending_monotonic') is None:
                state.update(pending_monotonic=monotonic, pending_since=now)
            fired = monotonic - state['pending_monotonic'] >= rule.get('for_seconds', 60)
            state.update(status='firing' if fired else 'pending', active=fired)
            if fired:
                state['firing_since'] = now
    elif active:
        state.update(pending_since=None, pending_monotonic=None)
        if value <= rule.get('clear_below', rule['above']):
            if state.get('clear_monotonic') is None:
                state.update(clear_monotonic=monotonic, clear_since=now)
            cleared = monotonic - state['clear_monotonic'] >= rule.get('clear_for_seconds', 30)
            state.update(status='resolved' if cleared else 'recovering', active=not cleared)
            if cleared:
                state['resolved_at'] = now
        else:
            state.update(status='firing', active=True, clear_since=None, clear_monotonic=None)
    else:
        state.update(status='normal', active=False, pending_since=None, pending_monotonic=None,
                     clear_since=None, clear_monotonic=None)
    return state


def allowed_rules(policy, uid):
    rows = []
    for rule in policy.get('monitoring', {}).get('rules', []):
        try:
            authorize(policy, uid, 'read', rule['environment'])
            rows.append(rule)
        except ValueError:
            pass
    return rows


def thresholds(engine, uid):
    p = engine.policy()
    authorize(p, uid, 'read')
    monitoring = p.get('monitoring', {})
    return {'host_id': p['host_id'], 'configuration_revision': p['revision'], 'mode': 'local',
            'notifications_enabled': False, 'automatic_actions_enabled': False,
            'sample_interval_seconds': monitoring.get('sample_interval_seconds', 10),
            'stale_after_seconds': monitoring.get('stale_after_seconds', 30), 'rules': allowed_rules(p, uid)}


def status(engine, uid):
    p = engine.policy()
    config = thresholds(engine, uid)
    schema(engine.store)
    rules = {r['name']: r for r in config['rules']}
    resources = {r['id']: r for r in engine.store.inventory()}
    rows, now = [], time.time()
    with engine.store.connection() as c:
        stored = [json.loads(r[0]) for r in c.execute('SELECT document FROM alert_states ORDER BY id')]
    for item in stored:
        if item['rule_id'] not in rules:
            continue
        rule = rules[item['rule_id']]
        # Rule names can be reused in a policy revision. Never expose old tenant
        # telemetry merely because a newly authorized rule has the same name.
        if (item['environment'], item['service'], item['container'], item['metric']) != (
                rule['environment'], rule['service'], rule.get('container', 'vm'), rule['metric']):
            continue
        resource = resources.get(item['resource_id'])
        if not resource or resource['state'] == 'removed' or resource['detail'].get('desired_power') == 'off':
            continue
        row = dict(item)
        if row.get('rule_digest') != digest(rule):
            row.update(status='unknown', reason='threshold_changed', value=None)
        elif row.get('epoch') != getattr(engine, '_alert_epoch', None) or now - row['observed_at'] > config['stale_after_seconds'] or now < row['observed_at']:
            row.update(status='unknown', reason='telemetry_stale', value=None)
        rows.append(row)
    observed = {r['rule_id'] for r in rows}
    for name, rule in rules.items():
        if name not in observed:
            rows.append({'rule_id': name, 'environment': rule['environment'], 'service': rule['service'],
                         'container': rule.get('container', 'vm'), 'metric': rule['metric'], 'status': 'unknown',
                         'active': False, 'reason': 'no_current_sample', 'threshold_revision': p['revision']})
    return {**config, 'alerts': rows}


def sample(engine):
    p = engine.policy()
    config = p.get('monitoring', {})
    rules = config.get('rules', [])
    if not rules:
        return
    now, mono = time.time(), time.monotonic()
    if mono - getattr(engine, '_last_alert_sample', -1e20) < config.get('sample_interval_seconds', 10):
        return
    engine._last_alert_sample = mono
    if not hasattr(engine, '_alert_epoch'):
        engine._alert_epoch = uuid.uuid4().hex
    schema(engine.store)
    resources = [r for r in engine.store.inventory() if r['owner'] == 'lens' and r['state'] == 'ready' and r['detail'].get('desired_power') != 'off']
    for resource in resources:
        selected = [r for r in rules if (r['environment'], r['service']) == (resource['environment'], resource['service'])]
        if not selected:
            continue
        metrics, telemetry_reason = {}, None
        if any(r['metric'] != 'endpoint_failure' for r in selected):
            try:
                metrics = json.loads(engine.backend.guest_exec(resource, ['/usr/bin/python3', '-I', '/opt/lens/workload.py', 'metrics'], timeout=20)).get('samples', {})
            except Exception:
                telemetry_reason = 'telemetry_unavailable'
        operation = engine.store.operation(resource['operation_id'])
        for rule in selected:
            value, reason = None, telemetry_reason
            container = rule.get('container', 'vm')
            if rule['metric'] == 'endpoint_failure':
                value, reason = probe(engine, resource, rule)
            else:
                reading = metrics.get(container, {})
                value = reading.get(rule['metric'])
                reason = None if value is not None else telemetry_reason or reading.get('reason', 'metric_unavailable')
            identity = resource['id'] + ':' + rule['name']
            with engine.store.connection() as c:
                row = c.execute('SELECT document FROM alert_states WHERE id=?', (identity,)).fetchone()
                previous = json.loads(row[0]) if row else None
                if rule['metric'] == 'cpu_percent' and container == 'vm':
                    reading = metrics.get('vm', {})
                    old = (previous or {}).get('raw', {})
                    delta = reading.get('total_ticks', 0) - old.get('total_ticks', 0)
                    busy = reading.get('cpu_ticks', 0) - old.get('cpu_ticks', 0)
                    if old and old.get('boot_id') == reading.get('boot_id') and delta > 0 and busy >= 0:
                        value, reason = busy / delta * 100, None
                    else:
                        value, reason = None, 'waiting_for_cpu_delta'
                state = evaluate(previous, rule, value, now=now, monotonic=mono, epoch=engine._alert_epoch,
                                 max_gap=config.get('stale_after_seconds', 30), revision=p['revision'], reason=reason)
                if rule['metric'] == 'cpu_percent' and container == 'vm':
                    state['raw'] = metrics.get('vm', {})
                state.update(id=identity, resource_id=resource['id'], rule_id=rule['name'], environment=resource['environment'],
                             service=resource['service'], container=container, metric=rule['metric'])
                c.execute('INSERT INTO alert_states VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document',
                          (identity, resource['environment'], resource['id'], rule['name'], json.dumps(state)))
                if not previous or (previous['status'], previous.get('reason')) != (state['status'], state.get('reason')):
                    engine.store.audit(c, operation['id'], 0, 'alert-state-changed',
                                       {'alert_id': identity, 'status': state['status'], 'metric': rule['metric'], 'reason': reason})


def probe(engine, resource, rule):
    import socket
    import urllib.error
    import urllib.request
    address = resource['detail'].get('address')
    if not address:
        return None, 'address_unavailable'
    from .engine import NoRedirect
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open('http://' + address + ':' + str(rule['port']) + rule.get('path', '/'), timeout=rule.get('timeout_seconds', 2)) as response:
            return (0, None) if response.status == rule.get('expected_status', 200) else (1, 'unexpected_http_status')
    except (TimeoutError, socket.timeout):
        return 1, 'endpoint_timeout'
    except urllib.error.HTTPError as exc:
        return (0, None) if exc.code == rule.get('expected_status', 200) else (1, 'unexpected_http_status')
    except urllib.error.URLError as exc:
        return 1, 'endpoint_timeout' if isinstance(exc.reason, (TimeoutError, socket.timeout)) else 'endpoint_unreachable'
    except Exception:
        return 1, 'endpoint_probe_failed'
