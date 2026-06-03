from __future__ import annotations

import os
from pathlib import Path

def claude_workspace(value: str | Path | None = None) -> Path:
    raw = value or os.environ.get("AGENTCALL_CLAUDE_WORKSPACE") or os.getcwd()
    return Path(raw).resolve()


def is_claude_command(command: list[str]) -> bool:
    if not command:
        return False
    executable = Path(command[0]).name.lower()
    return executable in {"claude", "claude.exe"}
