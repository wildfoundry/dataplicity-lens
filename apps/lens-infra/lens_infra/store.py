"""Local, transactional inventory, capacity reservations and operation history."""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import sqlite3
import time
from pathlib import Path

from .policy import Rejected, digest, require


class Store:
    def __init__(self, directory):
        self.root = Path(directory)
        self.root.mkdir(parents=True, mode=0o700, exist_ok=True)
        st = self.root.lstat()
        require(self.root.is_dir() and not self.root.is_symlink(), "state directory must not be a symlink")
        require(st.st_uid == os.geteuid() and st.st_mode & 0o022 == 0, "state directory must have a trusted owner and permissions")
        db = self.root / "state.sqlite3"
        require(not db.is_symlink(), "state database must not be a symlink")
        self.path = str(db)
        with self.connection() as c:
            schema = """
                CREATE TABLE IF NOT EXISTS policies(revision INTEGER PRIMARY KEY, digest TEXT UNIQUE NOT NULL, document TEXT NOT NULL, applied_at REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS resources(id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, owner TEXT NOT NULL, protected INTEGER NOT NULL, environment TEXT, service TEXT, operation_id TEXT, state TEXT NOT NULL, vcpus INTEGER NOT NULL, memory_mib INTEGER NOT NULL, disk_gib INTEGER NOT NULL, observed_at REAL NOT NULL, detail TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY, request_digest TEXT NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, stage TEXT NOT NULL, actor INTEGER NOT NULL, created_at REAL NOT NULL, updated_at REAL NOT NULL, error TEXT, previous_id TEXT);
                CREATE TABLE IF NOT EXISTS events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, operation_id TEXT, timestamp REAL NOT NULL, actor INTEGER NOT NULL, event TEXT NOT NULL, detail TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS services(id TEXT PRIMARY KEY, current_id TEXT, previous_id TEXT);
            """
            for statement in schema.split(";"):
                if statement.strip():
                    c.execute(statement)
        os.chmod(db, 0o600)

    @contextlib.contextmanager
    def connection(self):
        c = sqlite3.connect(self.path, timeout=10, isolation_level=None)
        c.row_factory = sqlite3.Row
        c.execute("PRAGMA foreign_keys=ON")
        c.execute("PRAGMA synchronous=FULL")
        c.execute("BEGIN IMMEDIATE")
        try:
            yield c
            c.execute("COMMIT")
        except BaseException:
            c.execute("ROLLBACK")
            raise
        finally:
            c.close()

    @contextlib.contextmanager
    def exclusive(self):
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(self.root / "executor.lock", flags, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise Rejected("another infrastructure executor is active") from exc
            yield
        finally:
            os.close(fd)

    @staticmethod
    def audit(c, operation, actor, event, detail=None):
        c.execute("INSERT INTO events(operation_id,timestamp,actor,event,detail) VALUES(?,?,?,?,?)",
                  (operation, time.time(), actor, event, json.dumps(detail or {}, sort_keys=True)))

    def policy(self):
        with self.connection() as c:
            row = c.execute("SELECT document FROM policies ORDER BY revision DESC LIMIT 1").fetchone()
            return json.loads(row[0]) if row else None

    def policy_at(self, revision):
        with self.connection() as c:
            row = c.execute("SELECT document FROM policies WHERE revision=?", (revision,)).fetchone()
            require(row is not None, "configuration revision is not available locally")
            return json.loads(row[0])

    def apply_policy(self, policy, uid):
        with self.connection() as c:
            old = c.execute("SELECT revision,digest FROM policies ORDER BY revision DESC LIMIT 1").fetchone()
            d = digest(policy)
            if old and old[0] == policy["revision"] and old[1] == d:
                return {"revision": old[0], "digest": d, "changed": False}
            require(not old or policy["revision"] > old[0], "policy revisions must increase; rollback uses a new revision")
            if old:
                before = json.loads(c.execute("SELECT document FROM policies WHERE revision=?", (old[0],)).fetchone()[0])
                require(before["host_id"] == policy["host_id"], "host identity cannot be changed by a policy update")
                for section in ("classes", "templates", "networks"):
                    for key in before.get(section, {}).keys() & policy.get(section, {}).keys():
                        require(before[section][key] == policy[section][key], f"{section} definitions are immutable; create a new versioned name")
                active = c.execute("SELECT 1 FROM resources WHERE owner='lens' AND state!='removed' LIMIT 1").fetchone()
                if active:
                    require(before.get("networks", {}).keys() <= policy.get("networks", {}).keys(), "active workloads require their network definitions to remain available")
                for key in before.get("secrets", {}).keys() & policy.get("secrets", {}).keys():
                    a, b = before["secrets"][key], policy["secrets"][key]
                    if a != b:
                        require(b["generation"] > a["generation"] and b["path"] != a["path"], "secret rotation requires a new generation and a new versioned local file")
            require(not c.execute("SELECT 1 FROM operations WHERE state IN ('accepted','running','interrupted') LIMIT 1").fetchone(),
                    "finish or resolve active operations before changing policy")
            c.execute("INSERT INTO policies VALUES(?,?,?,?)", (policy["revision"], d, json.dumps(policy), time.time()))
            self.audit(c, None, uid, "configuration-applied", {"revision": policy["revision"], "digest": d})
            return {"revision": policy["revision"], "digest": d, "changed": True}

    def observe(self, resources):
        """Unknown objects are protected, including objects with copied Lens metadata."""
        with self.connection() as c:
            for item in resources:
                if item.get("detail", {}).get("provider") == "wildfoundry-runner-v1":
                    # An explicitly configured provider identifies a logical slot
                    # across normal per-job UUID changes. Keep old identities as
                    # protected aliases; charge its current disk/reservation once.
                    for alias in c.execute("SELECT * FROM resources WHERE name=? AND owner='external' AND id!=?", (item["name"], item["id"])).fetchall():
                        old_detail = json.loads(alias["detail"])
                        old_files = [d.get("file") for d in old_detail.get("disks", [])]
                        new_files = [d.get("file") for d in item["detail"].get("disks", [])]
                        if alias["kind"] == "vm" and old_files and old_files == new_files:
                            old_detail.pop("capacity_unknown", None)
                            old_detail["provider_alias_for"] = item["id"]
                            c.execute("UPDATE resources SET state='provider-history',vcpus=0,memory_mib=0,disk_gib=0,detail=? WHERE id=?", (json.dumps(old_detail), alias["id"]))
                old = c.execute("SELECT * FROM resources WHERE id=?", (item["id"],)).fetchone()
                owned = bool(old and old["owner"] == "lens" and old["state"] != "removed" and item.get("operation_id") == old["operation_id"])
                owner = "lens" if owned else "external"
                protected = 0 if owned else 1
                detail = (json.loads(old["detail"]) if owned else {}) | dict(item.get("detail", {}))
                if owned:
                    detail["observed_state"] = item["state"]
                c.execute("""INSERT INTO resources VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)
                    ON CONFLICT(id) DO UPDATE SET name=excluded.name,kind=excluded.kind,owner=excluded.owner,
                    protected=excluded.protected,state=excluded.state,vcpus=excluded.vcpus,memory_mib=excluded.memory_mib,
                    disk_gib=excluded.disk_gib,observed_at=excluded.observed_at,detail=excluded.detail""",
                    (item["id"], item["name"], item["kind"], owner, protected,
                     old["environment"] if owned else None, old["service"] if owned else None,
                     old["operation_id"] if owned else None, old["state"] if owned else item["state"],
                     max(old["vcpus"], item.get("vcpus", 0)) if owned else item.get("vcpus", 0),
                     max(old["memory_mib"], item.get("memory_mib", 0)) if owned else item.get("memory_mib", 0),
                     max(old["disk_gib"], item.get("disk_gib", 0)) if owned else item.get("disk_gib", 0), time.time(), json.dumps(detail)))
            # Missing observations never delete records or release reservations implicitly.

    def inventory(self):
        with self.connection() as c:
            rows = c.execute("SELECT * FROM resources ORDER BY kind,name").fetchall()
            return [dict(r) | {"detail": json.loads(r["detail"])} for r in rows]

    @staticmethod
    def _capacity(c, policy, request, allocations):
        all_rows = c.execute("SELECT * FROM resources").fetchall()
        rows = [row for row in all_rows if row["state"] != "removed"]
        limits = policy["capacity"]
        env = policy["environments"][request["environment"]]
        overrides = policy.get("protected_resources", {})
        parked = set(policy.get("parked_resources", []))
        for identity in parked:
            matches = [row for row in rows if row["id"] == identity]
            require(len(matches) == 1 and matches[0]["protected"] and matches[0]["kind"] == "vm" and matches[0]["state"] == "shut off" and time.time() - matches[0]["observed_at"] < 30,
                    "parked VM must remain defined, protected and freshly verified shut off")
        for row in rows:
            unknown = row["kind"] != "vm" or json.loads(row["detail"]).get("capacity_unknown")
            require(not unknown or row["id"] in overrides, "protected runtime resources need explicit administrator capacity reservations before new builds")
        for key in ("vcpus", "memory_mib", "disk_gib"):
            # Retained disks remain charged even after a VM is unregistered.
            budget_rows = all_rows if key == "disk_gib" else [row for row in rows if row["id"] not in parked]
            used = sum(max(row[key], overrides.get(row["id"], {}).get(key, 0)) for row in budget_rows)
            needed = sum(a[key] for a in allocations)
            require(used + needed <= limits[key], f"host {key} capacity exhausted: reserved={used}, requested={needed}, limit={limits[key]}")
            env_used = sum(row[key] for row in budget_rows if row["environment"] == request["environment"])
            require(env_used + needed <= env[key], f"environment {key} quota exhausted")
        env_count = sum(row["environment"] == request["environment"] for row in rows)
        require(env_count + len(allocations) <= env["max_instances"], "environment instance quota exhausted (including rollout surge)")

    def check_capacity(self, policy, request, allocations):
        with self.connection() as c:
            self._capacity(c, policy, request, allocations)

    def reserve(self, policy, request, allocations, uid, expected_previous=None):
        """The idempotency check, service lock and all reservations commit together."""
        with self.connection() as c:
            active = c.execute("SELECT digest FROM policies ORDER BY revision DESC LIMIT 1").fetchone()
            require(active and active[0] == digest(policy), "policy changed before reservation; plan again against the active revision")
            prior = c.execute("SELECT * FROM operations WHERE id=?", (request["operation_id"],)).fetchone()
            d = digest(request)
            if prior:
                require(prior["request_digest"] == d, "operation ID was already used for a different request")
                return dict(prior) | {"duplicate": True}
            for row in c.execute("SELECT request FROM operations WHERE state IN ('accepted','running','interrupted')"):
                other = json.loads(row[0])
                require((other["environment"], other["service"]) != (request["environment"], request["service"]), "another operation owns this service")
            template = policy["templates"][request["template"]]
            if template.get("profile", "private") != "private":
                others = c.execute("SELECT request FROM operations WHERE state IN ('accepted','running','interrupted') UNION SELECT o.request FROM services s JOIN operations o ON o.id=s.current_id").fetchall()
                for other in (json.loads(row[0]) for row in others):
                    if (other["environment"], other["service"]) == (request["environment"], request["service"]):
                        continue
                    require(other.get("listen_port", 8080) != request.get("listen_port", 8080), "listener is already assigned to another service")
            self._capacity(c, policy, request, allocations)
            now = time.time()
            service_id = request["environment"] + "/" + request["service"]
            current = c.execute("SELECT current_id FROM services WHERE id=?", (service_id,)).fetchone()
            previous_id = current[0] if current else None
            require(request["instances"] > 0 or previous_id is not None, "only an existing service can be drained to zero")
            if expected_previous is not None:
                require(previous_id == expected_previous, "active release changed before reservation; review again")
            c.execute("INSERT INTO operations VALUES(?,?,?,?,?,?,?,?,?,?)",
                      (request["operation_id"], d, json.dumps(request), "accepted", "reserved", uid, now, now, None, previous_id))
            for a in allocations:
                c.execute("INSERT INTO resources VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                          (a["id"], a["name"], "vm", "lens", 0, request["environment"], request["service"],
                           request["operation_id"], "reserved", a["vcpus"], a["memory_mib"], a["disk_gib"], now,
                           json.dumps(a)))
            self.audit(c, request["operation_id"], uid, "deployment-accepted", {"revision": policy["revision"]})
            return {"id": request["operation_id"], "state": "accepted", "duplicate": False}

    def transition(self, operation_id, stage, state="running", error=None):
        with self.connection() as c:
            c.execute("UPDATE operations SET state=?,stage=?,updated_at=?,error=? WHERE id=?",
                      (state, stage, time.time(), error, operation_id))
            self.audit(c, operation_id, 0, stage, {"state": state, "error": error})

    def operation(self, operation_id):
        with self.connection() as c:
            row = c.execute("SELECT * FROM operations WHERE id=?", (operation_id,)).fetchone()
            require(row is not None, "unknown operation")
            result = dict(row)
            result["request"] = json.loads(result["request"])
            result["resources"] = [dict(r) | {"detail": json.loads(r["detail"])} for r in c.execute("SELECT * FROM resources WHERE operation_id=?", (operation_id,))]
            return result

    def resource_state(self, resource_id, state):
        with self.connection() as c:
            require(c.execute("SELECT 1 FROM resources WHERE id=? AND owner='lens' AND protected=0", (resource_id,)).fetchone(), "existing/imported resources are protected")
            c.execute("UPDATE resources SET state=?,observed_at=? WHERE id=?", (state, time.time(), resource_id))

    def resource_detail(self, resource_id, detail):
        with self.connection() as c:
            c.execute("UPDATE resources SET detail=? WHERE id=? AND owner='lens'", (json.dumps(detail), resource_id))

    def promote(self, operation_id):
        with self.connection() as c:
            row = c.execute("SELECT request,previous_id FROM operations WHERE id=?", (operation_id,)).fetchone()
            r = json.loads(row[0])
            c.execute("INSERT INTO services VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET current_id=excluded.current_id,previous_id=excluded.previous_id",
                      (r["environment"] + "/" + r["service"], operation_id, row[1]))

    def interrupted(self):
        with self.connection() as c:
            c.execute("UPDATE operations SET state='interrupted' WHERE state='running'")
            return [r[0] for r in c.execute("SELECT id FROM operations WHERE state IN ('accepted','interrupted') ORDER BY created_at")]

    def events(self):
        with self.connection() as c:
            return [dict(r) | {"detail": json.loads(r["detail"])} for r in c.execute("SELECT * FROM events ORDER BY sequence DESC LIMIT 1000")]

    def current_operations(self):
        with self.connection() as c:
            return [r[0] for r in c.execute("SELECT current_id FROM services WHERE current_id IS NOT NULL")]

    def service(self, environment, service):
        with self.connection() as c:
            row = c.execute("SELECT * FROM services WHERE id=?", (environment + "/" + service,)).fetchone()
            require(row and row["current_id"], "service has no active release")
            return dict(row)

    def services(self):
        with self.connection() as c:
            return [dict(r) for r in c.execute("SELECT * FROM services ORDER BY id")]

    def cancel(self, operation_id, uid):
        with self.connection() as c:
            row = c.execute("SELECT state FROM operations WHERE id=?", (operation_id,)).fetchone()
            require(row and row[0] in ("accepted", "interrupted", "failed"), "only queued, interrupted or failed operations may be cancelled")
            require(not c.execute("SELECT 1 FROM services WHERE current_id=?", (operation_id,)).fetchone(), "the active release cannot be cancelled")
            c.execute("UPDATE operations SET state='cancelled',stage='retained',updated_at=? WHERE id=?", (time.time(), operation_id))
            self.audit(c, operation_id, uid, "cancelled-with-resources-retained")
            return {"id": operation_id, "state": "cancelled", "resources_retained": True}
