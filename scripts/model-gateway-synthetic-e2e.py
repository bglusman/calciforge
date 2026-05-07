#!/usr/bin/env python3
"""Run synthetic-model E2E tests against Calciforge's model gateway.

This starts Calciforge with the deterministic mock backend and exercises the
OpenAI-compatible API through real HTTP. It verifies routing behavior for the
synthetic model primitives without requiring real provider credentials.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
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


def http_json(
    method: str,
    url: str,
    payload: dict | None = None,
    timeout: float = 10.0,
) -> tuple[int, dict]:
    body = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            data = response.read()
            return response.status, json.loads(data.decode("utf-8")) if data else {}
    except urllib.error.HTTPError as exc:
        data = exc.read()
        return exc.code, json.loads(data.decode("utf-8")) if data else {}


def write_config(tmp: Path, port: int) -> Path:
    config = f"""
[calciforge]
version = 2

[proxy]
enabled = true
bind = "127.0.0.1:{port}"
backend_type = "mock"
timeout_seconds = 30

[proxy.token_estimator]
strategy = "char_ratio"
chars_per_token = 1.0
safety_margin = 1.0

[[alloys]]
id = "alloy-round-robin"
name = "Round Robin Alloy"
strategy = "round_robin"
min_context_window = 120

[[alloys.constituents]]
model = "gpt-4"
weight = 1
context_window = 120

[[alloys.constituents]]
model = "claude-3-5-sonnet"
weight = 1
context_window = 120

[[cascades]]
id = "cascade-size-aware"
name = "Size Aware Cascade"

[[cascades.models]]
model = "gpt-4"
context_window = 40

[[cascades.models]]
model = "kimi-free"
context_window = 140

[[dispatchers]]
id = "dispatcher-size-aware"
name = "Size Aware Dispatcher"

[[dispatchers.models]]
model = "gpt-4"
context_window = 40

[[dispatchers.models]]
model = "claude-3-5-sonnet"
context_window = 90

[[dispatchers.models]]
model = "kimi-free"
context_window = 140

"""
    config_path = tmp / "calciforge.toml"
    config_path.write_text(config, encoding="utf-8")
    return config_path


def stream_output(
    pipe,
    ready_queue: "queue.Queue[None]",
    collected: list[str],
    label: str,
) -> None:
    for line in iter(pipe.readline, ""):
        line = line.rstrip()
        collected.append(line)
        print(line, file=sys.stderr if label == "stderr" else sys.stdout, flush=True)
        if "Starting model gateway on" in line:
            ready_queue.put(None)


def start_calciforge(config_path: Path, home_dir: Path) -> tuple[subprocess.Popen, list[str]]:
    env = os.environ.copy()
    original_home = env.get("HOME")
    if original_home:
        env.setdefault("RUSTUP_HOME", str(Path(original_home) / ".rustup"))
    env["HOME"] = str(home_dir)
    env.setdefault("RUST_LOG", "calciforge=info")

    cmd = calciforge_command(config_path)
    print("+", " ".join(cmd), flush=True)
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        start_new_session=True,
    )

    collected: list[str] = []
    ready_queue: "queue.Queue[None]" = queue.Queue()
    threading.Thread(
        target=stream_output,
        args=(proc.stderr, ready_queue, collected, "stderr"),
        daemon=True,
    ).start()
    threading.Thread(
        target=stream_output,
        args=(proc.stdout, ready_queue, collected, "stdout"),
        daemon=True,
    ).start()

    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(
                f"Calciforge exited before proxy readiness; code={proc.returncode}"
            )
        try:
            ready_queue.get(timeout=1)
            return proc, collected
        except queue.Empty:
            pass
    raise TimeoutError("Calciforge did not report proxy readiness")


def wait_for_health(base_url: str, deadline: float) -> None:
    while time.monotonic() < deadline:
        try:
            status, body = http_json("GET", f"{base_url}/health", timeout=2.0)
            if status == 200 and body.get("status") == "healthy":
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise TimeoutError("Calciforge proxy health endpoint did not become ready")


def chat(base_url: str, model: str, content: str, max_tokens: int = 2) -> tuple[int, dict]:
    return http_json(
        "POST",
        f"{base_url}/v1/chat/completions",
        {
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": max_tokens,
        },
        timeout=10.0,
    )


def assert_model(base_url: str, model: str, content: str, expected_model: str) -> None:
    status, body = chat(base_url, model, content)
    if status != 200:
        raise AssertionError(f"{model}: expected 200, got {status}: {body}")
    actual_model = body.get("model")
    if actual_model != expected_model:
        raise AssertionError(
            f"{model}: expected routed model {expected_model}, got {actual_model}: {body}"
        )
    text = (
        body.get("choices", [{}])[0]
        .get("message", {})
        .get("content", "")
    )
    if expected_model not in text and "mock" not in text.lower():
        raise AssertionError(f"{model}: response content did not look mocked: {body}")


def assert_context_exceeded(base_url: str, model: str, content: str) -> None:
    status, body = chat(base_url, model, content)
    if status != 400:
        raise AssertionError(f"{model}: expected 400 for oversized request, got {status}: {body}")
    code = body.get("error", {}).get("code")
    if code != "context_window_exceeded":
        raise AssertionError(f"{model}: expected context_window_exceeded, got {body}")


def run_assertions(base_url: str) -> None:
    status, models_body = http_json("GET", f"{base_url}/v1/models", timeout=5.0)
    if status != 200:
        raise AssertionError(f"model list failed: {status}: {models_body}")
    model_ids = {item["id"] for item in models_body.get("data", [])}
    expected_ids = {
        "alloy-round-robin",
        "cascade-size-aware",
        "dispatcher-size-aware",
    }
    missing = expected_ids - model_ids
    if missing:
        raise AssertionError(f"model list missing synthetic ids {sorted(missing)}: {model_ids}")

    assert_model(base_url, "alloy-round-robin", "short", "gpt-4")
    assert_model(base_url, "alloy-round-robin", "short", "claude-3-5-sonnet")
    assert_model(base_url, "cascade-size-aware", "x" * 80, "kimi/kimi-free")
    assert_model(base_url, "dispatcher-size-aware", "short", "gpt-4")
    assert_model(base_url, "dispatcher-size-aware", "x" * 60, "claude-3-5-sonnet")
    assert_model(base_url, "dispatcher-size-aware", "x" * 110, "kimi/kimi-free")
    assert_context_exceeded(base_url, "dispatcher-size-aware", "x" * 200)


def stop_process(proc: subprocess.Popen) -> None:
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=5)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()

    port = args.port or find_free_port()
    base_url = f"http://127.0.0.1:{port}"

    with tempfile.TemporaryDirectory(prefix="calciforge-synthetic-e2e-") as tmp_raw:
        tmp = Path(tmp_raw)
        config_path = write_config(tmp, port)
        command = calciforge_command(config_path)
        if command and command[0] == "cargo" and shutil.which("cargo") is None:
            raise RuntimeError("cargo or CALCIFORGE_BIN is required")
        proc = None
        try:
            proc, _logs = start_calciforge(config_path, tmp)
            wait_for_health(base_url, time.monotonic() + 30)
            run_assertions(base_url)
        finally:
            if proc is not None:
                stop_process(proc)

    print("model gateway synthetic E2E passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"model gateway synthetic E2E failed: {exc}", file=sys.stderr)
        raise
