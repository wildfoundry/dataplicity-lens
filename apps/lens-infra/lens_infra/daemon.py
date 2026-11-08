"""Local Unix IPC, authenticated from kernel peer credentials, and a single executor."""

from __future__ import annotations

import json
import os
import queue
import socket
import struct
import threading
from pathlib import Path

from .policy import Rejected, authorize, digest, keys, require, validate

MAX_FRAME = 1024 * 1024


def read_frame(stream):
    data = bytearray()
    while len(data) <= MAX_FRAME:
        part = stream.recv(min(65536, MAX_FRAME + 1 - len(data)))
        require(part, "incomplete request")
        data.extend(part)
        if b"\n" in part:
            frame, extra = bytes(data).split(b"\n", 1)
            require(not extra.strip(), "one request per connection")
            return json.loads(frame)
    raise Rejected("request exceeds frame size")


class Server:
    def __init__(self, engine, socket_path, group=None):
        self.engine = engine
        self.path = Path(socket_path)
        self.group = group
        self.jobs = queue.Queue()
        self.execution = threading.Lock()

    def dispatch(self, message, uid):
        keys(message, ["method", "payload"], "IPC envelope")
        method, payload = message.get("method"), message.get("payload", {})
        if method == "config.apply":
            require(uid == 0, "only the local administrator may replace infrastructure policy")
            p = validate(payload)
            return self.engine.store.apply_policy(p, uid)
        p = self.engine.policy()
        if method in ("inventory", "status", "metrics", "events", "operation.status"):
            authorize(p, uid, "read")
            if method == "inventory":
                require(uid == 0, "full protected host inventory requires local administrator access")
                return self.engine.inventory()
            if method == "events":
                require(uid == 0, "audit history requires local administrator access")
                return self.engine.store.events()
            if method == "operation.status":
                keys(payload, ["operation_id"], "operation selector")
                result = self.engine.store.operation(payload["operation_id"])
                authorize(p, uid, "read", result["request"]["environment"])
                return result
            rows = self.engine.store.inventory()
            if uid != 0:
                allowed = {e for principal in p.get("principals", []) if principal["uid"] == uid for e in principal.get("environments", [])}
                rows = [r for r in rows if r["environment"] in allowed]
            if method == "metrics":
                lines = ["# TYPE lens_infra_configuration_revision gauge", "lens_infra_configuration_revision " + str(p["revision"]), "# TYPE lens_infra_resources gauge"]
                counts = {}
                for row in rows:
                    label = (row["environment"] or "protected", row["state"], bool(row["protected"]))
                    counts[label] = counts.get(label, 0) + 1
                for (environment, state, protected), count in sorted(counts.items()):
                    lines.append('lens_infra_resources{environment=' + json.dumps(environment) + ',state=' + json.dumps(state) + ',protected="' + str(protected).lower() + '"} ' + str(count))
                return "\n".join(lines) + "\n"
            services = self.engine.store.services()
            if uid != 0:
                services = [s for s in services if s["id"].split("/", 1)[0] in allowed]
            for service in services:
                operation = self.engine.store.operation(service["current_id"])
                req = operation["request"]
                service.update(desired_instances=req["instances"], applied_release=req["release"], applied_configuration_revision=req["configuration_revision"], last_healthy_operation=operation["id"])
            return {"host_id": p["host_id"], "configuration_revision": p["revision"], "configuration_digest": digest(p), "resources": rows, "services": services}
        if method == "images":
            authorize(p, uid, "read")
            result = []
            for environment, definition in p["environments"].items():
                try:
                    authorize(p, uid, "read", environment)
                except Rejected:
                    continue
                for image_id in definition.get("images", []):
                    image = p["images"][image_id]
                    # Paths are administrator metadata, not part of the user catalogue.
                    result.append({"image": image_id, "environment": environment, "os_family": image["os_family"],
                                   "format": image.get("format", "qcow2"), "sha256": image["sha256"], "capabilities": image["capabilities"]})
            return {"host_id": p["host_id"], "configuration_revision": p["revision"], "images": result}
        if method in ("alerts", "thresholds"):
            from . import alerts
            require(not payload, "alert queries do not accept configuration changes")
            return alerts.status(self.engine, uid) if method == "alerts" else alerts.thresholds(self.engine, uid)
        if method == "classes":
            from .capacity import classes
            return classes(self.engine, payload, uid)
        if method == "deploy.plan":
            return self.engine.plan(payload, uid)
        if method in ("runner.hold", "runner.resume"):
            from .runner_provider import control
            require("action" not in payload, "runner action is selected by the operation")
            return control(p, payload | {"action": method.split(".")[1]}, uid)
        if method == "runner.status":
            from . import runner_state
            require(uid == 0, "existing runner control state requires the local administrator")
            keys(payload, ["slot"], "runner selector")
            config = p.get("runner_controller")
            require(config and payload.get("slot") in config["control_slots"], "runner slot has not been explicitly adopted")
            return runner_state.status(payload["slot"])
        if method == "deploy.prepare":
            return self.engine.prepare(payload, uid)
        if method in ("vm.create", "vm.create.plan", "vm.restore", "vm.restore.plan"):
            from .vm import create_request, restore_request
            request = restore_request(self.engine, payload, uid) if method.startswith("vm.restore") else create_request(payload)
            if method.endswith(".plan"):
                return self.engine.plan(request, uid)
            result = self.engine.submit(request, uid)
            if not result["duplicate"]:
                self.jobs.put(result["id"])
            return result
        if method in ("vm.action.plan", "vm.action.apply"):
            from .vm import plan, submit
            if method.endswith(".plan"):
                return plan(self.engine, payload, uid)
            result = submit(self.engine, payload, uid)
            if not result["duplicate"]:
                self.jobs.put(result["id"])
            return result
        if method == "vm.backups":
            authorize(p, uid, "read")
            result = self.engine.store.backups()
            if uid != 0:
                allowed = {e for principal in p.get("principals", []) if principal["uid"] == uid for e in principal.get("environments", [])}
                result = [b for b in result if b["environment"] in allowed]
            return result
        if method == "deploy.apply":
            result = self.engine.submit(payload, uid)
            if not result["duplicate"]:
                self.jobs.put(result["id"])
            return result
        if method in ("service.scale", "service.drain", "service.rollback", "service.scale.plan", "service.drain.plan", "service.rollback.plan"):
            action = method.split(".")[1]
            planned = method.endswith(".plan")
            result = self.engine.service_change(action, payload, uid, plan=planned)
            if not planned and not result["duplicate"]:
                self.jobs.put(result["id"])
            return result
        if method == "operation.cancel":
            keys(payload, ["operation_id"], "operation selector")
            result = self.engine.store.operation(payload["operation_id"])
            authorize(p, uid, "deploy", result["request"]["environment"])
            require(self.execution.acquire(blocking=False), "executor is active; wait for a checkpoint or interrupt locally")
            try:
                return self.engine.store.cancel(result["id"], uid)
            finally:
                self.execution.release()
        if method == "operation.cleanup":
            keys(payload, ["operation_id"], "operation selector")
            require(self.execution.acquire(blocking=False), "executor is active; retry after the current operation")
            try:
                return self.engine.cleanup(payload["operation_id"], uid)
            finally:
                self.execution.release()
        if method == "operation.resume":
            keys(payload, ["operation_id"], "operation selector")
            result = self.engine.store.operation(payload["operation_id"])
            authorize(p, uid, "deploy", result["request"]["environment"])
            require(result["state"] in ("accepted", "interrupted"), "operation is not resumable")
            self.jobs.put(result["id"])
            return {"id": result["id"], "queued": True}
        raise Rejected("unknown local infrastructure operation")

    def worker(self):
        while True:
            operation = self.jobs.get()
            try:
                with self.execution:
                    self.engine.execute(operation)
            except Exception:
                self.engine.store.transition(operation, "failed", "failed", "local executor failed; administrator recovery required")
            finally:
                self.jobs.task_done()

    def monitor(self):
        import time
        while True:
            time.sleep(5)
            if self.execution.acquire(blocking=False):
                try:
                    self.engine.health_tick()
                    from . import alerts
                    alerts.sample(self.engine)
                    from .resize import expire
                    expire(self.engine)
                except Exception:
                    pass  # Dataplane checks remain active; never turn uncertainty into deletion.
                finally:
                    self.execution.release()

    def serve(self):
        require(os.geteuid() == 0, "infrastructure daemon must be started explicitly by the local administrator")
        with self.engine.store.exclusive():
            self.path.parent.mkdir(mode=0o750, parents=True, exist_ok=True)
            parent = self.path.parent.lstat()
            require(parent.st_uid == 0 and parent.st_mode & 0o022 == 0, "IPC parent directory is not trusted")
            if self.path.exists():
                import stat
                st = self.path.lstat()
                require(stat.S_ISSOCK(st.st_mode) and st.st_uid == 0, "IPC path is not an owned socket")
                self.path.unlink()
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(self.path))
            os.chmod(self.path, 0o660)
            if self.group is not None:
                os.chown(self.path, 0, self.group)
            listener.listen(16)
            # Never resume mutating work merely because the daemon restarted. Reconcile and
            # require an explicit, authenticated resume of interrupted accepted operations.
            self.engine.store.interrupted()
            self.engine.inventory()
            from .network import restore_firewall
            if self.engine.store.policy() is not None:
                restore_firewall(self.engine.policy(), self.engine.backend)
            self.engine.recover()
            threading.Thread(target=self.worker, daemon=True).start()
            threading.Thread(target=self.monitor, daemon=True).start()
            while True:
                stream, _ = listener.accept()
                with stream:
                    stream.settimeout(10)
                    credentials = stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
                    _pid, uid, _gid = struct.unpack("3i", credentials)
                    try:
                        result = self.dispatch(read_frame(stream), uid)
                        response = {"schema_version": 1, "ok": True, "result": result}
                    except Rejected as exc:
                        response = {"schema_version": 1, "ok": False, "error": str(exc)}
                    except Exception:
                        response = {"schema_version": 1, "ok": False, "error": "local request failed; administrator diagnostics required"}
                    try:
                        stream.sendall(json.dumps(response, separators=(",", ":")).encode() + b"\n")
                    except (BrokenPipeError, TimeoutError):
                        pass


def request(socket_path, message):
    data = json.dumps(message, separators=(",", ":")).encode() + b"\n"
    require(len(data) <= MAX_FRAME, "request exceeds frame size")
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(600)
        stream.connect(str(socket_path))
        stream.sendall(data)
        response = read_frame(stream)
    require(response.get("ok"), response.get("error", "local request failed"))
    return response["result"]
