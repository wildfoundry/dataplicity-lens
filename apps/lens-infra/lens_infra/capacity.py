"""Current class availability, using the same durable reservations as admission."""

import time
import uuid
from pathlib import Path

from .backend import allocations
from .policy import Rejected, authorize, deployment, keys, require


def classes(engine, selector, uid):
    keys(selector, ['environment', 'network', 'template', 'instances', 'workload', 'image'], 'class availability')
    p = engine.policy()
    environment = selector.get('environment')
    if environment is None:
        allowed = []
        for name in p['environments']:
            try:
                authorize(p, uid, 'read', name)
                allowed.append(name)
            except Rejected:
                pass
        require(len(allowed) == 1, 'select an environment to query class availability')
        environment = allowed[0]
    authorize(p, uid, 'read', environment)
    require(environment in p['environments'], 'unknown environment')
    env = p['environments'][environment]
    def choice(field, allowed):
        value = selector.get(field)
        if value is None:
            require(len(allowed) == 1, 'select a ' + field + ' to query class availability')
            value = allowed[0]
        require(value in allowed, field + ' is not available in this environment')
        return value
    network = choice('network', env['networks'])
    template = choice('template', env['templates'])
    image_id = choice('image', env['images']) if env.get('images') else None
    workload = selector.get('workload', 'vm')
    require(workload in ('vm', 'service'), 'unknown availability workload')
    count = selector.get('instances', 1)
    require(type(count) is int and 1 <= count <= 10000, 'invalid availability instance count')
    can_deploy = True
    try:
        authorize(p, uid, 'deploy', environment)
    except Rejected:
        can_deploy = False
    # This is reconciliation, never an allocation or VM mutation.
    engine.inventory()
    common = []
    try:
        engine.backend.assert_isolation(p)
        engine.backend.verify_network(p['networks'][network], p)
    except Rejected as exc:
        common.append({'code': 'network_not_ready', 'message': str(exc)})
    selected = dict(p['templates'][template])
    if image_id:
        image = p['images'][image_id]
        selected.update(image=image['path'], image_sha256=image['sha256'], image_format=image.get('format', 'qcow2'))
    artifacts = [(selected['image'], 'VM image')]
    profile = p.get('build_profiles', {}).get(selected.get('build_profile'), {})
    artifacts += [(a['path'], 'build artifact') for a in profile.values() if isinstance(a, dict) and 'path' in a]
    for kind in ('files', 'run_once'):
        artifacts += [(item['path'], 'bootstrap artifact') for item in profile.get(kind, [])]
    artifacts += [(c['archive'], 'container archive') for c in selected.get('containers', []) if c.get('archive')]
    if selected.get('runner'):
        artifacts.append((selected['runner']['archive'], 'runner archive'))
    for source, label in artifacts:
        if not Path(source).is_file() or Path(source).is_symlink():
            common.append({'code': 'artifact_unavailable', 'message': label + ' is not available locally'})
    rows = []
    for name in env['classes']:
        operation = 'capacity-' + uuid.uuid4().hex
        request = {'operation_id': operation, 'configuration_revision': p['revision'], 'host_id': p['host_id'],
                   'environment': environment, 'service': operation, 'class': name, 'template': template,
                   'network': network, 'release': 'capacity-preview', 'instances': 1 if workload == 'vm' else count,
                   'mode': 'server' if workload == 'vm' else 'service'}
        if image_id:
            request['image'] = image_id
        if workload == 'service' and selected.get('profile', 'private') != 'private':
            request['listen_port'] = selected.get('listen_port', 8080)
        reasons = list(common)
        if workload == 'vm' and not image_id:
            reasons.append({'code': 'image_catalogue_unconfigured', 'message': 'VM creation requires an explicit approved image'})
        if not can_deploy:
            reasons.append({'code': 'deployment_not_authorized', 'message': 'this identity has read access but cannot deploy'})
        try:
            deployment(p, request)
            engine.template(p, request)
        except Rejected as exc:
            reasons.append({'code': 'build_not_allowed', 'message': str(exc)})
        resources = allocations(p, request | {"instances": count})
        report = engine.store.capacity_report(p, request, resources)
        reasons.extend(report['reasons'])
        if hasattr(engine.backend, 'host_capacity_report'):
            physical = engine.backend.host_capacity_report(p, resources)
            reasons.extend(physical['reasons'])
        else:
            physical = {'checks': [], 'reasons': []}
        try:
            # Also verify the pinned migration CPU model, using admission's check.
            engine.backend.check_host_capacity(p, resources)
        except Rejected as exc:
            if not physical['reasons']:
                reasons.append({'code': 'host_not_ready', 'message': str(exc)})
        rows.append({'class': name, **p['classes'][name], 'available': not reasons,
                     'reasons': reasons, 'checks': report['checks'] + physical['checks']})
    return {'host_id': p['host_id'], 'configuration_revision': p['revision'], 'environment': environment,
            'network': network, 'template': template, 'instances': count, 'workload': workload, 'image': image_id, 'observed_at': time.time(),
            'can_deploy': can_deploy, 'classes': rows, 'advisory': True,
            'notice': 'Current observation; build admission rechecks and reserves capacity transactionally.'}
