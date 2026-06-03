from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
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
