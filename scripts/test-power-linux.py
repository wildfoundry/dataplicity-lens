#!/usr/bin/env python3
"""Fixed BMC sample cache interoperability on a disposable Linux CI machine."""
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    if sys.platform != 'linux' or os.geteuid() != 0 or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise SystemExit('Requires a disposable Linux CI root fixture.')
    root = Path('/run/lens-power')
    if root.exists() or root.is_symlink():
        raise SystemExit('REFUSED: existing power sampler state must not be replaced.')
    binary = Path(sys.argv[1]).resolve() / 'lens-hardware'
    root.mkdir(mode=0o755)
    sample = root / 'current.json'
    now = lambda offset=0: (datetime.now(timezone.utc) + timedelta(seconds=offset)).isoformat()
    def write(**changes):
        sample.unlink(missing_ok=True)
        value = {'watts': 182, 'source': 'local_bmc_dcmi', 'observed_at': now()}; value.update(changes)
        sample.write_text(json.dumps({'observed_at': value['observed_at'], 'power': value, 'supplies': []})); sample.chmod(0o644)
    def observe_server():
        result = subprocess.run(['/usr/sbin/runuser', '-u', 'lens-collector', '--', str(binary), '--once', '--json', '--fields', 'hardware'],
                                capture_output=True, timeout=20, check=True)
        return json.loads(result.stdout).get('hardware', {}).get('server') or {}
    def observe():
        return observe_server().get('power')
    try:
        write()
        assert observe()['watts'] == 182
        denied = subprocess.run(['/usr/sbin/runuser', '-u', 'lens-collector', '--', str(binary), '--sample-power'],
                                capture_output=True, timeout=10)
        assert denied.returncode != 0 and json.loads(sample.read_text())['power']['watts'] == 182
        for changes in ({'observed_at': now(-61)}, {'observed_at': now(120)}, {'watts': 0}, {'watts': 65535}, {'source': 'estimated'}):
            write(**changes); assert observe() is None
        write(); sample.chmod(0o666); assert observe() is None
        write(); retained = root / 'retained.json'; sample.rename(retained); sample.symlink_to(retained)
        assert observe() is None
        sample.unlink(); retained.unlink()
        # Supply observations must survive without a DCMI total, and cannot
        # bypass freshness, size or source validation through the side cache.
        at = now()
        supply = {'id': 'ipmi:10.1', 'name': 'PS1', 'present': True, 'health': 'healthy', 'states': ['Present'],
                  'watts': None, 'input_watts': 91, 'output_watts': None, 'current_milliamps': 400,
                  'voltage_millivolts': 236000, 'observed_at': at, 'source': 'local_bmc_ipmi'}
        def write_supplies(values):
            sample.write_text(json.dumps({'observed_at': at, 'power': None, 'supplies': values})); sample.chmod(0o644)
        write_supplies([supply])
        server = observe_server()
        assert server.get('power') is None and server['power_supplies'][0]['input_watts'] == 91
        for values in ([dict(supply, source='estimated')], [dict(supply, observed_at=now(-61))], [supply, supply], [dict(supply, id=f'ipmi:10.{n}') for n in range(33)]):
            write_supplies(values); assert not observe_server().get('power_supplies')
        subprocess.run([str(binary), '--sample-power'], capture_output=True, check=True, timeout=10)
        assert sample.stat().st_uid == 0 and sample.stat().st_mode & 0o022 == 0
        value = json.loads(sample.read_text())
        assert set(value) == {'power', 'observed_at', 'supplies'}
        invalid = subprocess.run([str(binary), '--sample-power', '--json'], capture_output=True, timeout=10)
        assert invalid.returncode != 0
        print('Power cache: unprivileged read, protected writer, stale/future/invalid/writable/symlink denial passed.')
    finally:
        for path in root.iterdir(): path.unlink()
        root.rmdir()


if __name__ == '__main__':
    main()
