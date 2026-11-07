"""ブリッジの HTTP 面を、偽の上流に対して確かめる。

本物の llama-server は要らない。ここで見たいのは:
- 壊れた入力でハンドラが落ちず、400 を返すこと
- ループバック以外と、拡張機能以外のオリジンに CORS を返さないこと
- 正常な入力では、組み立て直した PFML3 を上流へ渡すこと
"""

import importlib.util
import json
import os
import socket
import sys
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

# 偽の上流。受け取ったプロンプトをそのまま返す。
RECEIVED = []


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_GET(self):  # noqa: N802
        if self.path == "/health":
            self._send(200, {"status": "ok"})
        else:
            self._send(404, {})

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        RECEIVED.append(body)
        self._send(200, {"choices": [{"text": " 訳文 <|plamo:tag|>extra", "index": 0}]})

    def _send(self, status, payload):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def load_bridge(upstream_url):
    os.environ["PLAMO_UPSTREAM"] = upstream_url
    spec = importlib.util.spec_from_file_location("plamo_bridge_server", os.path.join(HERE, "server.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BridgeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        threading.Thread(target=cls.upstream.serve_forever, daemon=True).start()
        cls.upstream_url = f"http://127.0.0.1:{cls.upstream.server_address[1]}"

        cls.bridge_module = load_bridge(cls.upstream_url)
        cls.bridge = ThreadingHTTPServer(("127.0.0.1", 0), cls.bridge_module.Handler)
        threading.Thread(target=cls.bridge.serve_forever, daemon=True).start()
        cls.base = f"http://127.0.0.1:{cls.bridge.server_address[1]}"
        time.sleep(0.05)

    @classmethod
    def tearDownClass(cls):
        cls.bridge.shutdown()
        cls.upstream.shutdown()

    def post(self, body, headers=None):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        request = urllib.request.Request(
            f"{self.base}/v1/completions",
            data=data,
            headers={"Content-Type": "application/json", **(headers or {})},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status, json.loads(response.read()), response.headers
        except urllib.error.HTTPError as exc:
            return exc.code, json.loads(exc.read() or b"{}"), exc.headers

    def test_translates_and_strips_stop_token(self):
        RECEIVED.clear()
        status, payload, _ = self.post({"text": "会議は明日です。", "target_lang": "en"})
        self.assertEqual(status, 200)
        self.assertEqual(payload["choices"][0]["text"], "訳文")
        self.assertIn("<|plamo:msg|>translation", RECEIVED[0]["prompt"])
        self.assertEqual(RECEIVED[0]["temperature"], 0)

    def test_rejects_non_object_json(self):
        status, payload, _ = self.post(b"[]")
        self.assertEqual(status, 400)
        self.assertIn("error", payload)

    def test_rejects_invalid_utf8(self):
        status, _, _ = self.post(b"\xff\xfe\x00")
        self.assertEqual(status, 400)

    def test_rejects_bad_max_tokens(self):
        status, _, _ = self.post({"text": "hello", "target_lang": "en", "max_tokens": "abc"})
        self.assertEqual(status, 400)

    def test_rejects_unknown_language(self):
        status, _, _ = self.post({"text": "hello", "target_lang": "xx"})
        self.assertEqual(status, 400)

    def test_rejects_unhashable_language(self):
        status, _, _ = self.post({"text": "hello", "target_lang": ["en"]})
        self.assertEqual(status, 400)

    def test_rejects_infinite_max_tokens(self):
        status, _, _ = self.post({"text": "hello", "target_lang": "en", "max_tokens": 1e999})
        self.assertEqual(status, 400)

    def test_clamps_max_tokens(self):
        RECEIVED.clear()
        status, _, _ = self.post({"text": "hello", "target_lang": "en", "max_tokens": 999999})
        self.assertEqual(status, 200)
        self.assertEqual(RECEIVED[0]["max_tokens"], 4096)

    def test_ignores_malformed_history_and_glossary(self):
        status, _, _ = self.post(
            {
                "text": "hello",
                "target_lang": "en",
                "glossary": ["not-a-dict"],
                "history": ["nope", {"source": 1, "translation": 2}],
            }
        )
        self.assertEqual(status, 200)

    def test_health_reports_ready(self):
        with urllib.request.urlopen(f"{self.base}/health", timeout=5) as response:
            payload = json.loads(response.read())
        self.assertTrue(payload["ready"])

    def test_cors_only_for_extension_origins(self):
        request = urllib.request.Request(
            f"{self.base}/v1/completions",
            data=json.dumps({"text": "hello", "target_lang": "en"}).encode(),
            headers={"Content-Type": "application/json", "Origin": "chrome-extension://abcdef"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            self.assertEqual(response.headers.get("Access-Control-Allow-Origin"), "chrome-extension://abcdef")

    def test_rejects_foreign_origin_before_inference(self):
        RECEIVED.clear()
        request = urllib.request.Request(
            f"{self.base}/v1/completions",
            data=json.dumps({"text": "hello", "target_lang": "en"}).encode(),
            headers={"Content-Type": "text/plain", "Origin": "https://evil.example"},
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=5)
        self.assertEqual(caught.exception.code, 403)
        self.assertEqual(RECEIVED, [], "推論まで届かない")

    def test_rejects_deeply_nested_json(self):
        status, _, _ = self.post(b"[" * 5000)
        self.assertEqual(status, 400)

    def test_binds_ipv6_loopback(self):
        try:
            server = self.bridge_module.make_server("::1", 0)
        except OSError as exc:  # IPv6 が無効な環境
            self.skipTest(f"IPv6 unavailable: {exc}")
        try:
            self.assertEqual(server.address_family, socket.AF_INET6)
        finally:
            server.server_close()

    def test_requires_loopback_upstream(self):
        module = load_bridge("http://example.com:8090")
        self.assertFalse(module.upstream_is_loopback())


if __name__ == "__main__":
    unittest.main()
