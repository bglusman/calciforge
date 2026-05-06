#!/usr/bin/env python3
"""Smoke test Calciforge against a Helicone-shaped external gateway.

This is intentionally process-boundary coverage: it starts a tiny local HTTP
server that behaves like the Helicone AI Gateway `/v1/chat/completions` surface,
then starts Calciforge with `backend_type = "helicone"` and proves requests flow
through the gateway adapter rather than only unit-testing the router in-process.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def calciforge_command(config_path: Path) -> list[str]:
    configured_bin = os.environ.get("CALCIFORGE_BIN")
    if configured_bin:
        return [configured_bin, "--config", str(config_path), "--proxy-only"]

    repo_bin = Path.cwd() / "target" / "debug" / "calciforge"
    if repo_bin.exists():
        return [str(repo_bin), "--config", str(config_path), "--proxy-only"]

    return [
        "cargo",
        "run",
        "-p",
        "calciforge",
        "--",
        "--config",
        str(config_path),
        "--proxy-only",
    ]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def http_json(
    method: str,
    url: str,
    payload: dict | None = None,
    headers: dict[str, str] | None = None,
    timeout: float = 10.0,
) -> tuple[int, dict]:
    body = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json", **(headers or {})},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            data = response.read()
            return response.status, json.loads(data.decode("utf-8")) if data else {}
    except urllib.error.HTTPError as exc:
        data = exc.read()
        return exc.code, json.loads(data.decode("utf-8")) if data else {}


def http_no_redirect(method: str, url: str, timeout: float = 10.0) -> tuple[int, str]:
    request = urllib.request.Request(url, method=method)
    opener = urllib.request.build_opener(NoRedirect)
    try:
        with opener.open(request, timeout=timeout) as response:
            return response.status, response.headers.get("Location", "")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers.get("Location", "")


class HeliconeMockHandler(BaseHTTPRequestHandler):
    seen: "queue.Queue[dict]" = queue.Queue()

    def log_message(self, format: str, *args) -> None:
        return

    def do_GET(self) -> None:
        if self.path == "/dashboard":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"mock helicone dashboard")
            return
        self.send_response(404)
        self.end_headers()

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length)
        body = json.loads(raw.decode("utf-8")) if raw else {}
        self.seen.put(
            {
                "path": self.path,
                "authorization": self.headers.get("authorization"),
                "helicone_auth": self.headers.get("helicone-auth"),
                "body": body,
            }
        )

        if self.path != "/v1/chat/completions":
            self.send_response(404)
            self.end_headers()
            self.wfile.write(b"wrong path")
            return

        response = {
            "id": "chatcmpl-helicone-smoke",
            "object": "chat.completion",
            "created": 1,
            "model": body.get("model", "missing-model"),
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "helicone-smoke-ok",
                    },
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2,
            },
        }
        data = json.dumps(response).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def write_config(tmp: Path, gateway_port: int, upstream_port: int) -> Path:
    config = f"""
[calciforge]
version = 2

[proxy]
enabled = true
bind = "127.0.0.1:{gateway_port}"
api_key = "client-test-key"
backend_type = "helicone"
backend_url = "http://127.0.0.1:{upstream_port}/v1"
backend_api_key = "helicone-test-key"
gateway_ui_url = "http://127.0.0.1:{upstream_port}/dashboard"
timeout_seconds = 10
"""
    path = tmp / "config.toml"
    path.write_text(config, encoding="utf-8")
    return path


def wait_for_health(base_url: str, deadline: float) -> None:
    while time.monotonic() < deadline:
        try:
            status, _ = http_json("GET", f"{base_url}/health", timeout=1)
            if status == 200:
                return
        except Exception:
            pass
        time.sleep(0.2)
    raise RuntimeError("Calciforge gateway did not become healthy")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--startup-timeout", type=float, default=120.0)
    args = parser.parse_args()

    gateway_port = find_free_port()
    upstream_port = find_free_port()
    base_url = f"http://127.0.0.1:{gateway_port}"

    upstream = ThreadingHTTPServer(("127.0.0.1", upstream_port), HeliconeMockHandler)
    upstream_thread = threading.Thread(target=upstream.serve_forever, daemon=True)
    upstream_thread.start()

    with tempfile.TemporaryDirectory(prefix="calciforge-helicone-smoke-") as raw_tmp:
        tmp = Path(raw_tmp)
        config_path = write_config(tmp, gateway_port, upstream_port)
        proc = subprocess.Popen(
            calciforge_command(config_path),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
            preexec_fn=os.setsid if hasattr(os, "setsid") else None,
        )
        try:
            wait_for_health(base_url, time.monotonic() + args.startup_timeout)

            status, info = http_json("GET", f"{base_url}/gateway")
            if status != 200 or info.get("id") != "helicone":
                raise AssertionError(f"unexpected /gateway response {status}: {info}")

            status, location = http_no_redirect("GET", f"{base_url}/gateway/ui")
            expected_location = f"http://127.0.0.1:{upstream_port}/dashboard"
            if status not in (302, 303, 307, 308) or location != expected_location:
                raise AssertionError(
                    f"unexpected /gateway/ui redirect {status} to {location!r}"
                )

            status, completion = http_json(
                "POST",
                f"{base_url}/v1/chat/completions",
                {
                    "model": "openai/gpt-5.5",
                    "messages": [{"role": "user", "content": "reply exactly ok"}],
                },
                headers={"Authorization": "Bearer client-test-key"},
            )
            if status != 200:
                raise AssertionError(f"chat completion failed {status}: {completion}")
            content = completion["choices"][0]["message"]["content"]
            if content != "helicone-smoke-ok":
                raise AssertionError(f"unexpected completion content: {content!r}")

            seen = HeliconeMockHandler.seen.get(timeout=5)
            if seen["path"] != "/v1/chat/completions":
                raise AssertionError(f"wrong upstream path: {seen}")
            if seen["authorization"] != "Bearer helicone-test-key":
                raise AssertionError("Calciforge did not forward Helicone Authorization")
            if seen["helicone_auth"] != "Bearer helicone-test-key":
                raise AssertionError("Calciforge did not forward Helicone-Auth")
            if seen["body"].get("model") != "openai/gpt-5.5":
                raise AssertionError(f"wrong upstream model: {seen}")

            print("Helicone gateway smoke passed")
            return 0
        finally:
            upstream.shutdown()
            if proc.poll() is None:
                if hasattr(os, "killpg"):
                    os.killpg(proc.pid, signal.SIGTERM)
                else:
                    proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"Helicone gateway smoke failed: {exc}", file=sys.stderr)
        raise
