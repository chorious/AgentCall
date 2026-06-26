from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path


HEADER = """---
name: agentcall-supervisor
description: Use when Codex supervises Claude Code workers through AgentCall MCP; enforces projection-first reading, idempotent session control, permission-menu discipline, and evidence-based report acceptance.
---

# AgentCall Supervisor

"""


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate repository-managed AgentCall Codex skills.")
    parser.add_argument("--root", default=Path(__file__).resolve().parents[1], type=Path)
    parser.add_argument("--check", action="store_true", help="Fail if generated files are not up to date.")
    args = parser.parse_args()
    root = args.root.resolve()

    stale = []
    stale.extend(sync_supervisor_skill(root, check=args.check))
    stale.extend(sync_flow_skill(root, check=args.check))
    if args.check:
        if stale:
            print("AgentCall skills are stale:", file=sys.stderr)
            for item in stale:
                print(f"- {item}", file=sys.stderr)
            print("Run: python scripts/generate_agentcall_skill.py", file=sys.stderr)
            return 1
        print("[OK] AgentCall supervisor and flow skills are up to date")
        return 0
    return 0


def sync_supervisor_skill(root: Path, check: bool) -> list[str]:
    protocol_path = root / "docs" / "agentcall-protocol.md"
    skill_path = root / ".codex" / "skills" / "agentcall-supervisor" / "SKILL.md"
    docs_skill_path = root / "docs" / "agentcall-supervisor-skill.md"
    protocol = protocol_path.read_text(encoding="utf-8")
    body = protocol.split("\n", 1)[1] if protocol.startswith("# ") else protocol
    generated = HEADER + body.strip() + "\n"
    return sync_text_targets(root, generated, [skill_path, docs_skill_path], check)


def sync_flow_skill(root: Path, check: bool) -> list[str]:
    source_dir = root / "docs" / "agentcall-flow"
    target_dir = root / ".codex" / "skills" / "agentcall-flow"
    stale = sync_tree(root, source_dir, target_dir, check)
    skill_text = (source_dir / "SKILL.md").read_text(encoding="utf-8")
    stale.extend(sync_text_targets(root, skill_text, [root / "docs" / "agentcall-flow-skill.md"], check))
    return stale


def sync_text_targets(root: Path, text: str, targets: list[Path], check: bool) -> list[str]:
    stale = []
    for target in targets:
        if not target.exists() or target.read_text(encoding="utf-8") != text:
            stale.append(str(target.relative_to(root)))
            if not check:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(text, encoding="utf-8", newline="\n")
                print(f"[OK] wrote {target.relative_to(root)}")
    return stale


def sync_tree(root: Path, source_dir: Path, target_dir: Path, check: bool) -> list[str]:
    stale = []
    expected = {
        path.relative_to(source_dir)
        for path in source_dir.rglob("*")
        if path.is_file()
    }
    existing = {
        path.relative_to(target_dir)
        for path in target_dir.rglob("*")
        if path.is_file()
    } if target_dir.exists() else set()
    for rel_path in sorted(expected):
        source = source_dir / rel_path
        target = target_dir / rel_path
        if not target.exists() or source.read_bytes() != target.read_bytes():
            stale.append(str(target.relative_to(root)))
            if not check:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
                print(f"[OK] wrote {target.relative_to(root)}")
    for rel_path in sorted(existing - expected):
        target = target_dir / rel_path
        stale.append(str(target.relative_to(root)))
        if not check:
            target.unlink()
            print(f"[OK] removed {target.relative_to(root)}")
    return stale


if __name__ == "__main__":
    raise SystemExit(main())
