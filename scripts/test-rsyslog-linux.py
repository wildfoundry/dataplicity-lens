#!/usr/bin/env python3
"""Parse real generated rsyslog TLS configuration without making a network connection."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import uuid

assert sys.platform == 'linux' and os.geteuid() != 0
binary = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='lens-syslog-ci-') as root:
    directory=Path(root)
    os.chmod(directory,0o700)
    token='lnslog1.'+'a'*64+'.'+'b'*64
    state={'server_url':'https://app-api.dataplicity.com','registration_id':'00000000-0000-4000-8000-000000000001',
           'device_hash':'a'*64,'device_secret':'upload-fixture-not-a-real-credential','syslog_token':token,
           'gate':{'failures':0,'not_before_ms':0,'halted':False,'interval_seconds':60}}
    path=directory/'state.json'
    path.write_text(json.dumps(state));path.chmod(0o600)
    generated=subprocess.run([str(binary),'--state-dir',root,'syslog-config','--ca-file','/etc/ssl/certs/ca-certificates.crt'],capture_output=True,text=True,check=True)
    config=directory/'rsyslog.conf'
    config.write_text(generated.stdout);config.chmod(0o600)
    assert state['device_secret'] not in generated.stdout
    # Ubuntu confines rsyslog configuration reads to /etc. Use the same private root-owned
    # location as production setup instead of weakening directory/token permissions.
    assert os.environ.get('GITHUB_ACTIONS') == 'true', 'run only on disposable Linux CI'
    installed = Path('/etc/rsyslog.d') / ('.lens-ci-' + uuid.uuid4().hex + '.conf')
    subprocess.run(['sudo','install','-m','0600',str(config),str(installed)],check=True)
    try:
        result=subprocess.run(['sudo','rsyslogd','-N1','-f',str(installed)],capture_output=True,text=True)
        assert result.returncode==0,(result.stdout+result.stderr).replace(token,'[redacted]')
    finally:
        subprocess.run(['sudo','rm','-f',str(installed)],check=True)
    print('Generated rsyslog configuration accepted; logs-only credential and TLS boundary verified.')
