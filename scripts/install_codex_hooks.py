from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


EVENTS = [
    ("SessionStart", "AgentCall: loading workspace state", "startup|resume"),
    ("UserPromptSubmit", "AgentCall: checking orchestration state", None),
    ("Stop", "AgentCall: recording stop state", None),
    ("PreCompact", "AgentCall: saving pre-compact state", None),
    ("PostCompact", "AgentCall: restoring orchestration hints", None),
]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    hook_script = root / "scripts" / "agentcall-codex-hook.py"
    if not hook_script.exists():
        raise SystemExit(f"Hook script not found: {hook_script}")

    hooks_path = root / ".codex" / "hooks.json"
    config = {"hooks": {}}
    for event, status_message, matcher in EVENTS:
        command = " ".join(
            [
                shell_token(args.python),
                shell_token(str(hook_script)),
                "--root",
                shell_token(str(root)),
                "--event",
                event,
            ]
        )
        entry = {
            "hooks": [
                {
                    "type": "command",
                    "command": command,
                    "commandWindows": command,
                    "timeout": 10,
                    "statusMessage": status_message,
                }
            ]
        }
        if matcher:
            entry["matcher"] = matcher
        config["hooks"][event] = [entry]

    text = json.dumps(config, ensure_ascii=False, indent=2) + "\n"
    if args.dry_run:
        print(text, end="")
        return 0

    hooks_path.parent.mkdir(parents=True, exist_ok=True)
    hooks_path.write_text(text, encoding="utf-8")
    print(f"Installed AgentCall Codex hooks: {hooks_path}")
    print("Open a new Codex session or use the app hook trust flow if prompted.")
    return 0


def shell_token(value: str) -> str:
    normalized = value.replace("\\", "/")
    if any(char.isspace() for char in normalized):
        return '"' + normalized.replace('"', '\\"') + '"'
    return normalized


if __name__ == "__main__":
    raise SystemExit(main())
