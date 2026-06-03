from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--event", default=None)
    args = parser.parse_args()

    root = Path(args.root).resolve()
    payload = read_payload()
    event = args.event or payload.get("hook_event_name") or payload.get("hookEventName")
    if not event:
        print("AgentCall Codex hook missing event name", file=sys.stderr)
        return 1

    result = ingest(root, event, payload)
    if event in {"SessionStart", "UserPromptSubmit"}:
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": result.get("context_injection", ""),
                    },
                    "systemMessage": f"AgentCall: {event}",
                },
                ensure_ascii=False,
            )
        )
    return 0


def read_payload() -> dict:
    text = sys.stdin.read()
    if not text.strip():
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        print(f"Invalid Codex hook JSON: {exc}", file=sys.stderr)
        return {}


def ingest(root: Path, event: str, payload: dict) -> dict:
    _ = root
    daemon_url = os.environ.get("AGENTCALL_DAEMON_URL", "http://127.0.0.1:3293").rstrip("/")
    request = urllib.request.Request(
        f"{daemon_url}/api/hooks/ingest",
        data=json.dumps({"event": event, "runtime": "codex", "payload": payload}, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            text = response.read().decode("utf-8")
    except (OSError, urllib.error.URLError, urllib.error.HTTPError) as exc:
        print(f"AgentCall daemon ingest failed: {exc}", file=sys.stderr)
        return {}

    try:
        result = json.loads(text)
    except json.JSONDecodeError as exc:
        print(f"Invalid AgentCall daemon hook ingest JSON: {exc}", file=sys.stderr)
        return {}
    if not isinstance(result, dict):
        print("Invalid AgentCall daemon hook ingest response: expected JSON object", file=sys.stderr)
        return {}
    return result


if __name__ == "__main__":
    raise SystemExit(main())
