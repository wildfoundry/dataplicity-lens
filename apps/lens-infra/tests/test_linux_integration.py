"""Real local IPC and HAProxy tests. All resources live under a temporary directory."""

import base64
import hashlib
import http.server
import json
import multiprocessing
import os
from pathlib import Path
import signal
import socket
import sys
import tempfile
import threading
import time
import unittest
import urllib.request

from lens_infra.daemon import Server, request as ipc
from lens_infra.engine import Engine
from lens_infra.ingress import Ingress
from lens_infra.store import Store
from test_infrastructure import Backend, Proxy, policy, request


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.headers.get("Upgrade", "").lower() == "websocket":
            accept = base64.b64encode(hashlib.sha1((self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            self.connection.settimeout(5)
            try:
                while True:
                    head = self.rfile.read(2)
                    if len(head) != 2:
                        break
                    length = head[1] & 127
                    mask = self.rfile.read(4)
                    payload = self.rfile.read(length)
                    decoded = bytes(v ^ mask[i % 4] for i, v in enumerate(payload))
                    if head[0] & 15 == 9:
                        self.wfile.write(bytes([0x8a, len(decoded)]) + decoded)
                        self.wfile.flush()
                    elif head[0] & 15 == 8:
                        break
            except OSError:
                pass
            self.close_connection = True
            return
        data = self.server.identity.encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.calls += 1
        result = {"ready": True} if data.get("method") == "lens.health" else self.server.identity
        body = json.dumps({"jsonrpc": "2.0", "id": data.get("id"), "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class ProtocolReadinessTests(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.server.identity = "healthy"
        self.server.calls = 0
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()

    def test_websocket_gate_performs_upgrade_and_ping_pong(self):
        self.assertTrue(Engine.protocol_health("127.0.0.1", {"profile": "websocket", "port": self.port}))

    def test_jsonrpc_gate_requires_a_ready_result_without_retries(self):
        self.assertTrue(Engine.protocol_health("127.0.0.1", {"profile": "jsonrpc", "port": self.port}))
        self.assertFalse(Engine.protocol_health("127.0.0.1", {"profile": "jsonrpc", "port": self.port, "rpc_health_method": "wrong.health"}))
        self.assertEqual(self.server.calls, 2)


@unittest.skipUnless(sys.platform == "linux" and os.environ.get("HAPROXY_BIN"), "requires an isolated real HAProxy binary on Linux")
class RealProxyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="li-", dir="/tmp")
        self.root = Path(self.temp.name)
        self.proxy = Ingress(self.root, binary=os.environ["HAPROXY_BIN"], supervised=False)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.server.identity = "release-one"
        self.server.calls = 0
        self.backend_port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.front_port = free_port()
        self.req = request(listen_port=self.front_port)
        self.template = {"profile": "fast-web", "port": self.backend_port, "health_path": "/health", "drain_seconds": 10}

    def tearDown(self):
        for pidfile in self.root.glob("ingress/*/haproxy.pid"):
            try:
                pid = int(pidfile.read_text())
                if Path(f"/proc/{pid}/comm").read_text().strip() == "haproxy":
                    os.kill(pid, signal.SIGTERM)
            except (OSError, ValueError):
                pass
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()

    def publish(self, profile="fast-web", operation="deploy-one"):
        return self.proxy.publish(self.req | {"operation_id": operation}, self.template | {"profile": profile}, [{"address": "127.0.0.1"}])

    def test_real_http_endpoint_and_persistent_generation(self):
        self.publish()
        with urllib.request.urlopen(f"http://127.0.0.1:{self.front_port}/", timeout=3) as response:
            self.assertEqual(response.read(), b"release-one")
        generation = next(self.root.glob("ingress/*/generation.json"))
        self.assertEqual(json.loads(generation.read_text())["request"]["operation_id"], "deploy-one")

    def test_real_jsonrpc_request_is_not_replayed(self):
        self.publish("jsonrpc")
        body = json.dumps({"jsonrpc": "2.0", "id": 7, "method": "health"}).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{self.front_port}/", data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=3) as response:
            self.assertEqual(json.load(response)["id"], 7)
        self.assertEqual(self.server.calls, 1)

    def test_websocket_survives_a_real_proxy_reload(self):
        self.publish("websocket")
        with socket.create_connection(("127.0.0.1", self.front_port), timeout=3) as ws:
            ws.sendall(b"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
            self.assertIn(b"101", ws.recv(4096))
            self.publish("websocket", "deploy-two")
            mask = b"abcd"
            payload = bytes(v ^ mask[i % 4] for i, v in enumerate(b"lens"))
            ws.sendall(b"\x89\x84" + mask + payload)
            self.assertEqual(ws.recv(6), b"\x8a\x04lens")
            self.assertGreaterEqual(self.proxy.connections("production/api", retired=True), 1)


def serve_test(directory, socket_path):
    store = Store(Path(directory) / "state")
    engine = Engine(store, Backend(), Proxy())
    Server(engine, socket_path, 65534).serve()


def unauthorised_client(path, connection):
    os.setgroups([])
    os.setgid(65534)
    os.setuid(65534)
    try:
        ipc(path, {"method": "config.apply", "payload": {}})
        connection.send("unexpectedly-authorised")
    except Exception as exc:
        connection.send(str(exc))


@unittest.skipUnless(sys.platform == "linux" and os.geteuid() == 0, "requires Linux root only inside the isolated integration test")
class KernelIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="li-ipc-", dir="/tmp")
        self.root = Path(self.temp.name)
        self.root.chmod(0o755)
        self.socket = self.root / "control.sock"
        self.policy = policy(self.root)
        self.process = multiprocessing.Process(target=serve_test, args=(self.root, self.socket))
        self.process.start()
        deadline = time.monotonic() + 5
        while not self.socket.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(self.socket.exists())
        ipc(self.socket, {"method": "config.apply", "payload": self.policy})

    def tearDown(self):
        self.process.terminate()
        self.process.join(5)
        self.temp.cleanup()

    def test_kernel_peer_uid_cannot_be_forged(self):
        parent, channel = multiprocessing.Pipe()
        process = multiprocessing.Process(target=unauthorised_client, args=(self.socket, channel))
        process.start()
        self.assertIn("local administrator", parent.recv())
        process.join(5)
        self.assertEqual(process.exitcode, 0)

    def test_socket_can_report_applied_configuration(self):
        result = ipc(self.socket, {"method": "status"})
        self.assertEqual(result["configuration_revision"], 1)


if __name__ == "__main__":
    unittest.main()
