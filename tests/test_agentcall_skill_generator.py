from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_agentcall_supervisor_skill_is_generated_and_contains_action_matrix() -> None:
    result = subprocess.run(
        [sys.executable, "scripts/generate_agentcall_skill.py", "--check"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr

    skill = (ROOT / ".codex/skills/agentcall-supervisor/SKILL.md").read_text(encoding="utf-8")
    assert "agentcall_board" in skill
    assert "next_recommended_action" in skill or "Action Matrix" in skill
    assert "idempotency_key" in skill
    assert "select_option" in skill
    assert "Do not default to raw terminal" in skill
    assert "confidence.band=low" in skill
