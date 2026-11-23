#!/usr/bin/python3 -I
"""Pinned guest file injection and at-most-once automatic bootstrap attempts."""

import hashlib
import fcntl
import json
import os
from pathlib import Path
import pwd
import grp
import signal
import subprocess
import sys
import tempfile
import time


def write_state(path, value):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, mode='w', delete=False) as f:
        pending = Path(f.name)
        json.dump(value, f)
        f.flush(); os.fsync(f.fileno())
    try:
        pending.chmod(0o600)
        os.replace(pending, path)
        fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try: os.fsync(fd)
        finally: os.close(fd)
    finally:
        pending.unlink(missing_ok=True)


def physical(root, guest_path):
    value = Path(guest_path)
    if not value.is_absolute() or '..' in value.parts:
        raise RuntimeError('invalid bootstrap path')
    target = root / str(value).lstrip('/')
    current = root
    for part in value.parts[1:]:
        current = current / part
        if current.is_symlink():
            raise RuntimeError('bootstrap path traverses a symlink')
    return target


def artifact(root, item):
    if not item['path'].startswith('/var/cache/lens/artifacts/'):
        raise RuntimeError('bootstrap artifact was not staged')
    source = physical(root, item['path'])
    with source.open('rb') as f:
        if hashlib.file_digest(f, 'sha256').hexdigest() != item['sha256']:
            raise RuntimeError('bootstrap artifact verification failed')
    return source


def apply(profile, root=Path('/')):
    root = root.resolve()
    state = physical(root, '/var/lib/lens/bootstrap')
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    # A previous QGA execution can still be running after the host loses its
    # reply. Recovery must not race that process past a missing step marker.
    fd = os.open(state / 'apply.lock', os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('bootstrap already running; operator recovery required') from None
        return _apply(profile, root)
    finally:
        os.close(fd)


def _apply(profile, root):
    state = root / 'var/lib/lens/bootstrap'
    result = {'files': [], 'run_once': []}
    for item in profile.get('files', []):
        source = artifact(root, item)
        destination = physical(root, item['destination'])
        marker = state / 'files' / (item['name'] + '.json')
        mode = int(item.get('mode', '0644'), 8)
        uid = pwd.getpwnam(item.get('owner', 'root')).pw_uid
        gid = grp.getgrnam(item.get('group', 'root')).gr_gid
        if marker.exists():
            previous = json.loads(marker.read_text())
            if previous.get('sha256') != item['sha256'] or previous.get('destination') != item['destination']:
                raise RuntimeError('file identity changed; use a new injection name')
            with destination.open('rb') as f:
                if hashlib.file_digest(f, 'sha256').hexdigest() != item['sha256']:
                    raise RuntimeError('injected file drift requires explicit reconciliation')
            st = destination.stat()
            if (st.st_mode & 0o777, st.st_uid, st.st_gid) != (mode, uid, gid):
                raise RuntimeError('injected file permissions changed')
        else:
            destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            # Recheck parents after creation; no path from a manifest is executed.
            physical(root, item['destination'])
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as f:
                pending = Path(f.name)
                with source.open('rb') as src:
                    while block := src.read(1024 * 1024): f.write(block)
                os.fchmod(f.fileno(), mode)
                os.fchown(f.fileno(), uid, gid)
                f.flush(); os.fsync(f.fileno())
            try:
                os.replace(pending, destination)
                directory_fd = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
                try: os.fsync(directory_fd)
                finally: os.close(directory_fd)
                write_state(marker, {'sha256': item['sha256'], 'destination': item['destination'], 'completed_at': time.time()})
            finally:
                pending.unlink(missing_ok=True)
        result['files'].append({'name': item['name'], 'sha256': item['sha256'], 'state': 'completed'})
    for item in profile.get('run_once', []):
        source = artifact(root, item)
        marker = state / 'steps' / (item['name'] + '.json')
        if marker.exists():
            previous = json.loads(marker.read_text())
            if previous.get('sha256') != item['sha256']:
                raise RuntimeError('bootstrap identity changed; use a new step name')
            if previous.get('state') != 'completed':
                raise RuntimeError('bootstrap attempt incomplete; operator recovery required')
            result['run_once'].append({'name': item['name'], 'state': 'completed', 'exit_code': 0})
            continue
        record = {'name': item['name'], 'sha256': item['sha256'], 'state': 'running', 'started_at': time.time()}
        # Commit intent BEFORE execution. A crash between effects/completion is
        # ambiguous; never silently retry arbitrary customer bootstrap code.
        write_state(marker, record)
        log = physical(root, '/var/log/lens-bootstrap/' + item['name'] + '.log')
        log.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            with os.fdopen(fd, 'wb') as output:
                process = subprocess.Popen([item.get('interpreter', '/bin/sh'), str(source)], stdin=subprocess.DEVNULL,
                                           stdout=output, stderr=subprocess.STDOUT, start_new_session=True,
                                           env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
                try:
                    code = process.wait(timeout=item.get('timeout_seconds', 300))
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGTERM)
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL); process.wait()
                    code = 124
            record.update(state='completed' if code == 0 else 'failed', exit_code=code, completed_at=time.time())
            write_state(marker, record)
            if code != 0:
                raise RuntimeError('bootstrap failed; private log retained in guest')
        except Exception:
            # A running marker is intentionally left when completion is unknown.
            raise
        result['run_once'].append({'name': item['name'], 'state': 'completed', 'exit_code': 0})
    return result


if __name__ == '__main__':
    try:
        profile = json.loads(Path('/run/lens/manifest.json').read_text())['build_profile']
        print(json.dumps(apply(profile)))
    except Exception:
        print('{"error":"guest bootstrap failed; inspect local guest state"}')
        raise SystemExit(1)
