#!/usr/bin/env python3
"""Exercise the shipped group viewer without credentials or network access."""
import fcntl
import json
import os
import pathlib
import pty
import select
import struct
import subprocess
import sys
import termios
import time

binary = str(pathlib.Path(sys.argv[1]).resolve() / 'lens-group')
group = '11111111-1111-4111-8111-111111111111'
result = subprocess.run([binary, '--demo', '--group', group, '--json', '--search', 'database'], capture_output=True, check=True, timeout=5)
page = json.loads(result.stdout)
assert page['results'] and all('database' in row['name'] for row in page['results'])
assert all('memory_total_bytes' in row and 'service_tag' in row for row in page['results'])
plain = subprocess.check_output([binary, '--demo', '--group', group, '--once'], timeout=5).decode()
assert 'Storage' in plain and 'Report ' in plain and 'ms since epoch' not in plain
assert 'DEMO' in plain
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
process = subprocess.Popen([binary, '--demo'], stdin=slave, stdout=slave, stderr=slave, env={**os.environ, 'TERM': 'xterm-256color'})
os.close(slave)
pending = bytearray()
def wait_for(text):
    deadline = time.monotonic() + 5
    while text not in pending:
        assert process.poll() is None, bytes(pending)
        assert time.monotonic() < deadline, bytes(pending)
        if select.select([master], [], [], .1)[0]:
            pending.extend(os.read(master, 65536))
try:
    wait_for(b'Production servers')
    os.write(master, b'\r')
    wait_for(b'build-01')
    pending.clear()
    os.write(master, b'/database\r')
    # Demo requests are local but still travel through the same UI worker.
    wait_for(b'database-07')
    pending.clear()
    os.write(master, b'g')
    # Ratatui emits cursor-positioned deltas, so a changed heading is not
    # necessarily contiguous in the byte stream. Drain the transition, then
    # verify the chooser remains interactive and quit works.
    until = time.monotonic() + .3
    while time.monotonic() < until:
        if select.select([master], [], [], .05)[0]:
            os.read(master, 65536)
    assert process.poll() is None
    start = time.monotonic()
    os.write(master, b'q')
    while process.poll() is None and time.monotonic() - start < 2:
        if select.select([master], [], [], .05)[0]:
            try:
                os.read(master, 65536)
            except OSError:
                break
    assert process.wait(timeout=.2) == 0
    assert time.monotonic() - start < 2
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
print('Group chooser, server search, human/JSON output and responsive terminal exit passed.')
