from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


EVENTS = [
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", "*"),
    ("PostToolUse", "*"),
    ("Notification", None),
    ("Stop", None),
    ("SubagentStop", None),
    ("PreCompact", None),
    ("SessionEnd", None),
]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--scope", choices=["project-local", "project", "user"], default="project-local")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    hook_script = root / "scripts" / "agentcall-claude-hook.py"
    if not hook_script.exists():
        raise SystemExit(f"Hook script not found: {hook_script}")

    if args.scope == "user":
        settings_path = Path.home() / ".claude" / "settings.json"
    elif args.scope == "project":
        settings_path = root / ".claude" / "settings.json"
    else:
        settings_path = root / ".claude" / "settings.local.json"

    settings = read_json(settings_path)
    hooks = settings.setdefault("hooks", {})
    for event, matcher in EVENTS:
        entry = {
            "hooks": [
                {
                    "type": "command",
                    "command": args.python,
                    "args": [
                        str(hook_script),
                        "--root",
                        str(root),
                        "--event",
                        event,
                    ],
                    "timeout": 30,
                }
            ]
        }
        if matcher:
            entry["matcher"] = matcher
        hooks[event] = [entry]

    text = json.dumps(settings, ensure_ascii=False, indent=2) + "\n"
    if args.dry_run:
        print(text, end="")
        return 0

    settings_path.parent.mkdir(parents=True, exist_ok=True)
    settings_path.write_text(text, encoding="utf-8")
    print(f"Installed AgentCall Claude Code hooks: {settings_path}")
    print("Use /hooks inside Claude Code to inspect the loaded project/local hooks.")
    return 0


def read_json(path: Path) -> dict:
    if not path.exists():
        return {}
    raw = path.read_text(encoding="utf-8")
    if not raw.strip():
        return {}
    return json.loads(raw)


if __name__ == "__main__":
    raise SystemExit(main())
