#!/usr/bin/env python3
"""Linux CI integration: real FIFO permissions and live non-root collector, no network."""
import json
import os
import pathlib
import pwd
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def reader(path):
    # The publisher's actual account must be unable to alter the diode or reach collector state.
    for operation in (lambda: os.open(path, os.O_WRONLY | os.O_NONBLOCK),
                      lambda: os.open(path.parent / 'injected', os.O_CREAT | os.O_WRONLY, 0o600)):
        try:
            fd = operation()
        except PermissionError:
            pass
        else:
            os.close(fd)
            raise AssertionError('publisher obtained write access')
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    pending = bytearray()
    deadline = time.monotonic() + 105
    try:
        while time.monotonic() < deadline:
            try:
                chunk = os.read(fd, 8192)
            except BlockingIOError:
                chunk = b''
            pending.extend(chunk)
            assert len(pending) <= 256 * 1024 + 8192
            while b'\n' in pending:
                line, _, rest = pending.partition(b'\n')
                pending[:] = rest
                if not line:
                    continue
                value = json.loads(line)
                assert len(line) <= 256 * 1024
                assert value['protocol_version'] == 1
                assert value['schema_version'] == '2'
                assert value['expires_at_ms'] - value['collected_at_ms'] == 1800000
                assert value['snapshot']['host']['hostname']
                assert value['snapshot']['processes']
                assert all('start_time_ticks' in p for p in value['snapshot']['processes'])
                print('Real Linux observation received; publisher write attempts denied.')
                return
            time.sleep(0.025)
        raise AssertionError('no collector observation before deadline')
    finally:
        os.close(fd)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == 'reader':
        return reader(pathlib.Path(sys.argv[2]))
    assert sys.platform == 'linux' and os.geteuid() == 0, 'run only as root in disposable Linux CI'
    binary_dir = pathlib.Path(sys.argv[1]).resolve()
    for binary in ('lens-daemon', 'lens-publisher'):
        args = [binary_dir / binary] + (['run'] if binary == 'lens-publisher' else [])
        result = subprocess.run(args, capture_output=True)
        assert result.returncode != 0 and b'non-root' in result.stderr, 'reporting executable accepted root'
    collector = pwd.getpwnam('lens-collector')
    import grp
    group = grp.getgrnam('lens-report')
    directory = pathlib.Path(tempfile.mkdtemp(prefix='lens-report-ci-', dir='/run'))
    process = None
    try:
        os.chown(directory, collector.pw_uid, group.gr_gid)
        os.chmod(directory, 0o710)
        pipe = directory / 'observations'
        os.mkfifo(pipe, 0o240)
        os.chmod(pipe, 0o240)
        os.chown(pipe, collector.pw_uid, group.gr_gid)
        process = subprocess.Popen([str(binary_dir / 'lens-daemon'), '--pipe', str(pipe)],
                                   user=collector.pw_uid, group=collector.pw_gid, extra_groups=[],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                   start_new_session=True)
        subprocess.run(['runuser', '-u', 'lens-publisher', '--', sys.executable,
                        str(pathlib.Path(__file__).resolve()), 'reader', str(pipe)],
                       check=True, timeout=110)
    finally:
        if process is not None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                _, error = process.communicate(timeout=30)
                assert process.returncode in (0, -signal.SIGTERM), error.decode(errors='replace')
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise
        shutil.rmtree(directory)


if __name__ == '__main__':
    main()
