#!/usr/bin/env python3
"""Qualify real VMs inside disposable mount+network namespaces.

The caller supplies unpacked distribution tools and a checksum-verified Ubuntu cloud
image. Nothing is installed on the host. Existing VMs, containers, networks and services
are hidden from the test's libvirt instance. Run through `unshare --mount --net`.
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def command(*argv, **kwargs):
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, **kwargs).stdout


def mounted_root(args, scratch):
    if os.geteuid() != 0 or sys.platform != "linux":
        raise SystemExit("requires root inside disposable namespaces")
    for namespace in ("net", "mnt"):
        if Path(f"/proc/self/ns/{namespace}").stat().st_ino == Path(f"/proc/1/ns/{namespace}").stat().st_ino:
            raise SystemExit("REFUSED: use unshare --mount --net; the host namespace must not be modified")
    command("/usr/bin/mount", "--make-rprivate", "/")
    for target in ("/etc", "/usr/bin", "/usr/sbin", "/usr/lib", "/usr/share", "/opt"):
        ident = target.replace("/", "_")
        upper, work = scratch / (ident + "-upper"), scratch / (ident + "-work")
        upper.mkdir()
        work.mkdir()
        command("/usr/bin/mount", "-t", "overlay", "overlay", "-o", f"lowerdir={target},upperdir={upper},workdir={work}", target)
    for target in ("/run", "/var/lib", "/var/log", "/root", "/var/snap"):
        if Path(target).exists():
            command("/usr/bin/mount", "-t", "tmpfs", "-o", "size=512m", "tmpfs", target)
    root = Path("/opt/lens-qualification-tools")
    root.mkdir(mode=0o755)
    command("/usr/bin/mount", "--bind", str(args.tools / "root"), str(root))
    libs = str(root / "usr/lib/x86_64-linux-gnu") + ":" + str(root / "usr/lib")
    for relative in ("usr/share/libvirt", "usr/share/qemu", "usr/share/seabios", "usr/lib/libvirt"):
        source = root / relative
        if source.exists():
            destination = Path("/") / relative
            destination.mkdir(parents=True, exist_ok=True)
            command("/usr/bin/mount", "--bind", str(source), str(destination))
    drivers = root / "usr/lib/x86_64-linux-gnu/libvirt/connection-driver"
    target = Path("/usr/lib/x86_64-linux-gnu/libvirt/connection-driver")
    target.mkdir(parents=True, exist_ok=True)
    command("/usr/bin/mount", "--bind", str(drivers), str(target))
    py = Path("/usr/lib/python3/dist-packages")
    py.mkdir(parents=True, exist_ok=True)
    for source in (root / "usr/lib/python3/dist-packages").glob("libvirt*"):
        destination = py / source.name
        if source.is_dir():
            shutil.copytree(source, destination, dirs_exist_ok=True)
        else:
            shutil.copy2(source, destination)
    import shlex
    for executable in ("usr/bin/virsh", "usr/bin/qemu-img", "usr/bin/cloud-localds", "usr/bin/genisoimage",
                       "usr/bin/qemu-system-x86_64", "usr/sbin/dnsmasq"):
        source = root / executable
        if not source.exists():
            raise SystemExit("missing unpacked qualification tool: " + executable)
        destination = Path("/") / executable
        extra = ""
        if executable.endswith("qemu-system-x86_64"):
            extra = " -L /usr/share/qemu -bios /usr/share/seabios/bios-256k.bin"
        destination.write_text("#!/bin/sh\nexport LD_LIBRARY_PATH=" + shlex.quote(libs) + "\nexec " + shlex.quote(str(source)) + extra + ' "$@"\n')
        destination.chmod(0o755)
    libvirt = Path("/etc/libvirt")
    libvirt.mkdir(parents=True, exist_ok=True)
    filters = root / "usr/share/libvirt/nwfilter"
    if filters.exists():
        shutil.copytree(filters, libvirt / "nwfilter", dirs_exist_ok=True)
    import shlex
    for binary in ("iptables", "ip6tables", "ebtables"):
        source = root / "usr/sbin" / (binary + "-nft")
        if source.exists():
            destination = Path("/usr/sbin") / binary
            if destination.is_symlink():
                destination.unlink()
            destination.write_text("#!/bin/sh\nexport LD_LIBRARY_PATH=" + shlex.quote(libs) + "\nexec " + shlex.quote(str(source)) + ' "$@"\n')
            destination.chmod(0o755)
    # Distribution drivers resolve compiled-in service accounts before loading qemu.conf.
    # These files are in the private /etc overlay, never the host account database.
    passwd = Path("/etc/passwd")
    existing = passwd.read_text()
    for account, uid, gid in (("libvirt-qemu", 65534, 992), ("libvirt-dnsmasq", 65533, 65533)):
        if not any(line.startswith(account + ":") for line in existing.splitlines()):
            existing += f"{account}:x:{uid}:{gid}:Qualification only:/nonexistent:/usr/sbin/nologin\n"
    passwd.write_text(existing)
    group = Path("/etc/group")
    existing = group.read_text()
    for account, gid in (("libvirt-qemu", 992), ("libvirt-dnsmasq", 65533), ("libvirt", 65532)):
        if not any(line.startswith(account + ":") for line in existing.splitlines()):
            existing += f"{account}:x:{gid}:\n"
    group.write_text(existing)
    (libvirt / "libvirtd.conf").write_text('listen_tcp=0\nlisten_tls=0\nauth_unix_rw="none"\nauth_unix_ro="none"\n')
    (libvirt / "qemu.conf").write_text('user="nobody"\ngroup="kvm"\nsecurity_driver="none"\nsecurity_default_confined=0\nsecurity_require_confined=0\ncgroup_controllers=[]\nlock_manager="nop"\nstdio_handler="file"\n')
    (libvirt / "network.conf").write_text('firewall_backend="nftables"\n')
    storage = scratch / "vm-storage"
    storage.mkdir(mode=0o711)
    storage.chmod(0o711)
    state = scratch / "state"
    state.mkdir(mode=0o700)
    Path("/var/lib/lens-infra").mkdir()
    command("/usr/bin/mount", "--bind", str(state), "/var/lib/lens-infra")
    Path("/var/lib/libvirt/images/lens-infra").mkdir(parents=True)
    command("/usr/bin/mount", "--bind", str(storage), "/var/lib/libvirt/images/lens-infra")
    Path("/run/libvirt").mkdir()
    Path("/run/lens-infra-qualification").write_text("private-libvirt-qualification\n")
    command("/usr/sbin/ip", "link", "set", "lo", "up")
    env = dict(os.environ, LD_LIBRARY_PATH=libs, LIBVIRT_DRIVER_DIR=str(drivers),
               LIBVIRT_CPU_MAP_DIR="/usr/share/libvirt/cpu_map", LIBVIRT_SCHEMAS_DIR="/usr/share/libvirt/schemas")
    # Re-exec so the loader sees the private library path before importing native bindings.
    argv = ["python3", str(Path(__file__).resolve()), "--inside", "--tools", str(args.tools),
            "--image", str(args.image), "--scratch", str(scratch)]
    if args.lens_package:
        argv += ["--lens-package", str(args.lens_package)]
    if args.build_only:
        argv += ["--build-only"]
    os.execve("/usr/bin/python3", argv, env)


def qualify(args):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "apps/lens-infra"))
    from lens_infra.backend import Libvirt, allocations, file_digest, run
    from lens_infra.engine import Engine
    from lens_infra.ingress import Ingress
    from lens_infra.network import provision
    from lens_infra.policy import validate
    from lens_infra.store import Store
    import libvirt

    log = (args.scratch / "libvirtd.log").open("wb")
    daemon = subprocess.Popen([str(args.tools / "root/usr/sbin/libvirtd"), "-f", "/etc/libvirt/libvirtd.conf", "-p", "/run/libvirt/qualification.pid"], stdout=log, stderr=log)
    class DiagnosedLibvirt(Libvirt):
        artifact_transfers = 0

        def guest_stream(self, resource, destination, reader):
            if destination.startswith('/var/cache/lens/artifacts/'):
                self.artifact_transfers += 1
            return super().guest_stream(resource, destination, reader)

        def guest_exec(self, resource, argv, **kwargs):
            try:
                return super().guest_exec(resource, argv, **kwargs)
            except Exception:
                # Fixed diagnostic only for these disposable, dummy-secret appliance VMs.
                code = 'import runpy,traceback,subprocess; p=subprocess.run(["/usr/sbin/nft","--check","-f","/run/lens/firewall.nft"],capture_output=True); print(p.returncode,p.stderr.decode()); m=runpy.run_path("/opt/lens/workload.py"); m["apply"]()'
                try:
                    result = self.guest(resource, {"execute": "guest-exec", "arguments": {"path": "/usr/bin/python3", "arg": ["-c", code], "capture-output": True}})
                    for _ in range(10):
                        status = self.guest(resource, {"execute": "guest-exec-status", "arguments": {"pid": result["pid"]}})
                        if status.get("exited"):
                            (args.scratch / "guest-diagnostic.json").write_text(json.dumps(status))
                            break
                        time.sleep(.5)
                except Exception:
                    pass
                raise
    backend = DiagnosedLibvirt("/var/lib/lens-infra")
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if daemon.poll() is not None:
                raise RuntimeError("private libvirt failed; inspect " + str(args.scratch / "libvirtd.log"))
            try:
                backend.virsh("list", "--all")
                break
            except Exception:
                time.sleep(.2)
        print("private libvirt ready", flush=True)
        guest_packages = [next(args.tools.glob(pattern)) for pattern in ("qemu-guest-agent_*.deb", "liburing2_*.deb", "ubuntu-virt_*.deb", "ubuntu-helper-virt-hwe_*.deb")]
        fixture_http = 'from http.server import BaseHTTPRequestHandler,HTTPServer\nclass H(BaseHTTPRequestHandler):\n def log_message(self,*a):pass\n def do_GET(self):\n  b=b"lens-local-qualified";self.send_response(200);self.send_header("Content-Length",str(len(b)));self.end_headers();self.wfile.write(b)\nHTTPServer(("0.0.0.0",8080),H).serve_forever()\n'
        fixture_unit = '[Unit]\nAfter=network.target\n[Service]\nExecStart=/usr/bin/python3 -I /opt/lens-qualification/fixture_endpoint.py\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n'
        secret = Path("/etc/lens-infra-qualification-secret")
        secret.write_bytes(b"qualification-sentinel-not-a-customer-secret")
        secret.chmod(0o600)
        p = {"schema_version": 1, "revision": 1, "host_id": "qualification-host",
             "capacity": {"vcpus": 6, "memory_mib": 4096, "disk_gib": 64, "host_reserve_mib": 1024},
             "classes": {"small-v1": {"vcpus": 1, "memory_mib": 1024, "disk_gib": 8}},
             "networks": {"app": {"environment": "qualification", "libvirt_network": "li-qualification", "cidr": "10.253.242.0/24", "mode": "isolated", "managed": True}},
             "environments": {"qualification": {"networks": ["app"], "classes": ["small-v1"], "templates": ["http-v1"], "max_instances": 4, "vcpus": 6, "memory_mib": 4096, "disk_gib": 64}},
             "templates": {"http-v1": {"kind": "appliance", "image": str(args.image), "image_sha256": file_digest(args.image), "profile": "fast-web", "port": 8080, "listen_port": 18081,
                                       "health_path": "/health", "startup_seconds": 180, "drain_seconds": 1, "secrets": ["fixture"]}},
             "secrets": {"fixture": {"path": str(secret), "encrypted": False, "generation": 1, "environments": ["qualification"]}}}
        key = Path('/var/lib/lens-infra/qualification_ed25519')
        command('/usr/bin/ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(key))
        public_key = Path(str(key)+'.pub').read_text().strip()
        p['build_profiles'] = {'agents-v1': {'shell': 'dataplicity-server-v1', 'users': [
            {'name': 'vmadmin', 'ssh_keys': [public_key], 'sudo': True},
            {'name': 'observer', 'ssh_keys': [public_key], 'sudo': False}]}}
        p['templates']['http-v1']['build_profile'] = 'agents-v1'
        p['classes']['larger-v1'] = {'vcpus': 2, 'memory_mib': 1536, 'disk_gib': 12}
        p['environments']['qualification']['classes'].append('larger-v1')
        p['templates']['server-v1'] = p['templates']['http-v1'] | {'profile': 'private'}
        p['environments']['qualification']['templates'].append('server-v1')
        if args.lens_package:
            logging = args.scratch / "approved-logging.conf"
            logging.write_text('if $programname == "lens-build-qualification" then action(type="omfile" file="/var/log/lens-build-profile.log")\n')
            p["build_profiles"]["agents-v1"].update({"lens": {"path": str(args.lens_package), "sha256": file_digest(args.lens_package), "version": "0.3.11"},
                                                        "logging": {"path": str(logging), "sha256": file_digest(logging)}})
            p["templates"]["http-v1"]["build_profile"] = "agents-v1"
        validate(p)
        provision(p, backend, apply=True)
        print("isolated environment network ready", flush=True)

        class ImageBootstrap(Libvirt):
            def virsh(self, *argv, **kwargs):
                if argv and argv[0] == "define":
                    import xml.etree.ElementTree as ET
                    definition = Path(argv[1])
                    xml = ET.fromstring(definition.read_bytes())
                    ET.SubElement(xml.find("devices/serial"), "log", {"file": str(args.scratch / "bootstrap-console.log"), "append": "on"})
                    definition.write_bytes(ET.tostring(xml))
                if argv and argv[0] == "start":
                    directory = self.storage / argv[1]
                    path = directory / "user-data"
                    user_data = json.loads(path.read_text().split("\n", 1)[1])
                    user_data["package_update"] = False
                    user_data["write_files"] += [
                        {"path": "/opt/lens-qualification/fixture_endpoint.py", "permissions": "0644", "content": fixture_http},
                        {"path": "/etc/systemd/system/lens-qualification-http.service", "permissions": "0644", "content": fixture_unit}]
                    for package in guest_packages:
                        user_data["write_files"].append({"path": "/opt/lens-qualification/" + package.name, "encoding": "b64", "permissions": "0600", "content": base64.b64encode(package.read_bytes()).decode()})
                    user_data["runcmd"] = [["dpkg", "-i", *["/opt/lens-qualification/" + package.name for package in guest_packages]], ["systemctl", "daemon-reload"],
                                            ["systemctl", "start", "qemu-guest-agent.service"], ["systemctl", "enable", "--now", "lens-qualification-http.service"], *user_data.get("runcmd", [])]
                    path.write_text("#cloud-config\n" + json.dumps(user_data))
                    run(["/usr/bin/cloud-localds", "--network-config", str(directory / "network-data"), str(directory / "seed.iso"), str(path), str(directory / "meta-data")])
                return super().virsh(*argv, **kwargs)

        bootstrap = ImageBootstrap("/var/lib/lens-infra")
        first = {"operation_id": "qualification-image", "configuration_revision": 1, "host_id": p["host_id"], "environment": "qualification", "service": "image-preparation", "class": "small-v1", "template": "http-v1", "network": "app", "instances": 1, "release": "bootstrap"}
        resource = allocations(p, first)[0]
        bootstrap.prepare(p["templates"]["http-v1"])
        bootstrap.create(resource, first, p)
        print("qualification bootstrap VM started", flush=True)
        bootstrap.wait_agent(resource, 180)
        print("native guest agent ready", flush=True)
        try:
            bootstrap.guest_exec(resource, ["/usr/bin/cloud-init", "status", "--wait"], timeout=180)
        except Exception:
            diagnostic = bootstrap.guest_exec(resource, ["/usr/bin/python3", "-I", "-c", 'import subprocess,json; from pathlib import Path; p=subprocess.run(["/usr/bin/cloud-init","status","--long","--format=json"],capture_output=True); print(json.dumps({"code":p.returncode,"stdout":p.stdout.decode(),"stderr":p.stderr.decode()})); print(Path("/var/log/cloud-init.log").read_text()[-32000:])'])
            (args.scratch / "cloud-init-diagnostic.txt").write_bytes(diagnostic)
            raise
        bootstrap.guest_exec(resource, ["/usr/sbin/userdel", "--remove", "vmadmin"])
        bootstrap.guest_exec(resource, ["/usr/sbin/userdel", "--remove", "observer"])
        bootstrap.guest_exec(resource, ["/usr/sbin/groupdel", "lensu-vmadmin"])
        bootstrap.guest_exec(resource, ["/usr/sbin/groupdel", "lensu-observer"])
        bootstrap.guest_exec(resource, ["/usr/bin/cloud-init", "clean", "--logs", "--machine-id", "--seed"])
        bootstrap.remove(resource)
        golden = args.scratch / "approved-base.qcow2"
        run(["/usr/bin/qemu-img", "convert", "-O", "qcow2", str(bootstrap.storage / resource["id"] / "root.qcow2"), str(golden)], timeout=180)
        print("offline approved image prepared", flush=True)
        p["templates"]["http-v1"]["image"] = str(golden)
        p["templates"]["http-v1"]["image_sha256"] = file_digest(golden)
        p["templates"]["server-v1"]["image"] = str(golden)
        p["templates"]["server-v1"]["image_sha256"] = file_digest(golden)
        p['images'] = {'qualified-linux-v1': {'path': str(golden), 'sha256': file_digest(golden),
            'format': 'qcow2', 'os_family': 'debian',
            'capabilities': ['cloud-init', 'qemu-guest-agent', 'python3', 'nftables', 'systemd', 'bash']}}
        p['environments']['qualification']['images'] = ['qualified-linux-v1']
        config_file = args.scratch / 'first-boot.conf'
        config_file.write_text('qualification-config-v1\n')
        run_once_file = args.scratch / 'first-boot.sh'
        run_once_file.write_text('set -eu\ntest "$(cat /etc/lens-qualification.conf)" = qualification-config-v1\nprintf "initialised\\n" >> /var/lib/lens-first-boot-count\n')
        p['build_profiles']['agents-v1'].update(
            files=[{'name': 'first-boot-config-v1', 'path': str(config_file), 'sha256': file_digest(config_file),
                    'destination': '/etc/lens-qualification.conf', 'owner': 'root', 'group': 'root', 'mode': '0640'}],
            run_once=[{'name': 'first-boot-v1', 'path': str(run_once_file), 'sha256': file_digest(run_once_file),
                       'interpreter': '/bin/sh', 'timeout_seconds': 30}])
        store = Store("/var/lib/lens-infra")
        store.apply_policy(validate(p), 0)
        proxy = Ingress(store.root, binary=str(args.tools.parent / "haproxy"), supervised=False)
        engine = Engine(store, backend, proxy)
        req = first | {"operation_id": "qualification-deploy-one", "service": "http", "release": "release-one", "listen_port": 18081}
        engine.submit(req, 0)
        engine.execute(req["operation_id"])
        result = store.operation(req["operation_id"])
        if result["state"] != "succeeded":
            diagnostic = 'import subprocess,json; commands=[["cloud-init","status","--long","--format=json"],["tail","-n","100","/var/log/cloud-init-output.log"],["systemctl","show","lens-qualification-http.service","--property=ActiveState","--property=ExecMainStatus"],["ss","-ltnp"],["ip","-j","address"],["nft","list","ruleset"],["journalctl","-u","lens-qualification-http.service","-n","20","--no-pager"]]; print(json.dumps([{"command":a,"output":subprocess.run(a,capture_output=True).stdout.decode()} for a in commands]))'
            for instance in result["resources"]:
                try:
                    data = backend.guest_exec(instance, ["/usr/bin/python3", "-c", diagnostic])
                    (args.scratch / "readiness-diagnostic.json").write_bytes(data)
                    address = instance["detail"].get("address", "10.253.242.2")
                    curl = subprocess.run(["/usr/bin/curl", "--noproxy", "*", "--max-time", "3", "-sv", f"http://{address}:8080/health"], capture_output=True)
                    (args.scratch / "http-diagnostic.txt").write_bytes(curl.stdout + curl.stderr)
                except Exception:
                    pass
            raise RuntimeError("first deployment failed: " + str(result["error"]))
        import urllib.request
        with urllib.request.urlopen("http://127.0.0.1:18081/health", timeout=5) as response:
            assert response.read() == b"lens-local-qualified"
        print("real local VM, secret delivery and ingress qualified", flush=True)
        if args.lens_package:
            instance = result["resources"][0]
            version = backend.guest_exec(instance, ["/usr/bin/lens", "--version"]).decode().strip()
            assert "0.3.11" in version
            backend.guest_exec(instance, ["/usr/bin/logger", "-t", "lens-build-qualification", "approved-profile-delivered"])
            for _ in range(10):
                log_data = backend.guest_exec(instance, ["/usr/bin/python3", "-I", "-c", 'import pathlib; p=pathlib.Path("/var/log/lens-build-profile.log"); print(p.read_text() if p.exists() else "")'])
                if b"approved-profile-delivered" in log_data:
                    break
                time.sleep(1)
            assert b"approved-profile-delivered" in log_data
            print("versioned Lens and logging build profile qualified", flush=True)
        req2 = req | {"operation_id": "qualification-deploy-two", "release": "release-two"}
        engine.submit(req2, 0)
        engine.execute(req2["operation_id"])
        result2 = store.operation(req2["operation_id"])
        if result2["state"] != "succeeded":
            raise RuntimeError("replacement deployment failed: " + str(result2["error"]))
        assert store.operation(req["operation_id"])["resources"][0]["state"] == "removed"
        from lens_infra import vm
        server_request = vm.create_request({'operation_id': 'qualification-server', 'configuration_revision': 1, 'host_id': p['host_id'],
                                           'environment': 'qualification', 'name': 'persistent-server', 'class': 'small-v1', 'template': 'server-v1', 'network': 'app', 'release': 'server-one', 'image': 'qualified-linux-v1'})
        engine.submit(server_request, 0)
        engine.execute(server_request['operation_id'])
        server = store.operation(server_request['operation_id'])
        assert server['state'] == 'succeeded', server['error']
        instance = server['resources'][0]
        address = instance['detail']['address']
        ssh = ['/usr/bin/ssh', '-i', str(key), '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new', '-o', 'UserKnownHostsFile=/var/lib/lens-infra/qualification-known-hosts', '-l', 'vmadmin', address]
        assert command(*ssh, 'sudo -n id -u').strip() == b'0'
        import shlex
        home_check = 'import os,pathlib; p=pathlib.Path.home(); assert p==pathlib.Path("/home/vmadmin"); assert p.stat().st_uid==os.getuid(); assert p.stat().st_dev==pathlib.Path("/").stat().st_dev; (p/"home-marker").write_text("preserved-private-home")'
        command(*ssh, shlex.join(['/usr/bin/python3', '-I', '-c', home_check]))
        observer_ssh = ssh[:-2] + ['observer', address]
        assert command(*observer_ssh, '/usr/bin/id -un').strip() == b'observer'
        denied = subprocess.run([*observer_ssh, 'sudo -n id -u'], capture_output=True, timeout=10)
        assert denied.returncode != 0, 'ordinary named user obtained administrator privileges'
        denied = subprocess.run([*observer_ssh, '/usr/bin/test -r /home/vmadmin/home-marker'], capture_output=True, timeout=10)
        assert denied.returncode != 0, 'ordinary user could read another user home'
        config = backend.guest_exec(instance, ['/usr/sbin/sshd', '-T']).decode()
        assert 'permitrootlogin no' in config and 'passwordauthentication no' in config
        shell = subprocess.run([*ssh, '-tt', 'env NO_COLOR=1 bash --login -i'], input=b'false\nprintf "COMMAND_STATUS=%s\\n" "$?"\nexit\n', capture_output=True, timeout=15)
        assert shell.returncode == 0, 'interactive SSH shell failed'
        assert b'[QUALIFICATION]' in shell.stdout and b'@persistent-server:' in shell.stdout and b'exit 1' in shell.stdout and b'COMMAND_STATUS=1' in shell.stdout, shell.stdout[-3000:]
        root_shell = subprocess.run([*ssh, '-tt', 'sudo -n env NO_COLOR=1 bash --login -i'], input=b'true\nexit\n', capture_output=True, timeout=15)
        assert b'ROOT]' in root_shell.stdout, 'elevated shell was not unmistakable'
        status = json.loads(command(*ssh, 'dp-status --json'))
        assert status['environment'] == 'qualification' and status['class'] == 'small-v1'
        def bootstrap_status(target):
            code = 'import json;from pathlib import Path;f=Path("/etc/lens-qualification.conf");s=Path("/var/lib/lens/bootstrap/steps/first-boot-v1.json");print(json.dumps({"config":f.read_text(),"mode":f.stat().st_mode & 511,"count":Path("/var/lib/lens-first-boot-count").read_text(),"step":json.loads(s.read_text())["state"]}))'
            return json.loads(backend.guest_exec(target, ['/usr/bin/python3', '-I', '-c', code]))
        expected_bootstrap = {'config': 'qualification-config-v1\n', 'mode': 0o640, 'count': 'initialised\n', 'step': 'completed'}
        assert bootstrap_status(instance) == expected_bootstrap
        # Delivery repeats during controller recovery/boot. Completed setup must not.
        engine.deliver(instance, server_request, p)
        assert bootstrap_status(instance) == expected_bootstrap
        print('explicit local image, pinned file injection and non-repeating bootstrap qualified', flush=True)
        print('real interactive SSH prompt, root distinction and local status qualified', flush=True)
        if args.build_only:
            evidence = {'qualified': True, 'runtime': 'real-kvm-libvirt', 'scope': 'private-mount-and-network-namespaces',
                        'checks': ['explicit-local-image-selection', 'pinned-file-injection', 'file-owner-and-mode',
                                   'run-once-before-readiness', 'run-once-not-repeated-by-recovery', 'private-server-without-HTTP-readiness',
                                   'named-user-SSH-and-sudo', 'Lens-and-logging-build-profile' if args.lens_package else 'local-build-profile'],
                        'excluded': ['disk-resize', 'backup-and-restore', 'host-reboot']}
            (args.scratch / 'evidence.json').write_text(json.dumps(evidence, indent=2))
            print(json.dumps(evidence), flush=True)
            return
        backend.guest_exec(instance, ['/usr/bin/python3', '-I', '-c', 'from pathlib import Path;Path("/var/lib/lens-backup-marker").write_text("preserved-customer-data")'])
        def action(kind, suffix, **extra):
            selector = {'operation_id': 'qualification-'+suffix, 'configuration_revision': 1, 'host_id': p['host_id'], 'instance_id': instance['id'], 'action': kind, **extra}
            vm.submit(engine, selector, 0)
            engine.execute(selector['operation_id'])
            result = store.operation(selector['operation_id'])
            assert result['state'] == 'succeeded', result['error']
        action('shutdown', 'server-stop')
        action('resize', 'server-resize', new_class='larger-v1')
        action('backup', 'server-backup')
        restore = vm.restore_request(engine, {'operation_id': 'qualification-restore', 'configuration_revision': 1, 'host_id': p['host_id'],
                                              'backup_id': 'qualification-server-backup', 'name': 'restored-server', 'class': 'larger-v1', 'network': 'app'}, 0)
        engine.submit(restore, 0)
        engine.execute(restore['operation_id'])
        restored = store.operation(restore['operation_id'])
        assert restored['state'] == 'succeeded', restored['error']
        instance2 = restored['resources'][0]
        assert instance2['id'] != instance['id'] and instance2['state'] == 'stopped'
        selector = {'operation_id': 'qualification-restore-boot', 'configuration_revision': 1, 'host_id': p['host_id'], 'instance_id': instance2['id'], 'action': 'boot'}
        transfers_before_restore = backend.artifact_transfers
        vm.submit(engine, selector, 0)
        engine.execute(selector['operation_id'])
        boot_result = store.operation(selector['operation_id'])
        assert boot_result['state'] == 'succeeded', boot_result['error']
        assert backend.artifact_transfers == transfers_before_restore, 'verified restored artifact cache was retransmitted'
        marker = backend.guest_exec(instance2, ['/usr/bin/python3', '-I', '-c', 'from pathlib import Path;print(Path("/var/lib/lens-backup-marker").read_text())'])
        assert b'preserved-customer-data' in marker
        home_data = backend.guest_exec(instance2, ['/usr/bin/python3', '-I', '-c', 'from pathlib import Path;print(Path("/home/vmadmin/home-marker").read_text())'])
        assert b'preserved-private-home' in home_data
        assert bootstrap_status(instance2) == expected_bootstrap, 'restored bootstrap repeated or injected file changed'
        import xml.etree.ElementTree as ET
        definition = ET.fromstring(backend.virsh('dumpxml', instance2['id']))
        assert definition.find('memory').text == str(1536 * 1024)
        assert definition.find('vcpu').text == '2'
        assert store.resource(instance2['id'])['disk_gib'] == 12
        assert store.resource(instance2['id'])['detail']['guest_root_bytes'] >= 10 * 2**30
        print('real administrator SSH/sudo, offline resize, backup and restore qualified', flush=True)
        assert b"qualification-sentinel-not-a-customer-secret" not in Path(store.path).read_bytes()
        evidence = {"qualified": True, "runtime": "real-kvm-libvirt", "scope": "private-mount-and-network-namespaces", "checks": ["offline-image-preparation", "approved-class", "real-guest-agent", "local-credential-delivery", "readiness", "real-ingress", "replacement", "draining", "persistent-operation-history", "plaintext-absent-from-journal"]}
        evidence['checks'] += ['administrator-SSH-with-sudo', 'ordinary-named-user-without-sudo', 'private-local-home-preserved-by-backup', 'root-and-password-SSH-disabled', 'offline-class-and-disk-growth', 'offline-disk-backup', 'restore-to-new-stopped-VM', 'restored-data-survives-boot']
        evidence['checks'] += ['interactive-SSH-server-prompt', 'failed-command-status-preserved', 'distinct-root-prompt', 'noninteractive-local-status-JSON']
        evidence['checks'] += ['actual-resized-vCPU-and-memory', 'guest-filesystem-grew', 'verified-guest-artifact-cache-reused']
        if args.lens_package:
            evidence["checks"] += ["pinned-Lens-build-package", "versioned-logging-config", "actual-guest-log-delivery"]
        (args.scratch / "evidence.json").write_text(json.dumps(evidence, indent=2))
        print(json.dumps(evidence), flush=True)
    finally:
        diagnostic = args.scratch / "qemu-logs"
        if Path("/var/log/libvirt/qemu").exists():
            shutil.copytree("/var/log/libvirt/qemu", diagnostic, dirs_exist_ok=True)
        # Only this private libvirt instance is visible; still require our metadata before cleanup.
        try:
            for item in backend.inventory():
                if item.get("operation_id", "").startswith("qualification-"):
                    backend.virsh("destroy", item["id"], check=False)
            backend.virsh("net-destroy", "li-qualification", check=False)
        except Exception:
            pass
        daemon.terminate()
        daemon.wait(timeout=10)
        log.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tools", required=True, type=Path)
    parser.add_argument("--image", required=True, type=Path)
    parser.add_argument("--scratch", type=Path)
    parser.add_argument("--lens-package", type=Path)
    parser.add_argument("--build-only", action="store_true", help="Qualify image/bootstrap without disk lifecycle operations")
    parser.add_argument("--inside", action="store_true")
    args = parser.parse_args()
    if sys.platform != "linux" or os.geteuid() != 0:
        raise SystemExit("requires root inside disposable namespaces")
    for namespace in ("net", "mnt"):
        if Path(f"/proc/self/ns/{namespace}").stat().st_ino == Path(f"/proc/1/ns/{namespace}").stat().st_ino:
            raise SystemExit("REFUSED: qualification must not run in a host namespace")
    if not args.inside:
        available = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:")))
        if available < 8 * 1024 * 1024:
            raise SystemExit("qualification requires at least 8 GiB available memory; no workloads were changed")
        scratch = Path(tempfile.mkdtemp(prefix="vm-qualification-", dir=args.tools))
        scratch.chmod(0o711)
        print("qualification artifacts: " + str(scratch), flush=True)
        mounted_root(args, scratch)
    else:
        if not Path("/run/lens-infra-qualification").is_file():
            raise SystemExit("REFUSED: isolated qualification root was not prepared")
        qualify(args)


if __name__ == "__main__":
    main()
