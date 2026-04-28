#!/usr/bin/env python3
"""Run a real Matrix homeserver E2E test against Calciforge.

This test starts a disposable Synapse container, registers two Matrix users,
configures Calciforge as one user, opens a direct Matrix chat from the other
user, and waits for Calciforge to reply through the Matrix Client-Server API.

It intentionally complements the in-process Matrix API mock test:

- the mock test is fast and deterministic inside `cargo test`;
- this script verifies real homeserver registration, login, direct-message
  invite/join, `/sync`, `/send`, and Calciforge process wiring.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path


DEFAULT_IMAGE = "matrixdotorg/synapse:v1.117.0"
DEFAULT_PORT = 18088
SERVER_NAME = "localhost"
BOT_USER = "calciforge"
ALICE_USER = "alice"
EXPECTED_PROMPT = "hello real matrix"


def calciforge_command(config_path: Path) -> list[str]:
    configured_bin = os.environ.get("CALCIFORGE_BIN")
    if configured_bin:
        return [configured_bin, "--config", str(config_path)]

    repo_bin = Path.cwd() / "target" / "debug" / "calciforge"
    if repo_bin.exists():
        return [str(repo_bin), "--config", str(config_path)]

    return [
        "cargo",
        "run",
        "-p",
        "calciforge",
        "--features",
        "channel-matrix",
        "--",
        "--config",
        str(config_path),
    ]


def run(
    cmd: list[str],
    *,
    check: bool = True,
    display_cmd: list[str] | None = None,
    **kwargs,
) -> subprocess.CompletedProcess:
    print("+", " ".join(display_cmd or cmd), flush=True)
    return subprocess.run(cmd, check=check, text=True, **kwargs)


def require_command(name: str) -> None:
    if shutil.which(name) is None:
        raise RuntimeError(f"required command not found: {name}")


def temporary_directory(prefix: str) -> tempfile.TemporaryDirectory:
    try:
        return tempfile.TemporaryDirectory(
            prefix=prefix,
            ignore_cleanup_errors=True,
        )
    except TypeError:
        # Python < 3.10 lacks ignore_cleanup_errors. The harness clears
        # Docker-owned Synapse files before cleanup, so the fallback is fine.
        return tempfile.TemporaryDirectory(prefix=prefix)


def http_json(
    method: str,
    url: str,
    payload: dict | None = None,
    token: str | None = None,
    timeout: float = 10.0,
) -> dict:
    body = None
    headers = {"Content-Type": "application/json"}
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"

    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        data = response.read()
    if not data:
        return {}
    return json.loads(data.decode("utf-8"))


def wait_for_synapse(base_url: str, deadline: float) -> None:
    versions_url = f"{base_url}/_matrix/client/versions"
    while time.monotonic() < deadline:
        try:
            http_json("GET", versions_url, timeout=2.0)
            return
        except Exception:
            time.sleep(1)
    raise TimeoutError("Synapse did not become ready")


def register_user(container: str, user: str, password: str) -> None:
    cmd = [
        "docker",
        "exec",
        container,
        "register_new_matrix_user",
        "-c",
        "/data/homeserver.yaml",
        "-u",
        user,
        "-p",
        password,
        "-a",
        "http://127.0.0.1:8008",
    ]
    run(
        cmd,
        display_cmd=[arg if arg != password else "<generated-password>" for arg in cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def login(base_url: str, user: str, password: str) -> tuple[str, str]:
    response = http_json(
        "POST",
        f"{base_url}/_matrix/client/v3/login",
        {
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": user},
            "password": password,
            "initial_device_display_name": f"calciforge-e2e-{user}",
        },
    )
    return response["access_token"], response["user_id"]


def encoded(path_part: str) -> str:
    return urllib.parse.quote(path_part, safe="")


def create_room(base_url: str, alice_token: str, bot_user_id: str) -> str:
    response = http_json(
        "POST",
        f"{base_url}/_matrix/client/v3/createRoom",
        {
            "preset": "private_chat",
            "name": "Calciforge real Matrix E2E",
            "invite": [bot_user_id],
            "is_direct": True,
        },
        token=alice_token,
    )
    return response["room_id"]


def wait_for_bot_join(
    base_url: str,
    alice_token: str,
    room_id: str,
    bot_user_id: str,
    deadline: float,
) -> None:
    url = f"{base_url}/_matrix/client/v3/rooms/{encoded(room_id)}/joined_members"
    while time.monotonic() < deadline:
        try:
            response = http_json("GET", url, token=alice_token, timeout=5.0)
            if bot_user_id in response.get("joined", {}):
                return
        except Exception:
            pass
        time.sleep(1)
    raise TimeoutError(f"Calciforge bot did not join direct Matrix chat {room_id}")


def write_agent(path: Path, prefix: str) -> None:
    path.write_text(
        "#!/bin/sh\n"
        f"printf '{prefix}: %s\\n' \"$1\"\n",
        encoding="utf-8",
    )
    path.chmod(0o755)


def write_config(
    path: Path,
    base_url: str,
    bot_token_path: Path,
    alice_user_id: str,
    agent_path: Path,
    backup_agent_path: Path,
) -> None:
    path.write_text(
        f"""
[calciforge]
version = 2

[[identities]]
id = "alice"
display_name = "Alice"
aliases = [
  {{ channel = "matrix", id = "{alice_user_id}" }},
]

[[agents]]
id = "real-matrix-agent"
kind = "cli"
command = "{agent_path}"
args = ["{{message}}"]
timeout_ms = 5000

[[agents]]
id = "backup-agent"
kind = "cli"
command = "{backup_agent_path}"
args = ["{{message}}"]
timeout_ms = 5000

[[routing]]
identity = "alice"
default_agent = "real-matrix-agent"
allowed_agents = ["real-matrix-agent", "backup-agent"]

[context]
buffer_size = 20
inject_depth = 5

[[channels]]
kind = "matrix"
enabled = true
homeserver = "{base_url}"
access_token_file = "{bot_token_path}"
allowed_users = ["{alice_user_id}"]
""".lstrip(),
        encoding="utf-8",
    )


def stream_process_output(
    proc: subprocess.Popen,
    ready_queue: "queue.Queue[str]",
    collected: list[str],
    stream_name: str,
) -> None:
    stream = proc.stderr if stream_name == "stderr" else proc.stdout
    assert stream is not None
    for line in stream:
        collected.append(line)
        if stream_name == "stderr":
            sys.stderr.write(line)
        else:
            sys.stdout.write(line)
        if "Matrix channel listening" in line or "initial sync complete" in line:
            ready_queue.put(line)


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
    ready_queue: "queue.Queue[str]" = queue.Queue()
    threading.Thread(
        target=stream_process_output,
        args=(proc, ready_queue, collected, "stderr"),
        daemon=True,
    ).start()
    threading.Thread(
        target=stream_process_output,
        args=(proc, ready_queue, collected, "stdout"),
        daemon=True,
    ).start()

    # The workflow prebuilds Calciforge, but keep this deadline high enough for
    # local cargo fallback runs when a developer has not built the binary yet.
    deadline = time.monotonic() + 240
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(
                f"Calciforge exited before Matrix readiness; code={proc.returncode}"
            )
        try:
            ready_queue.get(timeout=1)
            return proc, collected
        except queue.Empty:
            pass
    raise TimeoutError("Calciforge did not report Matrix readiness")


def send_message(base_url: str, alice_token: str, room_id: str, body: str) -> None:
    txn_id = f"calciforge-e2e-{uuid.uuid4().hex}"
    http_json(
        "PUT",
        f"{base_url}/_matrix/client/v3/rooms/{encoded(room_id)}/send/m.room.message/{txn_id}",
        {"msgtype": "m.text", "body": body},
        token=alice_token,
    )


def wait_for_reply(
    base_url: str,
    alice_token: str,
    room_id: str,
    bot_user_id: str,
    expected_reply_fragment: str,
    deadline: float,
) -> None:
    room_messages_url = (
        f"{base_url}/_matrix/client/v3/rooms/{encoded(room_id)}/messages"
        "?dir=b&limit=20"
    )
    while time.monotonic() < deadline:
        try:
            history = http_json(
                "GET",
                room_messages_url,
                token=alice_token,
                timeout=5.0,
            )
        except urllib.error.HTTPError as exc:
            print(
                f"Matrix room history failed while waiting for reply: {exc}",
                file=sys.stderr,
            )
            time.sleep(1)
            continue

        events = history.get("chunk", [])
        for event in events:
            if event.get("sender") != bot_user_id:
                continue
            content = event.get("content", {})
            body = content.get("body", "")
            if expected_reply_fragment in body:
                return
        time.sleep(1)
    raise TimeoutError(
        f"did not receive expected Matrix reply containing: {expected_reply_fragment!r}"
    )


def send_and_expect(
    base_url: str,
    alice_token: str,
    room_id: str,
    bot_user_id: str,
    body: str,
    expected_reply_fragment: str,
) -> None:
    send_message(base_url, alice_token, room_id, body)
    wait_for_reply(
        base_url,
        alice_token,
        room_id,
        bot_user_id,
        expected_reply_fragment,
        time.monotonic() + 60,
    )


def stop_process(proc: subprocess.Popen | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(timeout=10)
    except Exception:
        os.killpg(proc.pid, signal.SIGKILL)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default=os.environ.get("MATRIX_E2E_IMAGE", DEFAULT_IMAGE))
    parser.add_argument("--port", type=int, default=int(os.environ.get("MATRIX_E2E_PORT", DEFAULT_PORT)))
    parser.add_argument("--keep", action="store_true", help="leave temp files/container for debugging")
    args = parser.parse_args()

    repo_calciforge_bin = Path(__file__).resolve().parents[1] / "target" / "debug" / "calciforge"
    has_calciforge_bin = bool(os.environ.get("CALCIFORGE_BIN")) or repo_calciforge_bin.exists()
    if not has_calciforge_bin:
        require_command("cargo")
    require_command("docker")

    base_url = f"http://127.0.0.1:{args.port}"
    container = f"calciforge-matrix-e2e-{os.getpid()}"
    registration_secret = f"calciforge-e2e-{uuid.uuid4().hex}"
    bot_password = f"calciforge-e2e-bot-{uuid.uuid4().hex}"
    alice_password = f"calciforge-e2e-alice-{uuid.uuid4().hex}"
    calciforge_proc: subprocess.Popen | None = None
    tmp_obj = temporary_directory(prefix="calciforge-matrix-e2e-")
    tmp = Path(tmp_obj.name)
    data_dir = tmp / "synapse"
    data_dir.mkdir()

    try:
        run(
            [
                "docker",
                "run",
                "--rm",
                "-v",
                f"{data_dir}:/data",
                "-e",
                f"SYNAPSE_SERVER_NAME={SERVER_NAME}",
                "-e",
                "SYNAPSE_REPORT_STATS=no",
                args.image,
                "generate",
            ]
        )
        run(
            [
                "docker",
                "run",
                "--rm",
                "-v",
                f"{data_dir}:/data",
                "-e",
                "REGISTRATION_SHARED_SECRET",
                "--entrypoint",
                "/bin/sh",
                args.image,
                "-c",
                "printf '\\nregistration_shared_secret: %s\\n' \"$REGISTRATION_SHARED_SECRET\" >> /data/homeserver.yaml",
            ],
            env={**os.environ, "REGISTRATION_SHARED_SECRET": registration_secret},
        )

        run(
            [
                "docker",
                "run",
                "--rm",
                "-d",
                "--name",
                container,
                "-p",
                f"127.0.0.1:{args.port}:8008",
                "-v",
                f"{data_dir}:/data",
                args.image,
            ],
            stdout=subprocess.PIPE,
        )
        wait_for_synapse(base_url, time.monotonic() + 90)

        register_user(container, BOT_USER, bot_password)
        register_user(container, ALICE_USER, alice_password)
        bot_token, bot_user_id = login(base_url, BOT_USER, bot_password)
        alice_token, alice_user_id = login(base_url, ALICE_USER, alice_password)
        token_path = tmp / "matrix-bot-token"
        token_path.write_text(f"{bot_token}\n", encoding="utf-8")
        agent_path = tmp / "real-matrix-agent"
        write_agent(agent_path, "real-matrix-agent saw")
        backup_agent_path = tmp / "backup-agent"
        write_agent(backup_agent_path, "backup-matrix-agent saw")
        config_path = tmp / "calciforge.toml"
        write_config(
            config_path,
            base_url,
            token_path,
            alice_user_id,
            agent_path,
            backup_agent_path,
        )

        calciforge_proc, _logs = start_calciforge(config_path, tmp)
        room_id = create_room(base_url, alice_token, bot_user_id)
        wait_for_bot_join(
            base_url,
            alice_token,
            room_id,
            bot_user_id,
            time.monotonic() + 30,
        )

        command_cases = [
            ("!ping", "pong"),
            ("!help", "Calciforge"),
            ("!agents", "backup-agent"),
            ("!status", "active agent: real-matrix-agent"),
            ("!metrics", "messages routed: 0"),
            ("!model", "No model shortcuts or synthetic models configured."),
            ("!sessions real-matrix-agent", "does not support session listing"),
        ]
        for body, expected in command_cases:
            send_and_expect(
                base_url,
                alice_token,
                room_id,
                bot_user_id,
                body,
                expected,
            )

        send_and_expect(
            base_url,
            alice_token,
            room_id,
            bot_user_id,
            "!switch backup-agent",
            "Your messages will now route to backup-agent",
        )
        send_and_expect(
            base_url,
            alice_token,
            room_id,
            bot_user_id,
            "after switch",
            "backup-matrix-agent saw: after switch",
        )
        send_and_expect(
            base_url,
            alice_token,
            room_id,
            bot_user_id,
            "!default",
            "Switched to default agent: real-matrix-agent",
        )
        send_and_expect(
            base_url,
            alice_token,
            room_id,
            bot_user_id,
            EXPECTED_PROMPT,
            EXPECTED_PROMPT,
        )
        print("real Matrix E2E passed")
        return 0
    finally:
        stop_process(calciforge_proc)
        if not args.keep:
            run(["docker", "rm", "-f", container], check=False, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "-v",
                    f"{data_dir}:/data",
                    "--entrypoint",
                    "/bin/sh",
                    args.image,
                    "-c",
                    "rm -rf /data/* /data/.[!.]* /data/..?* 2>/dev/null || true",
                ],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
            )
            tmp_obj.cleanup()
        else:
            print(f"kept temp dir: {tmp}")


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"matrix real E2E failed: {exc}", file=sys.stderr)
        raise
