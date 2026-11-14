"""Offline resize protection and retention of controller-owned recovery copies."""

import hashlib
import json
import time
from pathlib import Path

from .backend import file_digest
from .policy import Rejected, require

RETENTION_SECONDS = 7 * 24 * 60 * 60


def backup_id(operation_id):
    return "resize-" + hashlib.sha256(operation_id.encode()).hexdigest()[:32]


def protect(engine, operation, resource):
    """Never mutate a disk until its standalone recovery copy has been checked."""
    engine.backend.require_stopped(resource)
    identity = backup_id(operation["id"])
    backup = engine.store.backup(identity)
    require(backup["resource_id"] == resource["id"] and backup["detail"].get("resize_operation") == operation["id"],
            "resize_failure: recovery backup identity changed")
    destination = engine.store.root / "backups" / identity / "root.qcow2"
    if backup["state"] != "ready":
        engine.store.transition(operation["id"], "resize-backup")
        checksum = engine.backend.backup(resource, destination)
        engine.store.resize_backup_ready(identity, checksum, RETENTION_SECONDS)
        backup = engine.store.backup(identity)
    require(destination.is_file() and not destination.is_symlink() and file_digest(destination) == backup["sha256"],
            "resize_failure: recovery backup verification failed")
    engine.backend.require_stopped(resource)
    return backup


def expire(engine, now=None):
    """Remove only expired automatic copies, under the daemon's executor lock.

    Manual backups, failed/incomplete copies and copies referenced by live restore
    operations are not eligible. Reservations are released after files are removed.
    """
    now = time.time() if now is None else now
    with engine.store.connection() as connection:
        operations = [dict(row) for row in connection.execute("SELECT * FROM operations")]
        current = {row[0] for row in connection.execute("SELECT current_id FROM services WHERE current_id IS NOT NULL")}
    pinned = set()
    for operation in operations:
        request = json.loads(operation["request"])
        if operation["state"] in ("accepted", "running", "interrupted") or operation["id"] in current:
            if request.get("restore_backup"):
                pinned.add(request["restore_backup"])
            pinned.add(backup_id(operation["id"]))
    expired = []
    for backup in engine.store.backups():
        detail = backup["detail"]
        if detail.get("kind") != "pre-resize" or backup["state"] not in ("ready", "expiring"):
            continue
        expiry = detail.get("retain_until")
        if not isinstance(expiry, (int, float)) or expiry > now or backup["id"] in pinned or detail.get("workspace_gib", 0):
            continue
        require(backup["id"] == backup_id(detail.get("resize_operation", "")), "automatic backup identity is not trusted")
        directory = engine.store.root / "backups" / backup["id"]
        require(not directory.is_symlink(), "automatic backup directory is not trusted")
        path = directory / "root.qcow2"
        if backup["state"] == "ready":
            # Hash before authorizing deletion; never infer ownership from age/name alone.
            require(path.is_file() and not path.is_symlink() and file_digest(path) == backup["sha256"], "expired backup changed; local inspection required")
            with engine.store.connection() as connection:
                connection.execute("UPDATE backups SET state='expiring' WHERE id=?", (backup["id"],))
        if path.exists():
            require(path.is_file() and not path.is_symlink() and file_digest(path) == backup["sha256"], "expiring backup changed; local inspection required")
            path.unlink()
        with engine.store.connection() as connection:
            connection.execute("UPDATE backups SET state='expired' WHERE id=?", (backup["id"],))
            engine.store.audit(connection, detail["resize_operation"], 0, "resize-backup-expired", {"backup_id": backup["id"]})
        expired.append(backup["id"])
    return expired
