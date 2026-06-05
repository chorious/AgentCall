from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

INLINE_LIMIT = 4096


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description="Rebuild hook-classified AgentCall logs from events.ndjson.")
    parser.add_argument("--root", default=".", help="AgentCall workspace root.")
    parser.add_argument("--apply", action="store_true", help="Write classified logs and PostToolUse artifacts.")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    agent_dir = root / ".agentcall"
    events_path = agent_dir / "events.ndjson"
    if not events_path.exists():
        print(f"missing events log: {events_path}", file=sys.stderr)
        return 2

    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    counts: Counter[str] = Counter()
    artifacts = 0
    bad_lines = 0
    for line in events_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            bad_lines += 1
            continue
        event_type = str(event.get("type", ""))
        if not event_type.startswith("hook."):
            continue
        hook_name = event_type.removeprefix("hook.")
        event, written = sanitize_post_tool_use(agent_dir, event, write=args.apply)
        artifacts += written
        counts[hook_name] += 1
        grouped[hook_name].append(event)

    print(f"events: {sum(counts.values())}")
    print(f"bad_lines: {bad_lines}")
    print(f"posttooluse_artifacts: {artifacts}")
    for hook_name, count in counts.most_common():
        print(f"{hook_name}: {count}")

    if not args.apply:
        print("dry-run only; pass --apply to rewrite .agentcall/logs/hooks/*.ndjson")
        return 0

    hooks_dir = agent_dir / "logs" / "hooks"
    hooks_dir.mkdir(parents=True, exist_ok=True)
    for hook_name, events in grouped.items():
        path = hooks_dir / f"{hook_name}.ndjson"
        tmp = path.with_name(f".{path.name}.{Path.cwd().name}.{id(events)}.tmp")
        with tmp.open("w", encoding="utf-8", newline="\n") as file:
            for event in events:
                file.write(json.dumps(event, ensure_ascii=False, separators=(",", ":")) + "\n")
        tmp.replace(path)
    print(f"wrote hook logs: {hooks_dir}")
    return 0


def sanitize_post_tool_use(agent_dir: Path, event: dict[str, Any], *, write: bool) -> tuple[dict[str, Any], int]:
    if event.get("type") != "hook.PostToolUse":
        return event, 0
    event = json.loads(json.dumps(event, ensure_ascii=False))
    data = event.get("data")
    if not isinstance(data, dict):
        return event, 0
    raw = data.get("raw")
    if not isinstance(raw, dict):
        return event, 0
    tool_response = raw.get("tool_response")
    if not isinstance(tool_response, dict):
        return event, 0
    written = 0
    event_id = str(event.get("id") or "event")
    for field in ("stdout", "stderr"):
        original = tool_response.get(field)
        if not isinstance(original, str) or len(original.encode("utf-8")) <= INLINE_LIMIT:
            continue
        artifact_dir = agent_dir / "logs" / "artifacts" / "PostToolUse"
        artifact_path = artifact_dir / f"{event_id}-{field}.txt"
        if write:
            artifact_dir.mkdir(parents=True, exist_ok=True)
            artifact_path.write_text(original, encoding="utf-8")
        prefix = safe_prefix(original, INLINE_LIMIT)
        byte_len = len(original.encode("utf-8"))
        tool_response[field] = (
            f"{prefix}\n...[AgentCall truncated {byte_len} bytes; full output: {artifact_path}]"
        )
        tool_response[f"{field}_artifact"] = {
            "path": str(artifact_path),
            "original_bytes": byte_len,
            "hash": fnv1a_hex(original.encode("utf-8")),
            "truncated": True,
        }
        written += 1
    return event, written


def safe_prefix(text: str, max_bytes: int) -> str:
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    clipped = encoded[:max_bytes]
    while clipped:
        try:
            return clipped.decode("utf-8")
        except UnicodeDecodeError:
            clipped = clipped[:-1]
    return ""


def fnv1a_hex(data: bytes) -> str:
    value = 0xCBF29CE484222325
    for byte in data:
        value ^= byte
        value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


if __name__ == "__main__":
    raise SystemExit(main())
