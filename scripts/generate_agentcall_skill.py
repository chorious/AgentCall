from __future__ import annotations

import argparse
import sys
from pathlib import Path


HEADER = """---
name: agentcall-supervisor
description: Use when Codex supervises Claude Code workers through AgentCall MCP; enforces projection-first reading, idempotent session control, permission-menu discipline, and evidence-based report acceptance.
---

# AgentCall Supervisor

"""


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate AgentCall Codex supervisor skill from docs/agentcall-protocol.md.")
    parser.add_argument("--root", default=Path(__file__).resolve().parents[1], type=Path)
    parser.add_argument("--check", action="store_true", help="Fail if generated files are not up to date.")
    args = parser.parse_args()
    root = args.root.resolve()
    protocol_path = root / "docs" / "agentcall-protocol.md"
    skill_path = root / ".codex" / "skills" / "agentcall-supervisor" / "SKILL.md"
    docs_skill_path = root / "docs" / "agentcall-supervisor-skill.md"

    protocol = protocol_path.read_text(encoding="utf-8")
    body = protocol.split("\n", 1)[1] if protocol.startswith("# ") else protocol
    generated = HEADER + body.strip() + "\n"

    targets = [skill_path, docs_skill_path]
    if args.check:
        stale = []
        for target in targets:
            if not target.exists() or target.read_text(encoding="utf-8") != generated:
                stale.append(str(target.relative_to(root)))
        if stale:
            print("AgentCall supervisor skill is stale:", file=sys.stderr)
            for item in stale:
                print(f"- {item}", file=sys.stderr)
            print("Run: python scripts/generate_agentcall_skill.py", file=sys.stderr)
            return 1
        print("[OK] AgentCall supervisor skill is up to date")
        return 0

    for target in targets:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(generated, encoding="utf-8", newline="\n")
        print(f"[OK] wrote {target.relative_to(root)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
