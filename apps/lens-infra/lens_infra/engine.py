"""Explicit, durable service operations; no cloud control-plane dependency."""

from __future__ import annotations

import json
import copy
import base64
import hashlib
import http.client
import sys
import traceback
import os
import time
import urllib.error
import urllib.request
from pathlib import Path

from .backend import allocations, run
from .policy import Rejected, authorize, deployment, keys, name, require


class Engine:
    def __init__(self, store, backend, ingress):
        self.store, self.backend, self.ingress = store, backend, ingress

    def policy(self):
        p = self.store.policy()
        require(p is not None, "no local infrastructure configuration has been applied")
        return p

    def inventory(self):
        observed = self.backend.inventory(self.store.policy())
        self.store.observe(observed)
        return self.store.inventory()

    def template(self, policy, request):
        template = copy.deepcopy(policy["templates"][request["template"]])
        if request.get("mode") == "server":
            template["profile"] = "private"
        if request.get("restore_backup"):
            backup = self.store.backup(request["restore_backup"])
            require(backup["state"] == "ready" and backup["environment"] == request["environment"], "backup is not ready or belongs to another environment")
            require(backup["detail"]["source_request"]["template"] == request["template"], "restore requires the original approved template")
            require(policy["classes"][request["class"]]["disk_gib"] >= backup["disk_gib"], "restore class is too small for the backup disk")
            template.update(image=str(self.store.root / "backups" / backup["id"] / "root.qcow2"), image_sha256=backup["sha256"], image_format="qcow2")
        return template

    def plan(self, request, uid):
        p = self.policy()
        deployment(p, request)
        authorize(p, uid, "deploy", request["environment"])
        if request.get("restore_backup"):
            authorize(p, uid, "recover", request["environment"])
        self.template(p, request)
        self.inventory()
        self.backend.assert_isolation(p)
        resources = allocations(p, request)
        self.backend.check_host_capacity(p, resources)
        self.store.check_capacity(p, request, resources)
        self.backend.verify_network(p["networks"][request["network"]], p)
        t = self.template(p, request)
        profile = p.get("build_profiles", {}).get(t.get("build_profile"), {})
        return {"configuration_revision": p["revision"], "request": request, "resources": resources,
                "strategy": "vm-create" if request.get("mode") == "server" else "replacement", "secrets": t.get("secrets", []),
                "migration_pool": p.get("migration_pool"),
                "build_profile": {"name": t.get("build_profile"), "lens_version": profile.get("lens", {}).get("version"), "logging_sha256": profile.get("logging", {}).get("sha256")}}

    def prepare(self, request, uid):
        plan = self.plan(request, uid)
        p = self.policy()
        template = self.template(p, request)
        profile = p.get("build_profiles", {}).get(template.get("build_profile"))
        artifacts = self.backend.prepare(template, profile)
        for secret in template.get("secrets", []):
            # Check availability without decrypting merely to plan.
            binding = p["secrets"][secret]
            self._trusted_file(binding["path"], max_bytes=1024 * 1024)
        if template.get("certificate"):
            path = p["certificates"][template["certificate"]]["path"]
            self._trusted_file(path, max_bytes=1024 * 1024)
            run(["/usr/bin/openssl", "x509", "-in", path, "-noout", "-checkend", "86400"])
        return plan | {"prepared": artifacts}

    @staticmethod
    def _trusted_file(path, max_bytes):
        import stat
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as f:
            st = os.fstat(f.fileno())
            require(stat.S_ISREG(st.st_mode) and st.st_uid == os.geteuid() and st.st_mode & 0o077 == 0,
                    "credential must be a private regular file owned by the infrastructure administrator")
            data = f.read(max_bytes + 1)
            require(len(data) <= max_bytes, "credential exceeds size limit")
            return data

    def submit(self, request, uid, expected_previous=None):
        p = self.policy()
        deployment(p, request)
        authorize(p, uid, "deploy", request["environment"])
        if request.get("restore_backup"):
            authorize(p, uid, "recover", request["environment"])
        # Idempotent replays must not fail because the original operation reserved capacity.
        try:
            existing = self.store.operation(request["operation_id"])
        except Rejected:
            existing = None
        if existing:
            require(existing["request"] == request, "operation ID was already used for a different request")
            return {"id": existing["id"], "state": existing["state"], "duplicate": True}
        self.prepare(request, uid)
        return self.store.reserve(p, request, allocations(p, request), uid, expected_previous=expected_previous)

    def service_change(self, action, selector, uid, plan=False):
        keys(selector, ["operation_id", "configuration_revision", "host_id", "environment", "service", "from_operation_id", "instances"], "service change")
        p = self.policy()
        for field in ("operation_id", "environment", "service", "from_operation_id"):
            name(selector.get(field))
        authorize(p, uid, "deploy", selector["environment"])
        require(selector.get("host_id") == p["host_id"] and selector.get("configuration_revision") == p["revision"], "service change requires the active host and policy revision")
        service = self.store.service(selector["environment"], selector["service"])
        require(service["current_id"] == selector["from_operation_id"], "active release changed; review the service before changing it")
        source = service["previous_id"] if action == "rollback" else service["current_id"]
        require(source, "no previous release exists")
        request = self.store.operation(source)["request"] | {
            "operation_id": selector["operation_id"], "configuration_revision": p["revision"], "host_id": p["host_id"]}
        request.pop("configuration_digest", None)
        if action == "drain":
            request["instances"] = 0
        elif action == "scale":
            request["instances"] = selector.get("instances")
        else:
            require(action == "rollback", "unknown service action")
        # Rollback is a new, health-gated build from the previous immutable template,
        # so it still works after old VMs have been retired. Policy must still allow it.
        return self.plan(request, uid) if plan else self.submit(request, uid, expected_previous=selector["from_operation_id"])

    def deliver(self, resource, request, policy):
        t = self.template(policy, request)
        self.backend.guest_exec(resource, ["/usr/bin/install", "-d", "-m", "0700", "/run/lens", "/run/lens/secrets", "/var/cache/lens/artifacts", "/opt/lens"])
        generations = {}
        for secret in t.get("secrets", []):
            binding = policy["secrets"][secret]
            self._trusted_file(binding["path"], max_bytes=1024 * 1024)
            if binding.get("encrypted", True):
                data = run(["/usr/bin/systemd-creds", "decrypt", "--name=" + secret, binding["path"], "-"])
            else:
                data = self._trusted_file(binding["path"], max_bytes=1024 * 1024)
            try:
                self.backend.guest_file(resource, "/run/lens/secrets/" + secret, data)
            finally:
                del data
            generations[secret] = binding["generation"]
        manifest = {"schema_version": 1, "release": request["release"], "kind": t["kind"],
                    "configuration_revision": policy["revision"], "service": request["service"],
                    "instance": resource["name"], "instance_id": resource["id"], "containers": copy.deepcopy(t.get("containers", [])), "secret_generations": generations,
                    "runner": copy.deepcopy(t.get("runner")), "allowed_port": t.get("port")}
        manifest.update(host_id=policy["host_id"], environment=request["environment"], network=request["network"],
                        **{"class": resource["detail"].get("class", request["class"])},
                        instance_configuration_revision=resource["detail"].get("configuration_revision", policy["revision"]))
        if t.get("build_profile"):
            profile = copy.deepcopy(policy["build_profiles"][t["build_profile"]])
            for component, artifact in profile.items():
                if component in ("ssh_keys", "users", "shell"):
                    continue
                destination = "/var/cache/lens/artifacts/" + ("lens.deb" if component == "lens" else "logging.conf")
                self.backend.guest_artifact(resource, destination, artifact["path"], artifact["sha256"])
                artifact["path"] = destination
            manifest["build_profile"] = profile | {"name": t["build_profile"]}
            manifest["ssh_enabled"] = bool(profile.get("ssh_keys") or profile.get("users"))
            if profile.get("shell"):
                for asset in ("server-shell.bash", "server_status.py"):
                    self.backend.guest_file(resource, "/opt/lens/" + asset, Path(__file__).with_name(asset).read_bytes())
        for container in manifest["containers"]:
            if container.get("archive"):
                destination = "/var/cache/lens/artifacts/" + container["name"] + ".tar"
                self.backend.guest_artifact(resource, destination, container["archive"], container["archive_sha256"])
                container["archive"] = destination
        if t["kind"] == "github-runner":
            source = manifest["runner"]["archive"]
            destination = "/var/cache/lens/artifacts/runner.tar.gz"
            self.backend.guest_artifact(resource, destination, source, manifest["runner"]["sha256"])
            manifest["runner"]["archive"] = destination
        helper = Path(__file__).with_name("guest.py").read_bytes()
        self.backend.guest_file(resource, "/opt/lens/workload.py", helper)
        self.backend.guest_file(resource, "/run/lens/manifest.json", json.dumps(manifest).encode())
        self.backend.guest_exec(resource, ["/usr/bin/python3", "/opt/lens/workload.py", "apply"], timeout=300)
        return generations

    def health(self, address, template):
        if template.get("profile", "private") == "private":
            return True
        url = "http://" + address + ":" + str(template["port"]) + template.get("health_path", "/health")
        # Ignore process proxy environment; checks stay on the authorised guest network.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        try:
            with opener.open(url, timeout=3) as result:
                if result.status != 200:
                    return False
            return self.protocol_health(address, template)
        except (OSError, urllib.error.URLError):
            return False

    @staticmethod
    def protocol_health(address, template):
        profile = template.get("profile", "private")
        if profile not in ("websocket", "jsonrpc"):
            return True
        connection = http.client.HTTPConnection(address, template["port"], timeout=3)
        try:
            if profile == "jsonrpc":
                body = json.dumps({"jsonrpc": "2.0", "id": "lens-health", "method": template.get("rpc_health_method", "lens.health"), "params": {}})
                connection.request("POST", template.get("rpc_path", "/"), body, {"Content-Type": "application/json"})
                response = connection.getresponse()
                raw = response.read(65537)
                if response.status != 200 or len(raw) > 65536:
                    return False
                data = json.loads(raw)
                return data.get("jsonrpc") == "2.0" and data.get("id") == "lens-health" and "error" not in data and (data.get("result") is True or data.get("result", {}) == {"ready": True})
            key = base64.b64encode(os.urandom(16)).decode()
            connection.request("GET", template.get("websocket_path", "/"), headers={"Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": key})
            # Keep ownership of the upgraded socket; HTTPConnection.getresponse may
            # detach it according to HTTP connection-close semantics for status 101.
            response = http.client.HTTPResponse(connection.sock)
            response.begin()
            expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            if response.status != 101 or response.getheader("Sec-WebSocket-Accept") != expected:
                return False
            mask, payload = os.urandom(4), b"lens"
            connection.sock.sendall(b"\x89\x84" + mask + bytes(v ^ mask[i % 4] for i, v in enumerate(payload)))
            try:
                return response.fp.read(6) == b"\x8a\x04lens"
            finally:
                response.close()
        except (OSError, ValueError, http.client.HTTPException, AttributeError):
            return False
        finally:
            connection.close()

    def observe(self, backends, template):
        # Bounded iterations also make the duration explicit in operation history.
        for _ in range(template.get("observe_seconds", 10)):
            require(all(self.health(b["address"], template) for b in backends), "replacement failed the local observation gate")
            time.sleep(1)

    def wait_ready(self, resource, address, template):
        deadline = time.monotonic() + template.get("startup_seconds", 180)
        passes = 0
        while time.monotonic() < deadline:
            guest_status = json.loads(self.backend.guest_exec(resource, ["/usr/bin/python3", "/opt/lens/workload.py", "status"]))
            passes = passes + 1 if guest_status.get("healthy") and self.health(address, template) else 0
            if passes >= 3:
                return
            time.sleep(2)
        raise Rejected("replacement did not pass readiness checks")

    def execute(self, operation_id):
        operation = self.store.operation(operation_id)
        if operation["state"] in ("succeeded", "failed", "cancelled"):
            return
        if operation["request"].get("kind") == "vm-action":
            from .vm import execute
            return execute(self, operation)
        request = operation["request"]
        p = self.store.policy_at(request["configuration_revision"])
        t = self.template(p, request)
        p["templates"][request["template"]] = t
        resources = operation["resources"]
        published = False
        promoted = False
        try:
            self.store.transition(operation_id, "creating")
            self.backend.assert_isolation(p)
            backends = []
            for resource in resources:
                if resource["state"] == "removed":
                    raise Rejected("operation has removed instances and cannot resume automatically")
                from .runner_provider import admission_lock
                with admission_lock(p):
                    self.inventory()
                    self.store.check_capacity(p, request, [])
                    self.backend.create(resource, request, p, start=request.get("start", True))
                if request.get("mode") == "server" and not request.get("start", True):
                    self.store.resource_detail(resource["id"], resource["detail"] | {"desired_power": "off", "template": request["template"], "release": request["release"], "build_profile": t.get("build_profile")})
                    self.store.resource_state(resource["id"], "stopped")
                    continue
                self.store.resource_state(resource["id"], "starting")
                self.backend.wait_agent(resource, t.get("startup_seconds", 180))
                self.backend.guest_exec(resource, ["/usr/bin/cloud-init", "status", "--wait"], timeout=t.get("startup_seconds", 180))
                generations = self.deliver(resource, request, p)
                address = self.backend.address(resource, p["networks"][request["network"]])
                detail = resource["detail"] | {"address": address, "secret_generations": generations,
                                               "release": request["release"], "template": request["template"], "build_profile": t.get("build_profile"), "migration_pool": p.get("migration_pool")}
                detail["provisioned"] = True
                self.store.resource_detail(resource["id"], detail)
                self.wait_ready(resource, address, t)
                self.store.resource_state(resource["id"], "ready")
                backends.append({"id": resource["id"], "address": address})
            self.store.transition(operation_id, "ready")
            if t.get("profile", "private") != "private":
                certificate = p.get("certificates", {}).get(t.get("certificate"), {}).get("path")
                if operation["previous_id"] and backends and t.get("canary_percent", 5):
                    previous = self.store.operation(operation["previous_id"])
                    old_policy = self.store.policy_at(previous["request"]["configuration_revision"])
                    old_template = old_policy["templates"][previous["request"]["template"]]
                    require(old_template.get("port") == t["port"], "canary replacement requires a stable endpoint port")
                    old_backends = [{"id": r["id"], "address": r["detail"]["address"], "weight": max(1, (100 - t.get("canary_percent", 5)) * len(backends))} for r in previous["resources"] if r["state"] == "ready"]
                    if old_backends:
                        weighted = [b | {"weight": t.get("canary_percent", 5) * len(old_backends)} for b in backends]
                        self.store.transition(operation_id, "canary")
                        self.ingress.publish(request, t, old_backends + weighted, certificate)
                        published = True
                        self.observe(backends, t)
                self.ingress.publish(request, t, backends, certificate)
                published = True
                self.store.transition(operation_id, "observing")
                self.observe(backends, t)
            self.store.promote(operation_id)
            promoted = True
            self.store.transition(operation_id, "draining")
            if operation["previous_id"]:
                self._drain_previous(operation["previous_id"], request, t)
            self.store.transition(operation_id, "complete", "succeeded")
        except Exception as exc:
            # Administrator diagnostics retain locations, never exception values,
            # local variables, raw subprocess output or credential contents.
            diagnostic = {"event": "deployment-failed", "operation_id": operation_id, "exception_type": type(exc).__name__,
                          "frames": [{"file": Path(f.filename).name, "function": f.name, "line": f.lineno} for f in traceback.extract_tb(exc.__traceback__)]}
            print(json.dumps(diagnostic), file=sys.stderr)
            # Values and native diagnostic output never become operation errors.
            error = str(exc) if isinstance(exc, Rejected) else "local deployment failed; inspect administrator diagnostics"
            if promoted:
                # Cleanup cannot invalidate a healthy promoted release. Some old VMs may
                # already be removed; restoring that release would create an outage.
                self.store.transition(operation_id, "cleanup-pending", "succeeded", "healthy release active; previous instances retained for administrator cleanup")
                return
            if published and operation["previous_id"]:
                try:
                    self.restore(operation["previous_id"])
                except Exception:
                    error += "; previous ingress configuration requires local recovery"
            elif published:
                try:
                    self.ingress.publish(request, t, [], certificate)
                except Exception:
                    error += "; initial ingress requires local recovery"
            self.store.transition(operation_id, "failed", "failed", error)
            # Failed replacement VMs are retained and continue consuming reservations.

    def _drain_previous(self, previous_id, request, template):
        previous = self.store.operation(previous_id)
        deadline = time.monotonic() + template.get("drain_seconds", 30)
        service = request["environment"] + "/" + request["service"]
        while time.monotonic() < deadline:
            if self.ingress.connections(service, retired=True) == 0:
                break
            time.sleep(1)
        for resource in previous["resources"]:
            if resource["state"] == "removed":
                continue
            # A runner with an active job is retained, never terminated by a scale/deploy operation.
            old_policy = self.store.policy_at(previous["request"]["configuration_revision"])
            old_template = old_policy["templates"][previous["request"]["template"]]
            if old_template["kind"] == "github-runner":
                status = json.loads(self.backend.guest_exec(resource, ["/usr/bin/python3", "/opt/lens/workload.py", "status"]))
                if status.get("busy") or status.get("accepting_jobs", True):
                    # An idle snapshot is racy: the listener may receive another job.
                    # Preserve the VM until the operator has disabled job admission.
                    self.store.resource_state(resource["id"], "drain-pending")
                    continue
            self.backend.remove(resource)
            self.store.resource_state(resource["id"], "removed")

    def cleanup(self, operation_id, uid):
        operation = self.store.operation(operation_id)
        authorize(self.policy(), uid, "deploy", operation["request"]["environment"])
        require(operation_id not in self.store.current_operations(), "drain or replace the active service before removing its VMs")
        require(operation["state"] in ("succeeded", "failed", "cancelled"), "resolve or cancel the operation before cleanup")
        p = self.store.policy_at(operation["request"]["configuration_revision"])
        self._drain_previous(operation_id, operation["request"], p["templates"][operation["request"]["template"]])
        remaining = [r["id"] for r in self.store.operation(operation_id)["resources"] if r["state"] != "removed"]
        self.store.transition(operation_id, "cleanup-pending" if remaining else "retired", operation["state"])
        return {"id": operation_id, "remaining": remaining, "disks_retained": True}

    def recover(self):
        """Restore only recorded, desired managed instances; never adopt unknown objects."""
        current = self.store.policy()
        if current is None:
            return []
        self.backend.assert_isolation(current)
        restored = []
        for operation_id in self.store.current_operations():
            operation = self.store.operation(operation_id)
            request = operation["request"]
            self.store.check_capacity(current, request, [])
            p = self.store.policy_at(request["configuration_revision"])
            t = self.template(p, request)
            backends = []
            for resource in operation["resources"]:
                if resource["state"] == "removed":
                    continue
                if resource["detail"].get("desired_power") == "off":
                    continue
                self.backend.ensure_running(resource)
                self.backend.wait_agent(resource, t.get("startup_seconds", 180))
                generations = self.deliver(resource, request, p)
                address = self.backend.address(resource, p["networks"][request["network"]])
                self.store.resource_detail(resource["id"], resource["detail"] | {"address": address, "secret_generations": generations})
                try:
                    self.wait_ready(resource, address, t)
                    self.store.resource_state(resource["id"], "ready")
                    backends.append({"id": resource["id"], "address": address})
                except Rejected:
                    self.store.resource_state(resource["id"], "unready")
            if t.get("profile", "private") != "private":
                cert = p.get("certificates", {}).get(t.get("certificate"), {}).get("path")
                self.ingress.publish(request, t, backends, cert)
            restored.append(operation_id)
        return restored

    def health_tick(self):
        """HAProxy and guest systemd also check health independently of this loop."""
        for operation_id in self.store.current_operations():
            operation = self.store.operation(operation_id)
            req = operation["request"]
            p = self.store.policy_at(req["configuration_revision"])
            template = self.template(p, req)
            healthy = []
            changed = False
            for resource in operation["resources"]:
                if resource["state"] == "removed":
                    continue
                if resource["detail"].get("desired_power") == "off":
                    continue
                detail = resource["detail"]
                address = detail.get("address")
                if template.get("profile", "private") == "private":
                    try:
                        good = json.loads(self.backend.guest_exec(resource, ["/usr/bin/python3", "/opt/lens/workload.py", "status"])).get("healthy", False)
                    except Exception:
                        good = None  # Unknown is not proof of a stopped workload.
                else:
                    good = bool(address and self.health(address, template))
                failures = detail.get("health_failures", 0)
                failures = 0 if good else failures + 1 if good is False else failures
                detail = detail | {"health": "healthy" if good else "unhealthy" if good is False else "unknown",
                                   "health_failures": failures, "checked_at": time.time()}
                self.store.resource_detail(resource["id"], detail)
                next_state = "unready" if failures >= 3 else "ready" if good else resource["state"]
                changed = changed or next_state != resource["state"]
                self.store.resource_state(resource["id"], next_state)
                if next_state == "ready" and address:
                    healthy.append({"id": resource["id"], "address": address})
            if changed and template.get("profile", "private") != "private":
                cert = p.get("certificates", {}).get(template.get("certificate"), {}).get("path")
                self.ingress.publish(req, template, healthy, cert)

    def restore(self, operation_id):
        old = self.store.operation(operation_id)
        p = self.store.policy_at(old["request"]["configuration_revision"])
        template = p["templates"][old["request"]["template"]]
        backends = [{"id": r["id"], "address": r["detail"]["address"]} for r in old["resources"] if r["state"] != "removed"]
        require(backends, "previous release has no retained instances; an explicit replacement deployment is needed")
        if template.get("profile", "private") != "private":
            cert = p.get("certificates", {}).get(template.get("certificate"), {}).get("path")
            self.ingress.publish(old["request"], template, backends, cert)
        self.store.promote(operation_id)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise Rejected("health checks cannot redirect outside the approved service path")
