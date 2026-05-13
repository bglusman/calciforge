#!/usr/bin/env python3
"""Validate the integration-boundary registry used for aggression testing."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "tests" / "boundaries" / "integration-surfaces.json"
SCENARIOS = ROOT / "tests" / "scenarios" / "high-risk-scenarios.json"

REQUIRED_FIELDS = {
    "id",
    "title",
    "category",
    "status",
    "source_paths",
    "invalid_containment",
    "valid_correctness",
    "automation",
    "scenario_ids",
}
ALLOWED_STATUS = {"automated", "partial", "manual", "missing"}

WATCHED_DIRS = [
    "crates/calciforge/src/adapters",
    "crates/calciforge/src/channels",
    "crates/calciforge/src/proxy",
    "crates/calciforge/src/config",
    "crates/calciforge/src/doctor",
    "crates/calciforge/src/hooks",
    "crates/calciforge/src/install",
    "crates/calciforge/src/local_model",
    "crates/calciforge/src/providers",
    "crates/calciforge/src/voice",
    "crates/adversary-detector/src",
    "crates/clashd/src",
    "crates/host-agent/src",
    "crates/mcp-server/src",
    "crates/paste-server/src",
    "crates/secrets-client/src",
    "crates/security-proxy/src",
]

WATCHED_FILES = [
    "crates/calciforge/src/auth.rs",
    "crates/calciforge/src/commands.rs",
    "crates/calciforge/src/config.rs",
    "crates/calciforge/src/context.rs",
    "crates/calciforge/src/model_names.rs",
    "crates/calciforge/src/persistent_context.rs",
    "crates/calciforge/src/router.rs",
    "crates/calciforge/src/unified_context.rs",
]

IGNORED_FILE_NAMES = {"lib.rs", "main.rs", "mod.rs", "tests.rs"}
IGNORED_SUFFIXES = ("_tests.rs",)


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def read_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        fail(f"{path.relative_to(ROOT)}: invalid JSON: {exc}")


def require_nonempty_string(entry_id: str, entry: dict, field: str) -> None:
    value = entry.get(field)
    if not isinstance(value, str) or not value.strip():
        fail(f"{entry_id}: {field} must be a non-empty string")


def require_nonempty_string_list(entry_id: str, entry: dict, field: str) -> list[str]:
    value = entry.get(field)
    if not isinstance(value, list) or not value:
        fail(f"{entry_id}: {field} must be a non-empty list")
    if not all(isinstance(item, str) and item.strip() for item in value):
        fail(f"{entry_id}: {field} must contain only non-empty strings")
    return value


def watched_source_files() -> set[str]:
    watched: set[str] = set()
    for root in WATCHED_DIRS:
        directory = ROOT / root
        if not directory.exists():
            fail(f"watched directory does not exist: {root}")
        for path in directory.rglob("*.rs"):
            if path.name in IGNORED_FILE_NAMES:
                continue
            if path.name.endswith(IGNORED_SUFFIXES):
                continue
            watched.add(path.relative_to(ROOT).as_posix())

    for file_name in WATCHED_FILES:
        if not (ROOT / file_name).exists():
            fail(f"watched file does not exist: {file_name}")
        watched.add(file_name)

    return watched


def main() -> None:
    registry = read_json(REGISTRY)
    if not isinstance(registry, list) or not registry:
        fail("boundary registry must be a non-empty JSON list")

    scenarios = read_json(SCENARIOS)
    if not isinstance(scenarios, list):
        fail("scenario catalog must be a JSON list")
    scenario_ids = {entry.get("id") for entry in scenarios if isinstance(entry, dict)}

    seen_ids: set[str] = set()
    source_owner: dict[str, str] = {}
    statuses: set[str] = set()

    for index, entry in enumerate(registry):
        if not isinstance(entry, dict):
            fail(f"boundary entry {index}: must be an object")

        missing = REQUIRED_FIELDS - set(entry)
        if missing:
            fail(f"{entry.get('id', index)}: missing fields: {', '.join(sorted(missing))}")

        entry_id = entry["id"]
        if not isinstance(entry_id, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", entry_id):
            fail(f"{entry_id!r}: id must be lower-kebab-case")
        if entry_id in seen_ids:
            fail(f"{entry_id}: duplicate boundary id")
        seen_ids.add(entry_id)

        for field in ("title", "category", "invalid_containment", "valid_correctness"):
            require_nonempty_string(entry_id, entry, field)

        status = entry["status"]
        if status not in ALLOWED_STATUS:
            fail(f"{entry_id}: status must be one of {', '.join(sorted(ALLOWED_STATUS))}")
        statuses.add(status)

        source_paths = require_nonempty_string_list(entry_id, entry, "source_paths")
        automation = require_nonempty_string_list(entry_id, entry, "automation")
        scenario_refs = require_nonempty_string_list(entry_id, entry, "scenario_ids")

        for source_path in source_paths:
            path = ROOT / source_path
            if not path.exists():
                fail(f"{entry_id}: source path does not exist: {source_path}")
            if source_path in source_owner:
                fail(
                    f"{source_path}: registered by both {source_owner[source_path]} and {entry_id}"
                )
            source_owner[source_path] = entry_id

        for command in automation:
            if command.startswith("scripts/"):
                script = command.split()[0]
                if not (ROOT / script).exists():
                    fail(f"{entry_id}: automation script does not exist: {script}")

        for scenario_id in scenario_refs:
            if scenario_id not in scenario_ids:
                fail(f"{entry_id}: scenario id is not in high-risk catalog: {scenario_id}")

    if "partial" not in statuses and "automated" not in statuses:
        fail("registry must include at least one partially or fully automated boundary")

    watched = watched_source_files()
    missing = sorted(watched - set(source_owner))
    if missing:
        fail(
            "integration source files missing boundary registration:\n"
            + "\n".join(f"  - {path}" for path in missing)
        )

    print(f"validated {len(registry)} integration boundaries covering {len(watched)} source files")


if __name__ == "__main__":
    main()
