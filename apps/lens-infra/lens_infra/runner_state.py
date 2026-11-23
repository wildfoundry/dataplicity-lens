"""Durable local intent shared with the adopted per-slot runner controller."""

import contextlib
import fcntl
import json
import os
from pathlib import Path
import tempfile
import time
import uuid

from .policy import integer, require

ROOT = Path("/var/lib/lens-infra/runner-control")


@contextlib.contextmanager
def locked(slot):
    integer(slot, 1, 99, "runner slot")
    ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = ROOT.lstat()
    require(not ROOT.is_symlink() and st.st_uid == os.geteuid() and st.st_mode & 0o077 == 0, "runner control directory is not private")
    fd = os.open(ROOT / f"{slot:02}.lock", os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield ROOT / f"{slot:02}.json"
    finally:
        os.close(fd)


def read(path):
    if not path.exists():
        return None
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(fd) as f:
        st = os.fstat(f.fileno())
        require(st.st_uid == os.geteuid() and st.st_mode & 0o077 == 0 and st.st_size <= 16384, "runner intent file is not trusted")
        return json.load(f)


def write(path, data):
    with tempfile.NamedTemporaryFile(mode="w", dir=ROOT, delete=False) as f:
        pending = Path(f.name)
        try:
            json.dump(data, f)
            f.flush()
            os.fsync(f.fileno())
            os.replace(pending, path)
            fd = os.open(ROOT, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            pending.unlink(missing_ok=True)


def status(slot):
    with locked(slot) as path:
        return read(path)


def request(slot, mode, expected_uuid, actor=0):
    require(mode in ("hold", "resume"), "invalid runner intent")
    uuid.UUID(expected_uuid)
    with locked(slot) as path:
        old = read(path)
        if mode == "resume":
            require(old and old["mode"] == "hold" and old["stage"] == "held" and old["expected_uuid"] == expected_uuid, "resume requires the exact held VM")
        data = {"slot": slot, "mode": mode, "stage": "requested", "expected_uuid": expected_uuid,
                "generation": (old or {}).get("generation", 0) + 1, "actor": actor, "requested_at": time.time()}
        write(path, data)
        return data


def update(slot, intent, stage, **fields):
    with locked(slot) as path:
        data = read(path)
        require(data and data["generation"] == intent["generation"], "runner intent changed")
        data.update(stage=stage, updated_at=time.time(), **fields)
        write(path, data)
        return data


def held(slot):
    data = status(slot)
    return bool(data and data["mode"] == "hold")
