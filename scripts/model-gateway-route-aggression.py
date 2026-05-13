#!/usr/bin/env python3
"""Probe a live Calciforge model gateway with configured model selectors.

This is an operator-side aggression smoke, not a CI unit test. It reads a real
Calciforge TOML config, discovers exact model selectors from provider routes,
then sends a tiny OpenAI-compatible chat-completions request through the live
gateway. The goal is to catch deployment drift where doctor validates a route
graph but the configured gateway/provider cannot actually serve a model.
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


DEFAULT_CONFIG_PATHS = (
    Path(os.environ.get("CALCIFORGE_CONFIG", "")),
    Path.home() / ".config" / "calciforge" / "config.toml",
    Path("/opt/homebrew/etc/calciforge/config.toml"),
    Path("/etc/calciforge/config.toml"),
)


def existing_default_config() -> Path | None:
    for path in DEFAULT_CONFIG_PATHS:
        if str(path) and path.exists():
            return path
    return None


def load_config(path: Path) -> dict[str, Any]:
    with path.open("rb") as fh:
        data = fh.read()

    try:
        import tomllib  # type: ignore[import-not-found]

        return tomllib.loads(data.decode("utf-8"))
    except ModuleNotFoundError:
        try:
            import tomli  # type: ignore[import-not-found]

            return tomli.loads(data.decode("utf-8"))
        except ModuleNotFoundError:
            return parse_minimal_toml(data.decode("utf-8"))


def parse_minimal_toml(text: str) -> dict[str, Any]:
    """Parse the small TOML subset this smoke needs on older Python.

    Full TOML parsing comes from tomllib/tomli when available. This fallback is
    intentionally narrow: strings, booleans, and single-line arrays of strings
    under the sections used for gateway route discovery.
    """

    root: dict[str, Any] = {}
    current: dict[str, Any] | None = root
    for raw_line in text.splitlines():
        line = strip_comment(raw_line).strip()
        if not line:
            continue

        if line.startswith("[[") and line.endswith("]]"):
            section = line[2:-2].strip()
            parent, key = ensure_parent(root, section)
            entry: dict[str, Any] = {}
            parent.setdefault(key, []).append(entry)
            current = entry
            continue

        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip()
            current, _ = ensure_parent(root, section, terminal_is_table=True)
            continue

        if current is None or "=" not in line:
            continue
        key, value = line.split("=", 1)
        current[key.strip()] = parse_value(value.strip())

    return root


def ensure_parent(
    root: dict[str, Any],
    dotted: str,
    terminal_is_table: bool = False,
) -> tuple[dict[str, Any], str]:
    parts = dotted.split(".")
    parent = root
    for part in parts[:-1]:
        next_parent = parent.setdefault(part, {})
        if isinstance(next_parent, list):
            if not next_parent or not isinstance(next_parent[-1], dict):
                next_parent.append({})
            next_parent = next_parent[-1]
        parent = next_parent
    key = parts[-1]
    if terminal_is_table:
        next_parent = parent.setdefault(key, {})
        if isinstance(next_parent, list):
            if not next_parent or not isinstance(next_parent[-1], dict):
                next_parent.append({})
            next_parent = next_parent[-1]
        parent = next_parent
    return parent, key


def strip_comment(line: str) -> str:
    in_string = False
    escaped = False
    for idx, char in enumerate(line):
        if escaped:
            escaped = False
            continue
        if char == "\\" and in_string:
            escaped = True
            continue
        if char == '"':
            in_string = not in_string
            continue
        if char == "#" and not in_string:
            return line[:idx]
    return line


def parse_value(value: str) -> Any:
    if value == "true":
        return True
    if value == "false":
        return False
    try:
        return ast.literal_eval(value)
    except (SyntaxError, ValueError):
        return value.strip('"')


def read_secret(value: str | None, file_path: str | None) -> str | None:
    if value:
        return value.strip()
    if file_path:
        return Path(file_path).read_text(encoding="utf-8").strip()
    return None


def gateway_base_url(proxy: dict[str, Any], override: str | None) -> str:
    if override:
        return override.rstrip("/")

    bind = str(proxy.get("bind", "127.0.0.1:8080"))
    host, port = parse_bind(bind)
    if host in ("", "0.0.0.0", "::"):
        host = "127.0.0.1"
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"http://{host}:{port}"


def parse_bind(bind: str) -> tuple[str, int]:
    if bind.startswith("["):
        host, _, tail = bind[1:].partition("]")
        if not tail.startswith(":"):
            raise ValueError(f"bind address lacks port: {bind}")
        return host, int(tail[1:])
    host, sep, port = bind.rpartition(":")
    if not sep:
        raise ValueError(f"bind address lacks port: {bind}")
    return host, int(port)


def exact_model_selectors(config: dict[str, Any], include_synthetic: bool) -> list[str]:
    proxy = config.get("proxy") or {}
    selectors: list[str] = []

    for route in proxy.get("model_routes") or []:
        add_exact(selectors, route.get("pattern"))

    for provider in proxy.get("providers") or []:
        for model in provider.get("models") or []:
            add_exact(selectors, model)

    if include_synthetic:
        for shortcut in config.get("model_shortcuts") or []:
            add_exact(selectors, shortcut.get("alias"))
        for role in config.get("model_roles") or []:
            add_exact(selectors, role.get("role"))
        for key in ("alloys", "cascades", "dispatchers"):
            for entry in config.get(key) or []:
                add_exact(selectors, entry.get("id"))

    return selectors


def add_exact(selectors: list[str], value: Any) -> None:
    if not isinstance(value, str) or not value:
        return
    if "*" in value:
        return
    if value not in selectors:
        selectors.append(value)


def chat_completion(
    base_url: str,
    api_key: str | None,
    model: str,
    timeout: float,
    expected: str,
    max_tokens: int,
) -> tuple[bool, str, float]:
    payload = {
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": f"Reply with exactly: {expected}",
            }
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": False,
    }
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    request = urllib.request.Request(
        f"{base_url}/v1/chat/completions",
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
        method="POST",
    )

    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = response.read().decode("utf-8", errors="replace")
            elapsed = time.monotonic() - started
            if response.status != 200:
                return False, f"HTTP {response.status}: {trim(body)}", elapsed
            try:
                parsed = json.loads(body)
            except json.JSONDecodeError as exc:
                return False, f"invalid JSON response: {exc}: {trim(body)}", elapsed
            content = assistant_content(parsed)
            if content.strip() != expected:
                return False, unexpected_content_detail(parsed, content), elapsed
            return True, content.strip(), elapsed
    except urllib.error.HTTPError as exc:
        elapsed = time.monotonic() - started
        body = exc.read().decode("utf-8", errors="replace")
        return False, f"HTTP {exc.code}: {trim(body)}", elapsed
    except (TimeoutError, OSError, socket.timeout) as exc:
        elapsed = time.monotonic() - started
        return False, f"{type(exc).__name__}: {exc}", elapsed


def assistant_content(parsed: dict[str, Any]) -> str:
    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices:
        return ""
    message = choices[0].get("message") if isinstance(choices[0], dict) else None
    if not isinstance(message, dict):
        return ""
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, dict) and isinstance(item.get("text"), str):
                parts.append(item["text"])
        return "".join(parts)
    return ""


def unexpected_content_detail(parsed: dict[str, Any], content: str) -> str:
    choice = first_choice(parsed)
    finish_reason = choice.get("finish_reason") if isinstance(choice, dict) else None
    message = choice.get("message") if isinstance(choice, dict) else None
    reasoning = None
    if isinstance(message, dict):
        reasoning = message.get("reasoning") or message.get("reasoning_content")
    detail = f"unexpected content {content!r}"
    if finish_reason:
        detail += f"; finish_reason={finish_reason!r}"
    if isinstance(reasoning, str) and reasoning:
        detail += f"; reasoning_prefix={trim(reasoning, 160)!r}"
    return detail


def first_choice(parsed: dict[str, Any]) -> dict[str, Any] | None:
    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices:
        return None
    choice = choices[0]
    return choice if isinstance(choice, dict) else None


def trim(text: str, limit: int = 500) -> str:
    text = text.replace("\n", "\\n")
    if len(text) <= limit:
        return text
    return f"{text[:limit]}..."


def main() -> int:
    default_config = existing_default_config()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        type=Path,
        default=default_config,
        help="Calciforge config path; defaults to CALCIFORGE_CONFIG, ~/.config, Homebrew, then /etc",
    )
    parser.add_argument("--base-url", help="Override Calciforge gateway base URL")
    parser.add_argument("--model", action="append", help="Model selector to probe; repeatable")
    parser.add_argument(
        "--include-synthetic",
        action="store_true",
        help="Also probe shortcuts, roles, alloys, cascades, and dispatchers",
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--expected", default="PONG")
    parser.add_argument(
        "--max-tokens",
        type=int,
        default=512,
        help="Output budget for the smoke prompt; keep high enough for reasoning models to finish thinking",
    )
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON")
    args = parser.parse_args()

    if args.config is None:
        print("No Calciforge config found; pass --config", file=sys.stderr)
        return 2

    config = load_config(args.config)
    proxy = config.get("proxy") or {}
    if not proxy.get("enabled", False):
        print(f"proxy disabled in {args.config}", file=sys.stderr)
        return 2

    models = args.model or exact_model_selectors(config, args.include_synthetic)
    if not models:
        print("No exact model selectors found; pass --model or add exact route/provider models", file=sys.stderr)
        return 2

    base_url = gateway_base_url(proxy, args.base_url)
    api_key = read_secret(proxy.get("api_key"), proxy.get("api_key_file"))

    results = []
    ok = True
    for model in models:
        passed, detail, elapsed = chat_completion(
            base_url=base_url,
            api_key=api_key,
            model=model,
            timeout=args.timeout,
            expected=args.expected,
            max_tokens=args.max_tokens,
        )
        ok = ok and passed
        result = {
            "model": model,
            "ok": passed,
            "elapsed_seconds": round(elapsed, 3),
            "detail": detail,
        }
        results.append(result)
        if not args.json:
            mark = "ok" if passed else "FAIL"
            print(f"{mark:4} {model} {elapsed:.3f}s {detail}")

    if args.json:
        print(json.dumps({"base_url": base_url, "results": results}, indent=2))

    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
