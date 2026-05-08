#!/usr/bin/env python3
"""Regression tests for scripts/lib/upsert-calciforge-agent.py."""

from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - compatibility with older macOS Python.
    tomllib = None


ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "lib" / "upsert-calciforge-agent.py"


def run_helper(config: pathlib.Path, endpoint: str = "http://127.0.0.1:19090") -> None:
    subprocess.run(
        [
            sys.executable,
            str(HELPER),
            str(config),
            "hermes",
            "hermes",
            endpoint,
            "300000",
            "hermes",
            "/tmp/fake-api-key",
            "true",
            "opencode-go/kimi-k2.6",
        ],
        check=True,
        text=True,
        capture_output=True,
    )


def assert_valid_toml(config: pathlib.Path) -> None:
    if tomllib is not None:
        tomllib.loads(config.read_text())


def test_last_agent_does_not_rewrite_following_tables() -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        config = pathlib.Path(tmpdir) / "config.toml"
        config.write_text(
            """
[calciforge]
version = 2

[[agents]]
id = "hermes"
kind = "hermes"
endpoint = "http://127.0.0.1:19090"

[[routing]]
identity = "owner"
endpoint = "must-not-change"
allowed_agents = ["hermes"]
""".lstrip()
        )

        run_helper(config)

        text = config.read_text()
        assert_valid_toml(config)
        assert 'endpoint = "must-not-change"' in text
        assert text.count('endpoint = "http://127.0.0.1:19090"') == 1


def test_nested_registry_table_prevents_inline_registry_collision() -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        config = pathlib.Path(tmpdir) / "config.toml"
        config.write_text(
            """
[calciforge]
version = 2

[[agents]]
id = "hermes"
kind = "hermes"
endpoint = "http://127.0.0.1:19090"

[agents.registry]
display_name = "Hermes"

[[routing]]
identity = "owner"
allowed_agents = ["hermes"]
""".lstrip()
        )

        run_helper(config)

        text = config.read_text()
        assert_valid_toml(config)
        assert "[agents.registry]" in text
        assert "registry = {" not in text
        assert 'timeout_ms = 300000' in text
        assert text.index("timeout_ms = 300000") < text.index("[agents.registry]")


def test_updating_first_agent_preserves_second_agent_and_routing() -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        config = pathlib.Path(tmpdir) / "config.toml"
        second_agent = """
[[agents]]
id = "ironclaw"
kind = "ironclaw"
endpoint = "http://127.0.0.1:19191"
api_key_file = "/tmp/ironclaw-key"

[agents.registry]
display_name = "IronClaw"
""".strip()
        routing = """
[[routing]]
identity = "owner"
endpoint = "must-not-change"
allowed_agents = ["hermes", "ironclaw"]
""".strip()
        config.write_text(
            f"""
[calciforge]
version = 2

[[agents]]
id = "hermes"
kind = "hermes"
endpoint = "http://old.example.invalid"

{second_agent}

{routing}
""".lstrip()
        )

        run_helper(config)

        text = config.read_text()
        assert_valid_toml(config)
        assert second_agent in text
        assert routing in text
        assert 'endpoint = "http://old.example.invalid"' not in text
        assert text.count('endpoint = "http://127.0.0.1:19090"') == 1


def test_replacement_values_with_backslashes_remain_valid_toml() -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        config = pathlib.Path(tmpdir) / "config.toml"
        config.write_text(
            """
[calciforge]
version = 2

[[agents]]
id = "hermes"
kind = "hermes"
endpoint = "http://old.example.invalid"
""".lstrip()
        )

        endpoint = r"http://127.0.0.1:19090/C:\\tmp\\agent"
        run_helper(config, endpoint)

        text = config.read_text()
        assert_valid_toml(config)
        assert 'endpoint = "http://old.example.invalid"' not in text
        if tomllib is not None:
            parsed = tomllib.loads(text)
            assert parsed["agents"][0]["endpoint"] == endpoint


def main() -> int:
    tests = [
        test_last_agent_does_not_rewrite_following_tables,
        test_nested_registry_table_prevents_inline_registry_collision,
        test_updating_first_agent_preserves_second_agent_and_routing,
        test_replacement_values_with_backslashes_remain_valid_toml,
    ]
    for test in tests:
        test()
        print(f"ok {test.__name__}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
