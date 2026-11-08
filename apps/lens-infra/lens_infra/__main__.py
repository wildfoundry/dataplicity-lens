"""Command-line interface. Infrastructure policy and execution stay on the host."""

import argparse
import grp
import json
import os
from pathlib import Path
import sys
import tomllib

from .backend import Libvirt
from .daemon import Server, read_frame, request
from .engine import Engine
from .ingress import Ingress
from .policy import Rejected, load, require
from .store import Store


def main(argv=None):
    parser = argparse.ArgumentParser(prog="lens-infra", description="Opt-in local Lens infrastructure control; no cloud or reporting connection.")
    parser.add_argument("--socket", default="/run/lens-infra/control.sock")
    parser.add_argument("--json", action="store_true")
    sub = parser.add_subparsers(dest="command", required=True)
    config = sub.add_parser("config").add_subparsers(dest="action", required=True)
    for action in ("validate", "apply"):
        p = config.add_parser(action)
        p.add_argument("file")
        if action == "apply":
            p.add_argument("--yes", action="store_true")
            p.add_argument("--local-state", help="Administrator bootstrap/offline policy apply while the daemon is stopped")
    deploy = sub.add_parser("deploy").add_subparsers(dest="action", required=True)
    for action in ("plan", "prepare", "apply"):
        p = deploy.add_parser(action)
        p.add_argument("file")
        if action == "apply":
            p.add_argument("--yes", action="store_true")
    network = sub.add_parser("network").add_subparsers(dest="action", required=True)
    for action in ("plan", "apply"):
        p = network.add_parser(action)
        p.add_argument("file")
        if action == "apply":
            p.add_argument("--yes", action="store_true")
    operation = sub.add_parser("operation").add_subparsers(dest="action", required=True)
    for action in ("status", "resume", "cancel", "cleanup"):
        p = operation.add_parser(action)
        p.add_argument("operation_id")
        if action in ("resume", "cancel", "cleanup"):
            p.add_argument("--yes", action="store_true")
    service = sub.add_parser("service").add_subparsers(dest="action", required=True)
    for action in ("scale", "drain", "rollback"):
        p = service.add_parser(action)
        p.add_argument("file", help="TOML selector including expected current operation and active host/policy")
        p.add_argument("--plan", action="store_true")
        p.add_argument("--yes", action="store_true")
    inventory = sub.add_parser("inventory")
    runner = sub.add_parser("runner").add_subparsers(dest="action", required=True)
    for action in ("hold", "resume", "status"):
        p = runner.add_parser(action)
        if action == "status":
            p.add_argument("slot", type=int)
        else:
            p.add_argument("file", help="TOML selector with slot, expected VM UUID, host and policy revision")
            p.add_argument("--yes", action="store_true")
    inventory.add_argument("--direct", action="store_true", help="Read existing runtimes with the invoking user's permissions; no daemon needed")
    inventory.add_argument("--record", help="Persist direct inventory as protected resources in this administrator-owned directory")
    sub.add_parser("status")
    sub.add_parser("metrics")
    sub.add_parser("events")
    sub.add_parser("rpc", help="Restricted SSH forced-command transport; accepts one JSON request on stdin")
    worker = sub.add_parser("runner-controller", help="Adopted per-slot controller; local administrator/systemd entry point")
    worker.add_argument("slot", type=int)
    server = sub.add_parser("serve")
    server.add_argument("--state-dir", default="/var/lib/lens-infra")
    server.add_argument("--storage-dir", default="/var/lib/libvirt/images/lens-infra")
    server.add_argument("--group", default="lens-infra")
    args = parser.parse_args(argv)
    if args.command == "runner-controller":
        require(os.geteuid() == 0, "the runner controller requires the local administrator")
        from .runner_controller import main as run_controller
        run_controller([str(args.slot)])
        return 0
    if args.command == "serve":
        store = Store(args.state_dir)
        backend = Libvirt(args.state_dir, storage_directory=args.storage_dir)
        engine = Engine(store, backend, Ingress(args.state_dir))
        group = grp.getgrnam(args.group).gr_gid
        Server(engine, args.socket, group).serve()
        return 0
    if args.command == "inventory" and args.direct:
        backend = Libvirt(args.record or "/var/lib/lens-infra")
        observed = backend.inventory()
        if args.record:
            store = Store(args.record)
            store.observe(observed)
            result = store.inventory()
        else:
            result = [r | {"owner": "external", "protected": True} for r in observed]
    elif args.command == "network":
        from .network import provision
        policy = load(args.file)
        if args.action == "apply":
            require(args.yes and os.geteuid() == 0, "network changes require local administrator authority and --yes")
        result = provision(policy, Libvirt("/var/lib/lens-infra"), apply=args.action == "apply")
    elif args.command == "config":
        policy = load(args.file)
        if args.action == "validate":
            result = {"valid": True, "revision": policy["revision"], "host_id": policy["host_id"]}
        else:
            require(args.yes, "configuration changes require --yes")
            if args.local_state:
                require(os.geteuid() == 0, "offline policy bootstrap requires the local administrator")
                store = Store(args.local_state)
                with store.exclusive():
                    result = store.apply_policy(policy, os.geteuid())
            else:
                result = request(args.socket, {"method": "config.apply", "payload": policy})
    elif args.command == "deploy":
        with Path(args.file).open("rb") as f:
            payload = tomllib.load(f)
        if args.action == "apply":
            require(args.yes, "deployment requires --yes")
        result = request(args.socket, {"method": "deploy." + args.action, "payload": payload})
    elif args.command == "service":
        with Path(args.file).open("rb") as f:
            payload = tomllib.load(f)
        require(args.plan or args.yes, "service changes require --yes or --plan")
        result = request(args.socket, {"method": "service." + args.action + (".plan" if args.plan else ""), "payload": payload})
    elif args.command == "runner":
        if args.action == "status":
            payload = {"slot": args.slot}
        else:
            require(args.yes, "runner changes require --yes")
            with Path(args.file).open("rb") as f:
                payload = tomllib.load(f)
        result = request(args.socket, {"method": "runner." + args.action, "payload": payload})
    elif args.command == "operation":
        if args.action in ("resume", "cancel", "cleanup"):
            require(args.yes, "operation changes require --yes")
        result = request(args.socket, {"method": "operation." + args.action, "payload": {"operation_id": args.operation_id}})
    elif args.command == "rpc":
        line = sys.stdin.buffer.readline(1024 * 1024 + 1)
        require(len(line) <= 1024 * 1024, "request exceeds frame size")
        envelope = json.loads(line)
        # The SSH account's local UID is authenticated by the daemon. Client identity
        # fields, environment variables and SSH_ORIGINAL_COMMAND confer no authority.
        require(envelope.get("method") != "config.apply", "the CI transport cannot replace local policy")
        result = request(args.socket, envelope)
    else:
        result = request(args.socket, {"method": args.command})
    if args.command == "metrics":
        print(result, end="")
    else:
        print(json.dumps({"schema_version": 1, "result": result}, indent=None if args.command == "rpc" else 2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Rejected, OSError, ValueError) as exc:
        # Policy errors contain no secret values; native command output is never echoed.
        message = str(exc) if isinstance(exc, Rejected) else type(exc).__name__ + ": local operation failed"
        print(json.dumps({"schema_version": 1, "error": message}), file=sys.stderr)
        sys.exit(1)
