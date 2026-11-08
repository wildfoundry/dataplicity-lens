"""Human terminal views. RPC, JSON and metric contracts remain machine-readable."""

import os
import shutil
import sys
import unicodedata
from datetime import datetime, timezone


def clean(value):
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "yes" if value else "no"
    # Collected names/errors must never become terminal escape sequences or bidi controls.
    return "".join(" " if unicodedata.category(c).startswith("C") else c for c in str(value))


def cells(value):
    return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in ("W", "F") else 1 for c in value)


def chunks(value, width):
    result, line, used = [], "", 0
    for c in value:
        size = cells(c)
        if used + size > width and line:
            result.append(line)
            line, used = "", 0
        line += c
        used += size
    return result + [line]


def memory(value):
    if value is None:
        return "—"
    return f"{value / 1024:g} GiB" if value >= 1024 else f"{value:g} MiB"


def timestamp(value):
    try:
        return datetime.fromtimestamp(float(value), timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")
    except (TypeError, ValueError, OverflowError, OSError):
        return "—"


class View:
    def __init__(self, stream=None, width=None, color=None, unicode=None):
        self.stream = stream or sys.stdout
        self.width = max(20, min(160, width or shutil.get_terminal_size((100, 24)).columns))
        tty = self.stream.isatty()
        self.color = color if color is not None else tty and "NO_COLOR" not in os.environ and os.environ.get("TERM") != "dumb"
        encoding = getattr(self.stream, "encoding", None) or "utf-8"
        try:
            "╭─◆—".encode(encoding)
            supported = True
        except (UnicodeError, LookupError):
            supported = False
        self.unicode = unicode if unicode is not None else supported and os.environ.get("TERM") != "dumb"
        self.lines = []

    def safe(self, value):
        value = clean(value)
        if not self.unicode:
            value = value.replace("—", "-").replace("·", "|").replace("→", "->")
            value = value.encode("ascii", "replace").decode()
        return value

    def paint(self, value, role):
        if not self.color:
            return value
        # Match Lens's magenta brand/cyan information palette; keep terminal background.
        code = {"brand": "1;35", "title": "1", "muted": "90", "info": "36", "good": "32", "warn": "33", "bad": "1;31"}.get(role, "0")
        return f"\x1b[{code}m{value}\x1b[0m"

    def line(self, value="", role=None):
        for part in chunks(self.safe(value), self.width - 2):
            self.lines.append("  " + (self.paint(part, role) if role else part))

    def header(self, title, context=None):
        self.line()
        self.line("DATAPLICITY / LENS", "brand")
        self.line(title, "title")
        if context:
            self.line(context, "info")
        self.line(("─" if self.unicode else "-") * (self.width - 4), "muted")

    def section(self, title):
        self.line()
        self.line(title, "title")

    def fields(self, pairs):
        pairs = [(self.safe(k), self.safe(v)) for k, v in pairs if v is not None]
        if not pairs:
            return
        label = min(24, max(cells(k) for k, _ in pairs))
        for key, value in pairs:
            prefix = key + " " * max(0, label - cells(key)) + "  "
            if self.width < 52:
                self.line(key, "muted")
                self.line("  " + value)
            else:
                for index, part in enumerate(chunks(value, max(1, self.width - 2 - cells(prefix)))):
                    self.line((prefix if index == 0 else " " * cells(prefix)) + part)

    def table(self, headers, rows, state_column=None):
        if not rows:
            self.line("None.", "muted")
            return
        rows = [[self.safe(v) for v in row] for row in rows]
        # Stack on small terminals; never truncate UUIDs, operation IDs or names.
        if self.width < 68:
            for index, row in enumerate(rows):
                if index:
                    self.line()
                self.fields(zip(headers, row))
            return
        room = self.width - 2 - 2 * (len(headers) - 1)
        widths = [max(cells(self.safe(h)), *(min(44, cells(row[i])) for row in rows)) for i, h in enumerate(headers)]
        while sum(widths) > room:
            largest = max(range(len(widths)), key=lambda i: widths[i])
            widths[largest] -= 1
        def pad(value, width):
            return value + " " * max(0, width - cells(value))
        self.line("  ".join(pad(self.safe(h), widths[i]) for i, h in enumerate(headers)), "muted")
        self.line("  ".join(("─" if self.unicode else "-") * w for w in widths), "muted")
        for row in rows:
            wrapped = [chunks(value, width) for value, width in zip(row, widths)]
            for index in range(max(map(len, wrapped))):
                values = []
                for col, parts in enumerate(wrapped):
                    value = pad(parts[index] if index < len(parts) else "", widths[col])
                    if col == state_column:
                        value = self.paint(value, state_role(row[col]))
                    values.append(value)
                # Already measured before ANSI styling.
                self.lines.append("  " + "  ".join(values).rstrip())

    def resources(self, rows):
        self.table(["VM / resource", "State", "CPU", "Memory", "Disk", "Access"], [
            [r.get("service") or r.get("name") or r["id"], r.get("state") or "planned", r.get("vcpus", "—"), memory(r.get("memory_mib")),
             f"{r['disk_gib']} GiB" if "disk_gib" in r else "—", "protected" if r.get("protected") else "managed"] for r in rows], state_column=1)
        # Keep exact selectors available without forcing people to decipher UUID names.
        for r in rows:
            if not r.get("protected"):
                self.fields([("VM", r.get("service") or r.get("name")), ("Instance", r["id"]), ("Address", r.get("detail", {}).get("address"))])

    def finish(self):
        return "\n".join(self.lines) + "\n\n"


def state_role(state):
    if state in ("ready", "succeeded", "running", "held"):
        return "good"
    if state in ("failed", "unhealthy"):
        return "bad"
    return "warn"


def render(command, action, result, *, planned=False, stream=None, **options):
    v = View(stream, **options)
    title = " ".join(x for x in (command, action) if x).replace("-", " ").title()
    context = None
    if isinstance(result, dict):
        host = result.get("host_id") or result.get("request", {}).get("host_id")
        revision = result.get("configuration_revision", result.get("revision"))
        context = "  ·  ".join(str(x) for x in (host, f"configuration {revision}" if revision is not None else None) if x)
    v.header(title, context)
    if command == "status":
        rows = result.get("resources", [])
        managed = [r for r in rows if not r.get("protected") and r.get("state") != "removed"]
        protected = [r for r in rows if r.get("protected")]
        v.line(f"{len(managed)} managed  ·  {len(protected)} protected  ·  {len(result.get('services', []))} services")
        v.line("State reflects the controller's recorded observation.", "muted")
        v.section("Services")
        v.table(["Service", "Release", "Revision", "Replicas", "Current operation"], [
            [s["id"], s.get("applied_release"), s.get("applied_configuration_revision"), s.get("desired_instances"), s.get("current_id")] for s in result.get("services", [])])
        v.section("Managed VMs")
        v.resources(managed)
        if protected:
            v.section("Protected workloads")
            v.line(f"{len(protected)} existing workloads remain reserved and protected.")
            v.line("Inspect: lens-infra inventory", "muted")
        removed = sum(r.get("state") == "removed" for r in rows)
        if removed:
            v.line(f"{removed} retired definition(s); retained disks still consume storage.", "muted")
    elif command == "inventory":
        v.resources(result)
        v.line("Protected resources cannot be changed through generic VM controls.", "muted")
    elif command == "events":
        v.table(["Time (UTC)", "Operation", "Actor UID", "Event"], [
            [timestamp(r.get("timestamp")), r.get("operation_id"), r.get("actor"), r.get("event")] for r in result])
        v.line("Audit detail is available with --json.", "muted")
    elif command == "vm" and action == "backups":
        v.table(["Backup", "State", "Source instance", "Disk", "Created"], [
            [b["id"], b["state"], b.get("resource_id"), f"{b['disk_gib']} GiB", timestamp(b.get("created_at"))] for b in result], state_column=1)
        v.line("Offline disk copies. Backups continue to reserve storage.", "muted")
    elif command == "network":
        v.line("Applied" if result.get("applied") else "Plan only · no changes applied", "info")
        plans = result.get("networks", [])
        v.table(["Network", "Action", "Bridge", "Subnet"], [[p.get("network"), p.get("action"), p.get("bridge"), p.get("cidr")] for p in plans])
        v.line(result.get("notice", ""), "warn")
    elif command == "runner":
        if result is None:
            v.line("No local intent recorded for this adopted slot.", "muted")
        else:
            v.fields([("Slot", result.get("slot")), ("Intent", result.get("mode")), ("Stage", result.get("stage")),
                      ("VM UUID", result.get("expected_uuid")), ("Generation", result.get("generation")),
                      ("Updated", timestamp(result.get("updated_at", result.get("requested_at"))))])
    elif isinstance(result, dict) and ("request" in result or planned):
        req = result.get("request", {})
        if "state" in result:
            v.line(result["state"].upper() + (" · " + str(result["stage"]) if result.get("stage") else ""), state_role(result["state"]))
        elif planned or action in ("plan", "prepare"):
            v.line("Prepared · no VMs changed" if "prepared" in result else "Plan only · no changes applied", "info")
        v.fields([("Operation", result.get("id", req.get("operation_id"))), ("Host", req.get("host_id")),
                  ("Instance", req.get("instance_id", result.get("instance_id"))),
                  ("Environment", req.get("environment", result.get("environment"))), ("Service / VM", req.get("service", req.get("name"))),
                  ("Action", req.get("action", result.get("action", result.get("strategy")))), ("Class", req.get("new_class", req.get("class"))),
                  ("Template", req.get("template")), ("Network", req.get("network")), ("Release", req.get("release"))])
        if result.get("error"):
            v.section("Needs attention")
            v.line(result["error"], "bad")
        if result.get("resources"):
            v.section("VMs")
            v.resources(result["resources"])
        if result.get("build_profile", {}).get("name"):
            v.fields([("Build profile", result["build_profile"]["name"]), ("Lens version", result["build_profile"].get("lens_version"))])
        if result.get("secrets"):
            v.fields([("Local bindings", ", ".join(result["secrets"]))])
        if result.get("planned_shutdown_required"):
            v.line("Requires an explicitly shut down VM before applying.", "warn")
        if result.get("disks_retained"):
            v.line("Disk retained; storage remains reserved.", "warn")
        if result.get("backup_consistency"):
            v.fields([("Backup consistency", result["backup_consistency"])])
        if req.get("operation_id") and "state" not in result:
            v.line("Use --yes to apply this request.", "muted")
    elif isinstance(result, dict):
        v.fields([(label, result.get(key)) for key, label in (
            ("id", "Operation"), ("state", "State"), ("revision", "Configuration"), ("valid", "Valid"),
            ("changed", "Changed"), ("duplicate", "Already submitted"), ("queued", "Queued"),
            ("resources_retained", "Resources retained"), ("digest", "Digest"))])
        if result.get("state") == "accepted" or result.get("queued"):
            v.line("Accepted locally; execution continues after this command exits.", "info")
        if result.get("id"):
            v.line("Follow: lens-infra operation status " + str(result["id"]), "muted")
    else:
        v.line("Completed.")
    return v.finish()


def render_error(message, stream=None):
    v = View(stream)
    v.header("Request could not be completed")
    v.line(message, "bad")
    return v.finish()
