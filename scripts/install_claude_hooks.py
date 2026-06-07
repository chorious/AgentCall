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
    ("PostToolBatch", None),
    ("Notification", None),
    ("Stop", None),
    ("SubagentStop", None),
    ("PreCompact", None),
    ("SessionEnd", None),
]


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".")
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument(
        "--settings-root",
        "--claude-workspace",
        dest="settings_root",
        default=None,
        help="Claude Code cwd whose .claude/settings.local.json should receive hooks. Defaults to config/agentcall.local.json claude_workspace.",
    )
    parser.add_argument("--scope", choices=["project-local", "project", "user"], default="project-local")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    root = Path(args.root).resolve()
    hook_script = root / "scripts" / "agentcall-claude-hook.py"
    if not hook_script.exists():
        raise SystemExit(f"Hook script not found: {hook_script}")

    settings_root = resolve_settings_root(root, args.settings_root)

    if args.scope == "user":
        settings_path = Path.home() / ".claude" / "settings.json"
    elif args.scope == "project":
        settings_path = settings_root / ".claude" / "settings.json"
    else:
        settings_path = settings_root / ".claude" / "settings.local.json"

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
        hooks[event] = merge_agentcall_hook_entries(hooks.get(event), entry)

    text = json.dumps(settings, ensure_ascii=False, indent=2) + "\n"
    if args.dry_run:
        print(text, end="")
        return 0

    settings_path.parent.mkdir(parents=True, exist_ok=True)
    settings_path.write_text(text, encoding="utf-8")
    print(f"Installed AgentCall Claude Code hooks: {settings_path}")
    print(f"AgentCall root: {root}")
    print(f"Claude settings root: {settings_root}")
    print("Use /hooks inside Claude Code to inspect the loaded project/local hooks.")
    return 0


def resolve_settings_root(root: Path, explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    config_path = root / "config" / "agentcall.local.json"
    if not config_path.exists():
        raise SystemExit(
            "Missing config/agentcall.local.json. Copy config/agentcall.example.json and set claude_workspace, "
            "or pass --settings-root explicitly."
        )
    config = read_json(config_path)
    claude_workspace = config.get("claude_workspace")
    if not isinstance(claude_workspace, str) or not claude_workspace.strip():
        raise SystemExit(
            "Missing claude_workspace in config/agentcall.local.json. "
            "AgentCall writes Claude hooks to claude_workspace/.claude/settings.local.json."
        )
    return Path(claude_workspace).resolve()


def merge_agentcall_hook_entries(existing, agentcall_entry: dict) -> list:
    entries = existing if isinstance(existing, list) else []
    kept = [entry for entry in entries if not is_agentcall_hook_entry(entry)]
    kept.append(agentcall_entry)
    return kept


def is_agentcall_hook_entry(entry) -> bool:
    if not isinstance(entry, dict):
        return False
    hooks = entry.get("hooks")
    if not isinstance(hooks, list):
        return False
    return any(is_agentcall_hook_command(hook) for hook in hooks)


def is_agentcall_hook_command(hook) -> bool:
    if not isinstance(hook, dict):
        return False
    command = hook.get("command")
    args = hook.get("args")
    haystack = " ".join(
        part
        for part in [
            command if isinstance(command, str) else "",
            " ".join(str(arg) for arg in args) if isinstance(args, list) else "",
        ]
        if part
    )
    return "agentcall-claude-hook.py" in haystack


def read_json(path: Path) -> dict:
    if not path.exists():
        return {}
    raw = path.read_text(encoding="utf-8")
    if not raw.strip():
        return {}
    return json.loads(raw)

def configure_stdio() -> None:
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    raise SystemExit(main())
