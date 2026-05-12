#!/usr/bin/env python3
"""Exercise artifact-cli through Calciforge's mock channel.

The test starts a real Calciforge process with a mock channel and a deterministic
media-brief artifact recipe. It sends a user prompt through HTTP, verifies that
the adapter returned image, audio, and text artifacts in the safe fallback, and
checks that the generated files exist under Calciforge's per-run artifact root.
"""

from __future__ import annotations

import json
import os
import shutil
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
        return [configured_bin, "--config", str(config_path)]

    repo_bin = Path.cwd() / "target" / "debug" / "calciforge"
    if repo_bin.exists():
        return [str(repo_bin), "--config", str(config_path)]

    return ["cargo", "run", "-p", "calciforge", "--", "--config", str(config_path)]


def http_json(method: str, url: str, payload: dict | None = None, timeout: float = 10.0) -> dict:
    body = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method=method,
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        data = response.read()
        return json.loads(data.decode("utf-8")) if data else {}


def wait_for_health(base_url: str, proc: subprocess.Popen, logs: list[str]) -> None:
    deadline = time.monotonic() + 45
    last_error = ""
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise AssertionError(
                f"calciforge exited before mock health was ready ({proc.returncode})\n"
                + "\n".join(logs[-80:])
            )
        try:
            payload = http_json("GET", f"{base_url}/health", timeout=1)
            if payload.get("success") is True:
                return
        except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
            last_error = str(exc)
        time.sleep(0.25)
    raise AssertionError(f"mock channel health did not become ready: {last_error}")


def post_message(base_url: str, sender: str, text: str) -> str:
    payload = http_json(
        "POST",
        f"{base_url}/send",
        {"sender": sender, "text": text},
        timeout=30,
    )
    if payload.get("success") is not True:
        raise AssertionError(f"mock send failed: {payload}")
    return str(payload.get("data", {}).get("response", ""))


def write_config(config_path: Path, recipe_path: Path, control_port: int) -> None:
    recipe = str(recipe_path).replace("\\", "\\\\").replace('"', '\\"')
    config_path.write_text(
        f"""
[calciforge]
version = 2

[[identities]]
id = "tester"
display_name = "Test Operator"
aliases = [{{ channel = "mock", id = "tester" }}]
role = "owner"

[[agents]]
id = "media-brief"
kind = "artifact-cli"
command = "{recipe}"
timeout_ms = 30000
aliases = ["media"]
registry = {{ display_name = "Media Brief Demo", specialties = ["images", "audio", "briefs"] }}

[[routing]]
identity = "tester"
default_agent = "media-brief"
allowed_agents = ["media-brief"]

[[channels]]
kind = "mock"
enabled = true
control_port = {control_port}
""".strip()
        + "\n",
        encoding="utf-8",
    )


def collect_logs(proc: subprocess.Popen, logs: list[str]) -> threading.Thread:
    def reader() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            logs.append(line.rstrip())

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    return thread


def assert_generated_artifacts(tmp_root: Path) -> None:
    artifact_root = tmp_root / "calciforge-artifacts"
    if not artifact_root.is_dir():
        raise AssertionError(f"artifact root was not created: {artifact_root}")

    run_dirs = [path for path in artifact_root.iterdir() if path.is_dir()]
    if len(run_dirs) != 1:
        raise AssertionError(f"expected one artifact run dir under {artifact_root}, got {run_dirs}")

    run_dir = run_dirs[0]
    expected = {
        "cover.png": b"\x89PNG\r\n\x1a\n",
        "intro.wav": b"RIFF",
        "brief.md": b"# Media Brief",
    }
    for name, header in expected.items():
        path = run_dir / name
        if not path.is_file():
            raise AssertionError(f"missing generated artifact: {path}")
        if not path.read_bytes().startswith(header):
            raise AssertionError(f"generated artifact has unexpected header: {path}")


def main() -> int:
    repo = Path.cwd()
    recipe_path = repo / "examples" / "agent-recipes" / "media-brief-demo"
    if not recipe_path.is_file():
        raise AssertionError(f"missing media brief recipe: {recipe_path}")

    with tempfile.TemporaryDirectory(prefix="calciforge-artifact-e2e-") as tmp:
        tmp_path = Path(tmp)
        home = tmp_path / "home"
        tmp_root = tmp_path / "tmp"
        home.mkdir()
        tmp_root.mkdir()
        config_path = tmp_path / "calciforge.toml"
        port = find_free_port()
        write_config(config_path, recipe_path, port)

        env = os.environ.copy()
        env["HOME"] = str(home)
        env["TMPDIR"] = str(tmp_root)
        env["RUST_LOG"] = env.get("RUST_LOG", "calciforge=info")

        command = calciforge_command(config_path)
        if command[0] == "cargo" and shutil.which("cargo") is None:
            raise AssertionError("cargo is required when CALCIFORGE_BIN is not set")

        logs: list[str] = []
        proc = subprocess.Popen(
            command,
            cwd=repo,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        collect_logs(proc, logs)

        try:
            base_url = f"http://127.0.0.1:{port}"
            wait_for_health(base_url, proc, logs)
            response = post_message(
                base_url,
                "tester",
                "Create a voice-support training media kit for handling a delayed order.",
            )

            required = [
                "Media brief ready for:",
                "Attachments:",
                "image/png: cover.png",
                "audio/wav: intro.wav",
                "text/plain: brief.md",
            ]
            for needle in required:
                if needle not in response:
                    raise AssertionError(f"response missing {needle!r}:\n{response}")
            if str(tmp_root) in response or "calciforge-artifacts" in response:
                raise AssertionError(f"response leaked local artifact path:\n{response}")

            messages = http_json("GET", f"{base_url}/messages", timeout=5)
            sent = messages.get("data", {}).get("sent", [])
            if not sent or "Media brief ready for:" not in sent[-1].get("text", ""):
                raise AssertionError(f"mock sent history did not record artifact response: {messages}")

            assert_generated_artifacts(tmp_root)
            print("artifact recipe mock e2e passed")
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
