"""
拡張機能と llama-server のあいだに立つ薄いブリッジ。

拡張機能は 127.0.0.1:8788 だけを知る。ブリッジは
- ループバック以外からのリクエストを拒む
- CORS は拡張機能のオリジンだけに返す
- 上流 (llama-server) もループバックだけを許す
- プロンプトを PFML3 に組み立て直す
- 壊れた入力でハンドラを落とさない

llama-server を自分では起動しない。起動は start-bridge.ps1 が行う。
"""

from __future__ import annotations

import ipaddress
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(__file__))

from lib_pfml3 import STOP_TOKENS, build_prompt, clean_translation  # noqa: E402

HOST = os.environ.get("PLAMO_BRIDGE_HOST", "127.0.0.1")
PORT = int(os.environ.get("PLAMO_BRIDGE_PORT", "8788"))
UPSTREAM = os.environ.get("PLAMO_UPSTREAM", "http://127.0.0.1:8090").rstrip("/")
MODEL = os.environ.get("PLAMO_MODEL_NAME", "plamo-3-translate")
MAX_BODY = 256 * 1024


class BadRequest(Exception):
    """入力が読めないときに 400 を返すための例外。"""


def is_loopback(host: str) -> bool:
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def upstream_is_loopback() -> bool:
    parsed = urllib.parse.urlparse(UPSTREAM)
    return parsed.scheme in ("http", "https") and is_loopback(parsed.hostname or "")


def upstream_ready() -> tuple[bool, str]:
    try:
        with urllib.request.urlopen(f"{UPSTREAM}/health", timeout=3) as response:
            body = response.read().decode("utf-8", "replace")
        return True, body
    except Exception as exc:  # 接続拒否もタイムアウトも、拡張機能には同じ「未準備」で返す
        return False, str(exc)


def _as_dict(value) -> dict:
    return value if isinstance(value, dict) else {}


def _as_list(value) -> list:
    return value if isinstance(value, list) else []


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "plamo-bridge"

    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _origin_allowed(self) -> bool:
        origin = self.headers.get("Origin", "")
        return origin.startswith("chrome-extension://") or origin.startswith("extension://")

    def _reject_remote(self) -> bool:
        host = self.client_address[0]
        if is_loopback(host):
            return False
        self._send(403, {"error": "このブリッジはループバックからのみ受けます。"})
        return True

    def _send(self, status: int, payload: dict) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        if self._origin_allowed():
            self.send_header("Access-Control-Allow-Origin", self.headers.get("Origin", ""))
            self.send_header("Vary", "Origin")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self) -> None:  # noqa: N802
        self.send_response(204 if self._origin_allowed() else 403)
        if self._origin_allowed():
            self.send_header("Access-Control-Allow-Origin", self.headers.get("Origin", ""))
            self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Content-Type")
            self.send_header("Vary", "Origin")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self) -> None:  # noqa: N802
        if self._reject_remote():
            return
        if self.path.split("?", 1)[0] != "/health":
            self._send(404, {"error": "not found"})
            return
        if not upstream_is_loopback():
            self._send(503, {"ready": False, "upstream": UPSTREAM, "detail": "上流がループバックではありません。"})
            return
        ready, detail = upstream_ready()
        self._send(200, {"ready": ready, "upstream": UPSTREAM, "detail": detail[:300]})

    def do_POST(self) -> None:  # noqa: N802
        if self._reject_remote():
            return
        if self.path.split("?", 1)[0] != "/v1/completions":
            self._send(404, {"error": "not found"})
            return
        try:
            incoming = self._read_json()
            request = self._build_request(incoming)
        except BadRequest as exc:
            self._send(400, {"error": str(exc)})
            return
        except (ValueError, TypeError, OverflowError) as exc:
            self._send(400, {"error": str(exc)})
            return

        raw = json.dumps(request).encode("utf-8")
        upstream = urllib.request.Request(
            f"{UPSTREAM}/v1/completions",
            data=raw,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(upstream, timeout=180) as response:
                payload = json.loads(response.read().decode("utf-8", "replace"))
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", "replace")[:300]
            self._send(502, {"error": f"llama-server が {exc.code} を返しました。", "detail": detail})
            return
        except Exception as exc:
            self._send(502, {"error": f"llama-server に届きません。{exc}"})
            return

        choices = _as_list(_as_dict(payload).get("choices"))
        if choices and isinstance(choices[0], dict):
            choices[0]["text"] = clean_translation(choices[0].get("text", ""))
        self._send(200, payload if isinstance(payload, dict) else {"error": "上流の応答が JSON オブジェクトではありません。"})

    def _read_json(self) -> dict:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except (TypeError, ValueError) as exc:
            raise BadRequest("Content-Length が数値ではありません。") from exc
        if length < 0 or length > MAX_BODY:
            raise BadRequest("本文が大きすぎるか、長さが不正です。")
        raw = self.rfile.read(length) if length else b"{}"
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise BadRequest("本文が UTF-8 として読めません。") from exc
        try:
            parsed = json.loads(text or "{}")
        except json.JSONDecodeError as exc:
            raise BadRequest("JSON として読めません。") from exc
        if not isinstance(parsed, dict):
            raise BadRequest("JSON オブジェクトを送ってください。")
        return parsed

    def _build_request(self, incoming: dict) -> dict:
        prompt = incoming.get("prompt")
        if not isinstance(prompt, str) or not prompt.strip():
            prompt = build_prompt(
                text=incoming.get("text", ""),
                target_lang=incoming.get("target_lang") or incoming.get("targetLang"),
                source_lang=incoming.get("source_lang") or incoming.get("sourceLang") or None,
                context=incoming.get("context"),
                glossary=incoming.get("glossary"),
                form=incoming.get("form") or None,
                history=incoming.get("history"),
            )

        max_tokens = incoming.get("max_tokens", 512)
        try:
            max_tokens = int(max_tokens)
        except (TypeError, ValueError) as exc:
            raise BadRequest("max_tokens が数値ではありません。") from exc
        max_tokens = max(1, min(4096, max_tokens))

        stop = incoming.get("stop")
        if not isinstance(stop, list) or not stop:
            stop = list(STOP_TOKENS)

        return {
            "model": MODEL,
            "prompt": prompt,
            "temperature": 0,
            "max_tokens": max_tokens,
            "stop": [item for item in stop if isinstance(item, str) and item],
        }


def main() -> None:
    if not is_loopback(HOST):
        sys.exit("PLAMO_BRIDGE_HOST は 127.0.0.1 だけを許可しています。")
    if not upstream_is_loopback():
        sys.exit("PLAMO_UPSTREAM は 127.0.0.1 のようなループバックだけを許可しています。")
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"bridge listening on http://{HOST}:{PORT} -> {UPSTREAM}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
