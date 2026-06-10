from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--event", default=None)
    args, _unknown = parser.parse_known_args()

    root = Path(args.root).resolve()
    payload = sanitize(read_payload())
    wrapper_session = os.environ.get("AGENTCALL_WRAPPER_SESSION")
    if wrapper_session:
        payload["wrapper_session"] = sanitize(wrapper_session)
    event = args.event or payload.get("hook_event_name") or payload.get("hookEventName")
    if not event:
        print("AgentCall hook missing event name", file=sys.stderr)
        return 1

    result = ingest(root, event, payload)
    decision = result.get("decision") or {}
    if event in {"SessionStart", "UserPromptSubmit", "PostToolBatch"} and result.get("context_injection"):
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": result["context_injection"],
                    }
                },
                ensure_ascii=False,
            )
        )
        return 0

    if event == "PreToolUse" and decision and not decision.get("allowed", True):
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": decision.get("reason", "AgentCall file claim conflict"),
                    }
                },
                ensure_ascii=False,
            )
        )
        return 0

    return 0


def read_payload() -> dict:
    text = sys.stdin.buffer.read().decode("utf-8-sig", errors="replace")
    if not text.strip():
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        print(f"Invalid Claude Code hook JSON: {exc}", file=sys.stderr)
        return {}


def ingest(root: Path, event: str, payload: dict) -> dict:
    daemon_url = os.environ.get("AGENTCALL_DAEMON_URL", "http://127.0.0.1:3293").rstrip("/")
    headers = {"Content-Type": "application/json"}
    token = daemon_token(root)
    if token:
        headers["X-AgentCall-Token"] = token
    request = urllib.request.Request(
        f"{daemon_url}/api/hooks/ingest",
        data=json.dumps({"event": event, "payload": sanitize(payload)}, ensure_ascii=False).encode("utf-8"),
        headers=headers,
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


def daemon_token(root: Path) -> str | None:
    token = os.environ.get("AGENTCALL_DAEMON_TOKEN")
    if token:
        return token
    config_path = root / "config" / "agentcall.local.json"
    try:
        value = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    token = value.get("daemon_token")
    return token if isinstance(token, str) and token.strip() else None


def sanitize(value):
    if isinstance(value, str):
        return value.encode("utf-8", errors="replace").decode("utf-8")
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if isinstance(value, dict):
        return {sanitize(key): sanitize(item) for key, item in value.items()}
    return value


def configure_stdio() -> None:
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    raise SystemExit(main())
