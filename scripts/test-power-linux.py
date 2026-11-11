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
        sample.write_text(json.dumps(value)); sample.chmod(0o644)
    def observe():
        result = subprocess.run(['/usr/sbin/runuser', '-u', 'lens-collector', '--', str(binary), '--once', '--json', '--fields', 'hardware'],
                                capture_output=True, timeout=20, check=True)
        return (json.loads(result.stdout).get('hardware', {}).get('server') or {}).get('power')
    try:
        write()
        assert observe()['watts'] == 182
        denied = subprocess.run(['/usr/sbin/runuser', '-u', 'lens-collector', '--', str(binary), '--sample-power'],
                                capture_output=True, timeout=10)
        assert denied.returncode != 0 and json.loads(sample.read_text())['watts'] == 182
        for changes in ({'observed_at': now(-61)}, {'observed_at': now(120)}, {'watts': 0}, {'watts': 65535}, {'source': 'estimated'}):
            write(**changes); assert observe() is None
        write(); sample.chmod(0o666); assert observe() is None
        write(); retained = root / 'retained.json'; sample.rename(retained); sample.symlink_to(retained)
        assert observe() is None
        sample.unlink(); retained.unlink()
        subprocess.run([str(binary), '--sample-power'], capture_output=True, check=True, timeout=10)
        assert sample.stat().st_uid == 0 and sample.stat().st_mode & 0o022 == 0
        value = json.loads(sample.read_text())
        assert value is None or set(value) == {'watts', 'observed_at', 'source'}
        invalid = subprocess.run([str(binary), '--sample-power', '--json'], capture_output=True, timeout=10)
        assert invalid.returncode != 0
        print('Power cache: unprivileged read, protected writer, stale/future/invalid/writable/symlink denial passed.')
    finally:
        for path in root.iterdir(): path.unlink()
        root.rmdir()


if __name__ == '__main__':
    main()
