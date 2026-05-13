#!/usr/bin/env python3
"""Validate the high-risk scenario catalog used for aggression testing."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "tests" / "scenarios" / "high-risk-scenarios.json"
REQUIRED_FIELDS = {
    "id",
    "title",
    "user_action",
    "components",
    "promise",
    "aggression_vectors",
    "current_automation",
    "status",
}
ALLOWED_STATUS = {"automated", "partial", "manual", "missing"}


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def require_nonempty_list(scenario: dict, key: str) -> None:
    value = scenario.get(key)
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(item, str) and item.strip() for item in value)
    ):
        fail(f"{scenario.get('id', '<unknown>')}: {key} must be a non-empty list of strings")


def main() -> None:
    scenarios = json.loads(CATALOG.read_text(encoding="utf-8"))
    if not isinstance(scenarios, list) or not scenarios:
        fail("scenario catalog must be a non-empty JSON list")

    seen_ids: set[str] = set()
    statuses: set[str] = set()
    for index, scenario in enumerate(scenarios):
        if not isinstance(scenario, dict):
            fail(f"scenario {index}: must be an object")
        missing = REQUIRED_FIELDS - set(scenario)
        if missing:
            fail(f"{scenario.get('id', index)}: missing fields: {', '.join(sorted(missing))}")
        scenario_id = scenario["id"]
        if not isinstance(scenario_id, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", scenario_id):
            fail(f"{scenario_id!r}: id must be lower-kebab-case")
        if scenario_id in seen_ids:
            fail(f"{scenario_id}: duplicate scenario id")
        seen_ids.add(scenario_id)

        for key in ("title", "user_action", "promise"):
            if not isinstance(scenario[key], str) or not scenario[key].strip():
                fail(f"{scenario_id}: {key} must be a non-empty string")
        for key in ("components", "aggression_vectors", "current_automation"):
            require_nonempty_list(scenario, key)

        status = scenario["status"]
        if status not in ALLOWED_STATUS:
            fail(f"{scenario_id}: status must be one of {', '.join(sorted(ALLOWED_STATUS))}")
        statuses.add(status)

    if "automated" not in statuses:
        fail("catalog must include at least one automated scenario")
    if len(scenarios) < 5:
        fail("catalog must include at least five high-risk scenarios")

    print(f"validated {len(scenarios)} high-risk scenarios")


if __name__ == "__main__":
    main()
