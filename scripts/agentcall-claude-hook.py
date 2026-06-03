from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--event", default=None)
    parser.add_argument("--python", default=sys.executable)
    args = parser.parse_args()

    root = Path(args.root).resolve()
    payload = read_payload()
    event = args.event or payload.get("hook_event_name") or payload.get("hookEventName")
    if not event:
        print("AgentCall hook missing event name", file=sys.stderr)
        return 1

    result = ingest(root, event, payload, args.python)
    decision = result.get("decision") or {}
    if event in {"SessionStart", "UserPromptSubmit"} and result.get("context_injection"):
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
    text = sys.stdin.read()
    if not text.strip():
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        print(f"Invalid Claude Code hook JSON: {exc}", file=sys.stderr)
        return {}


def ingest(root: Path, event: str, payload: dict, python_bin: str) -> dict:
    daemon_result = ingest_via_daemon(event, payload, runtime=None)
    if daemon_result is not None:
        return daemon_result

    print("AgentCall daemon ingest unavailable; falling back to legacy Python hook ingest.", file=sys.stderr)
    return ingest_via_legacy_python(root, event, payload, python_bin)


def ingest_via_daemon(event: str, payload: dict, runtime: str | None) -> dict | None:
    daemon_url = os.environ.get("AGENTCALL_DAEMON_URL", "http://127.0.0.1:3293").rstrip("/")
    body = {"event": event, "payload": payload}
    if runtime:
        body["runtime"] = runtime

    request = urllib.request.Request(
        f"{daemon_url}/api/hooks/ingest",
        data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            text = response.read().decode("utf-8")
    except (OSError, urllib.error.URLError, urllib.error.HTTPError) as exc:
        print(f"AgentCall daemon ingest failed: {exc}", file=sys.stderr)
        return None

    try:
        result = json.loads(text)
    except json.JSONDecodeError as exc:
        print(f"Invalid AgentCall daemon hook ingest JSON: {exc}", file=sys.stderr)
        return None
    if not isinstance(result, dict):
        print("Invalid AgentCall daemon hook ingest response: expected JSON object", file=sys.stderr)
        return None
    return result


def ingest_via_legacy_python(root: Path, event: str, payload: dict, python_bin: str) -> dict:
    env = os.environ.copy()
    src = root / "src"
    env["PYTHONPATH"] = str(src) + os.pathsep + env.get("PYTHONPATH", "")
    process = subprocess.run(
        [
            python_bin,
            "-m",
            "agentcall",
            "--root",
            str(root),
            "hook",
            "ingest",
            str(event),
            "--payload-json",
            json.dumps(payload, ensure_ascii=False),
        ],
        cwd=root,
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
    )
    if process.returncode != 0:
        print(process.stderr.strip() or process.stdout.strip(), file=sys.stderr)
        return {}
    try:
        return json.loads(process.stdout)
    except json.JSONDecodeError as exc:
        print(f"Invalid AgentCall hook ingest JSON: {exc}", file=sys.stderr)
        return {}


if __name__ == "__main__":
    raise SystemExit(main())
