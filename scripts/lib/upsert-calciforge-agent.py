#!/usr/bin/env python3
"""Idempotently add or update a managed Calciforge agent block.

The installer preserves operator edits, so this helper intentionally edits TOML
text instead of re-serializing the whole config. It only mutates top-level keys
inside the matching ``[[agents]]`` block and leaves nested agent tables intact.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re


AGENT_TABLE_RE = re.compile(r"^\s*\[\[agents\]\]\s*$")
HEADER_RE = re.compile(r"^\s*\[(.+)\]\s*$")


def q(value: str) -> str:
    return json.dumps(value)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("config")
    parser.add_argument("agent_id")
    parser.add_argument("kind")
    parser.add_argument("endpoint")
    parser.add_argument("timeout_ms", type=int)
    parser.add_argument("aliases_csv")
    parser.add_argument("api_key_file")
    parser.add_argument("allow_model_override")
    parser.add_argument("model")
    return parser.parse_args()


def header_kind(line: str) -> str | None:
    match = HEADER_RE.match(line)
    if not match:
        return None
    name = match.group(1).strip()
    if name == "[agents]":
        return "agents"
    if name.startswith("[agents."):
        return "agents_child"
    if name.startswith("agents."):
        return "agents_child"
    if name == "agents":
        return "agents"
    return "other"


def agent_block_end(lines: list[str], start: int) -> int:
    for index in range(start + 1, len(lines)):
        kind = header_kind(lines[index])
        if kind == "agents" or kind == "other":
            return index
    return len(lines)


def agent_top_end(lines: list[str], start: int, end: int) -> int:
    for index in range(start + 1, end):
        if header_kind(lines[index]) == "agents_child":
            return index
    return end


def attached_comment_start(lines: list[str], table_start: int) -> int:
    start = table_start
    index = table_start - 1
    while index >= 0 and (lines[index].startswith("#") or not lines[index].strip()):
        if lines[index].startswith("#"):
            start = index
        index -= 1
    if start < table_start and "Managed by calciforge install for" in "\n".join(
        lines[start:table_start]
    ):
        return start
    return table_start


def find_existing_agent(lines: list[str], agent_id: str) -> tuple[int, int, int, int] | None:
    for table_start, line in enumerate(lines):
        if not AGENT_TABLE_RE.match(line):
            continue
        block_end = agent_block_end(lines, table_start)
        top_end = agent_top_end(lines, table_start, block_end)
        top = "\n".join(lines[table_start:top_end])
        found_id = re.search(r"(?m)^\s*id\s*=\s*[\"']([^\"']+)[\"']", top)
        if found_id and found_id.group(1) == agent_id:
            return attached_comment_start(lines, table_start), table_start, top_end, block_end
    return None


def has_key(lines: list[str], key: str) -> bool:
    return any(re.match(rf"^\s*{re.escape(key)}\s*=", line) for line in lines)


def upsert_key(lines: list[str], key: str, value: str, overwrite: bool = True) -> list[str]:
    for index, line in enumerate(lines):
        if re.match(rf"^\s*{re.escape(key)}\s*=", line):
            if overwrite:
                lines[index] = re.sub(
                    rf"^(\s*{re.escape(key)}\s*=\s*).*$",
                    rf"\g<1>{value}",
                    line,
                )
            return lines
    insert_at = len(lines)
    while insert_at > 0 and not lines[insert_at - 1].strip():
        insert_at -= 1
    return lines[:insert_at] + [f"{key} = {value}"] + lines[insert_at:]


def registry_exists(top_lines: list[str], child_lines: list[str]) -> bool:
    if has_key(top_lines, "registry"):
        return True
    return any(re.match(r"^\s*\[\[?agents\.registry", line) for line in child_lines)


def managed_block(
    agent_id: str,
    kind: str,
    endpoint: str,
    timeout_ms: int,
    aliases: list[str],
    api_key_file: str,
    allow_model_override: str,
    model: str,
) -> list[str]:
    display_name = {
        "hermes": "Hermes",
        "ironclaw": "IronClaw",
    }.get(agent_id, agent_id)
    lines = [
        f"# Managed by calciforge install for {agent_id}.",
        "[[agents]]",
        f"id = {q(agent_id)}",
        f"kind = {q(kind)}",
        f"endpoint = {q(endpoint)}",
        f"api_key_file = {q(api_key_file)}",
    ]
    if model:
        lines.append(f"model = {q(model)}")
    lines.append(f"timeout_ms = {int(timeout_ms)}")
    if allow_model_override.lower() == "true":
        lines.append("allow_model_override = true")
    if aliases:
        lines.append(f"aliases = {q(aliases)}")
    lines.append(
        "registry = { "
        f"display_name = {q(display_name)}, "
        f"specialties = {q([kind, 'managed'])} "
        "}"
    )
    return lines


def upsert_agent(config_path: pathlib.Path, args: argparse.Namespace) -> str:
    if not config_path.exists() or not config_path.read_text().strip():
        config_path.write_text("[calciforge]\nversion = 2\n")

    text = config_path.read_text()
    if not text.endswith("\n"):
        text += "\n"
    lines = text.splitlines()
    aliases = [item.strip() for item in args.aliases_csv.split(",") if item.strip()]
    block = managed_block(
        args.agent_id,
        args.kind,
        args.endpoint,
        args.timeout_ms,
        aliases,
        args.api_key_file,
        args.allow_model_override,
        args.model,
    )

    existing = find_existing_agent(lines, args.agent_id)
    if existing is None:
        new_text = "\n".join(lines).rstrip() + "\n\n" + "\n".join(block) + "\n"
        config_path.write_text(new_text)
        return "added"

    start, table_start, top_end, block_end = existing
    comment_lines = lines[start:table_start]
    top_lines = lines[table_start:top_end]
    child_lines = lines[top_end:block_end]
    if not any("Managed by calciforge install for" in line for line in comment_lines):
        comment_lines = [f"# Managed by calciforge install for {args.agent_id}."]

    top_lines = upsert_key(top_lines, "kind", q(args.kind))
    top_lines = upsert_key(top_lines, "endpoint", q(args.endpoint))
    top_lines = upsert_key(top_lines, "api_key_file", q(args.api_key_file))
    top_lines = upsert_key(top_lines, "timeout_ms", str(int(args.timeout_ms)))
    if args.model:
        top_lines = upsert_key(top_lines, "model", q(args.model), overwrite=False)
    if args.allow_model_override.lower() == "true":
        top_lines = upsert_key(top_lines, "allow_model_override", "true")
    if aliases:
        top_lines = upsert_key(top_lines, "aliases", q(aliases), overwrite=False)
    if not registry_exists(top_lines, child_lines):
        top_lines.append(block[-1])

    replacement = comment_lines + top_lines + child_lines
    new_lines = lines[:start]
    if new_lines and new_lines[-1].strip():
        new_lines.append("")
    new_lines.extend(replacement)
    if block_end < len(lines) and lines[block_end].strip():
        new_lines.append("")
    new_lines.extend(lines[block_end:])
    config_path.write_text("\n".join(new_lines).rstrip() + "\n")
    return "updated"


def main() -> int:
    args = parse_args()
    config_path = pathlib.Path(args.config).expanduser()
    config_path.parent.mkdir(parents=True, exist_ok=True)
    action = upsert_agent(config_path, args)
    print(f"{action} calciforge agent {args.agent_id!r} in {config_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
