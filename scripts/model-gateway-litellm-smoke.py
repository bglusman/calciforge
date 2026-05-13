#!/usr/bin/env python3
"""Smoke test Calciforge against a LiteLLM external gateway process.

This starts a deterministic mock OpenAI-compatible upstream, starts a LiteLLM
proxy that owns the upstream model/key mapping, then starts Calciforge with a
builtin HTTP transport pointed at LiteLLM and marked
`model_credential_owner = "provider"`. That distinction matters: the HTTP adapter is
only Calciforge's transport to the external gateway process, not a raw upstream
provider route.

The proof is intentionally process-boundary coverage: Calciforge sees
`managed/default`, LiteLLM sees `default`, and the mock upstream sees the model
selected by LiteLLM's config rather than Calciforge's public selector.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import queue
import shlex
import shutil
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


CALCIFORGE_CLIENT_KEY = "calciforge-client-test-key"
LITELLM_GATEWAY_KEY = "sk-litellm-smoke"
UPSTREAM_PROVIDER_KEY = "mock-provider-key"
LITELLM_MODEL = "default"
CALCIFORGE_MODEL = f"managed/{LITELLM_MODEL}"
UPSTREAM_MODEL = "calciforge-litellm-upstream"
EXPECTED_CONTENT = "litellm-smoke-ok"


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def command_from_env(name: str, fallback: list[str]) -> list[str]:
    raw = os.environ.get(name)
    return shlex.split(raw) if raw else fallback


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


def litellm_command(config_path: Path, port: int) -> list[str]:
    base = command_from_env("LITELLM_COMMAND", ["litellm"])
    executable = base[0]
    if shutil.which(executable) is None:
        raise RuntimeError(
            f"LiteLLM command not found: {executable!r}. Install it with "
            "`uv tool install 'litellm[proxy]'`, or set LITELLM_COMMAND."
        )
    return [
        *base,
        "--config",
        str(config_path),
        "--host",
        "127.0.0.1",
        "--port",
        str(port),
    ]


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
        try:
            decoded = json.loads(data.decode("utf-8")) if data else {}
        except json.JSONDecodeError:
            decoded = {"raw": data.decode("utf-8", errors="replace")}
        return exc.code, decoded


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def http_no_redirect(method: str, url: str, timeout: float = 10.0) -> tuple[int, str]:
    request = urllib.request.Request(url, method=method)
    opener = urllib.request.build_opener(NoRedirect)
    try:
        with opener.open(request, timeout=timeout) as response:
            return response.status, response.headers.get("Location", "")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers.get("Location", "")


class MockOpenAiHandler(BaseHTTPRequestHandler):
    seen: "queue.Queue[dict]" = queue.Queue()

    def log_message(self, format: str, *args) -> None:
        return

    def do_GET(self) -> None:
        if self.path == "/v1/models":
            response = {
                "object": "list",
                "data": [
                    {
                        "id": UPSTREAM_MODEL,
                        "object": "model",
                        "created": 1,
                        "owned_by": "litellm-smoke",
                    }
                ],
            }
            self.write_json(200, response)
            return
        self.write_json(404, {"error": "not found"})

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length)
        body = json.loads(raw.decode("utf-8")) if raw else {}
        self.seen.put(
            {
                "path": self.path,
                "authorization": self.headers.get("authorization"),
                "body": body,
            }
        )

        if self.path != "/v1/chat/completions":
            self.write_json(404, {"error": f"wrong path {self.path}"})
            return

        response = {
            "id": "chatcmpl-litellm-smoke",
            "object": "chat.completion",
            "created": 1,
            "model": body.get("model", "missing-model"),
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": EXPECTED_CONTENT,
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
        self.write_json(200, response)

    def write_json(self, status: int, payload: dict) -> None:
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def write_litellm_config(tmp: Path, upstream_port: int) -> Path:
    config = f"""
model_list:
  - model_name: {LITELLM_MODEL}
    litellm_params:
      model: openai/{UPSTREAM_MODEL}
      api_base: http://127.0.0.1:{upstream_port}/v1
      api_key: {UPSTREAM_PROVIDER_KEY}

litellm_settings:
  drop_params: true

router_settings:
  num_retries: 0

general_settings:
  master_key: {LITELLM_GATEWAY_KEY}
"""
    path = tmp / "litellm.yaml"
    path.write_text(config, encoding="utf-8")
    return path


def write_calciforge_config(tmp: Path, gateway_port: int, litellm_port: int) -> Path:
    config = f"""
[calciforge]
version = 2

[proxy]
enabled = true
bind = "127.0.0.1:{gateway_port}"
api_key = "{CALCIFORGE_CLIENT_KEY}"
backend_type = "mock"
gateway_ui_url = "http://127.0.0.1:{litellm_port}/ui"
timeout_seconds = 20

[[proxy.providers]]
id = "litellm-local"
backend_type = "litellm"
url = "http://127.0.0.1:{litellm_port}/v1"
model_credential_owner = "provider"
api_key = "{LITELLM_GATEWAY_KEY}"
models = ["managed/*"]
strip_model_prefix = "managed/"
timeout_seconds = 20
"""
    path = tmp / "calciforge.toml"
    path.write_text(config, encoding="utf-8")
    return path


def drain_output(proc: subprocess.Popen, tail: "collections.deque[str]") -> None:
    def reader() -> None:
        if proc.stdout is None:
            return
        for line in proc.stdout:
            tail.append(line.rstrip())

    threading.Thread(target=reader, daemon=True).start()


def format_log_tail(tail: "collections.deque[str]") -> str:
    return "\n".join(tail) if tail else "no output captured"


def wait_for_calciforge(
    base_url: str,
    deadline: float,
    proc: subprocess.Popen,
    log_tail: "collections.deque[str]",
) -> None:
    while time.monotonic() < deadline:
        exit_code = proc.poll()
        if exit_code is not None:
            raise RuntimeError(
                "Calciforge exited before becoming healthy "
                f"(code {exit_code}). Recent output:\n{format_log_tail(log_tail)}"
            )
        try:
            status, body = http_json("GET", f"{base_url}/health", timeout=1)
            if status == 200 and body.get("status") == "healthy":
                return
        except Exception:
            pass
        time.sleep(0.2)
    raise RuntimeError(
        "Calciforge did not become healthy. Recent output:\n"
        f"{format_log_tail(log_tail)}"
    )


def wait_for_litellm(
    base_url: str,
    deadline: float,
    proc: subprocess.Popen,
    log_tail: "collections.deque[str]",
) -> None:
    payload = {
        "model": LITELLM_MODEL,
        "messages": [{"role": "user", "content": "reply exactly ok"}],
        "max_tokens": 8,
    }
    headers = {"Authorization": f"Bearer {LITELLM_GATEWAY_KEY}"}
    last_status = None
    last_body: dict = {}
    while time.monotonic() < deadline:
        exit_code = proc.poll()
        if exit_code is not None:
            raise RuntimeError(
                "LiteLLM exited before serving requests "
                f"(code {exit_code}). Recent output:\n{format_log_tail(log_tail)}"
            )
        try:
            last_status, last_body = http_json(
                "POST",
                f"{base_url}/v1/chat/completions",
                payload,
                headers=headers,
                timeout=2,
            )
            if last_status == 200:
                drain_seen_requests()
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise RuntimeError(
        f"LiteLLM did not become ready; last response {last_status}: {last_body}. "
        f"Recent output:\n{format_log_tail(log_tail)}"
    )


def drain_seen_requests() -> None:
    while True:
        try:
            MockOpenAiHandler.seen.get_nowait()
        except queue.Empty:
            return


def terminate(proc: subprocess.Popen) -> None:
    if proc.poll() is not None:
        return
    if hasattr(os, "killpg"):
        os.killpg(proc.pid, signal.SIGTERM)
    else:
        proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()


def start_process(cmd: list[str], env: dict[str, str] | None = None) -> tuple[subprocess.Popen, "collections.deque[str]"]:
    print("+", " ".join(shlex.quote(part) for part in cmd), flush=True)
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=env,
        start_new_session=hasattr(os, "setsid"),
    )
    tail: "collections.deque[str]" = collections.deque(maxlen=100)
    drain_output(proc, tail)
    return proc, tail


def assert_upstream_request(seen: dict) -> None:
    if seen["path"] != "/v1/chat/completions":
        raise AssertionError(f"LiteLLM used the wrong upstream path: {seen}")
    if seen["authorization"] != f"Bearer {UPSTREAM_PROVIDER_KEY}":
        raise AssertionError("LiteLLM did not own and forward the upstream provider key")

    upstream_model = seen["body"].get("model")
    accepted = {UPSTREAM_MODEL, f"openai/{UPSTREAM_MODEL}"}
    if upstream_model not in accepted:
        raise AssertionError(
            "LiteLLM did not translate the Calciforge selector to its configured "
            f"upstream model; saw {upstream_model!r}"
        )
    if upstream_model in {CALCIFORGE_MODEL, LITELLM_MODEL}:
        raise AssertionError(f"upstream saw a Calciforge/LiteLLM public selector: {seen}")


def main() -> int:
    parser = argparse.ArgumentParser(
        epilog=(
            "Set LITELLM_COMMAND to choose the LiteLLM launcher, for example: "
            "LITELLM_COMMAND=\"uvx --from litellm[proxy] litellm\"."
        )
    )
    parser.add_argument("--startup-timeout", type=float, default=120.0)
    args = parser.parse_args()

    gateway_port = find_free_port()
    litellm_port = find_free_port()
    upstream_port = find_free_port()
    calciforge_base = f"http://127.0.0.1:{gateway_port}"
    litellm_base = f"http://127.0.0.1:{litellm_port}"

    upstream = ThreadingHTTPServer(("127.0.0.1", upstream_port), MockOpenAiHandler)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()

    litellm_proc: subprocess.Popen | None = None
    calciforge_proc: subprocess.Popen | None = None

    with tempfile.TemporaryDirectory(prefix="calciforge-litellm-smoke-") as raw_tmp:
        tmp = Path(raw_tmp)
        litellm_config = write_litellm_config(tmp, upstream_port)
        calciforge_config = write_calciforge_config(tmp, gateway_port, litellm_port)
        home_dir = tmp / "home"
        home_dir.mkdir()

        litellm_env = os.environ.copy()
        litellm_env["HOME"] = str(home_dir)
        litellm_proc, litellm_tail = start_process(
            litellm_command(litellm_config, litellm_port),
            env=litellm_env,
        )
        try:
            wait_for_litellm(
                litellm_base,
                time.monotonic() + args.startup_timeout,
                litellm_proc,
                litellm_tail,
            )

            calciforge_proc, calciforge_tail = start_process(
                calciforge_command(calciforge_config),
            )
            wait_for_calciforge(
                calciforge_base,
                time.monotonic() + args.startup_timeout,
                calciforge_proc,
                calciforge_tail,
            )

            status, location = http_no_redirect("GET", f"{calciforge_base}/gateway/ui")
            expected_location = f"{litellm_base}/ui"
            if status not in (302, 303, 307, 308) or location != expected_location:
                raise AssertionError(
                    f"unexpected /gateway/ui redirect {status} to {location!r}"
                )

            status, completion = http_json(
                "POST",
                f"{calciforge_base}/v1/chat/completions",
                {
                    "model": CALCIFORGE_MODEL,
                    "messages": [{"role": "user", "content": "reply exactly ok"}],
                    "max_tokens": 8,
                },
                headers={"Authorization": f"Bearer {CALCIFORGE_CLIENT_KEY}"},
            )
            if status != 200:
                raise AssertionError(f"Calciforge chat completion failed {status}: {completion}")
            content = completion["choices"][0]["message"]["content"]
            if content != EXPECTED_CONTENT:
                raise AssertionError(f"unexpected completion content: {content!r}")

            seen = MockOpenAiHandler.seen.get(timeout=5)
            assert_upstream_request(seen)
            print("LiteLLM gateway smoke passed")
            print(f"Calciforge model: {CALCIFORGE_MODEL}")
            print(f"LiteLLM model group: {LITELLM_MODEL}")
            print(f"Upstream model: {seen['body'].get('model')}")
            return 0
        finally:
            upstream.shutdown()
            if calciforge_proc is not None:
                terminate(calciforge_proc)
            if litellm_proc is not None:
                terminate(litellm_proc)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"LiteLLM gateway smoke failed: {exc}", file=sys.stderr)
        raise
