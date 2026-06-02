from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any


class ChildMode(StrEnum):
    PLAN = "plan"
    EXECUTE = "execute"
    REVIEW = "review"

    def agent_mode_id(self) -> str:
        if self == ChildMode.PLAN:
            return "plan"
        if self == ChildMode.EXECUTE:
            return "acceptEdits"
        if self == ChildMode.REVIEW:
            return "plan"
        return str(self)


class ChildRole(StrEnum):
    PLANNER = "planner"
    EXECUTOR = "executor"
    REVIEWER = "reviewer"


@dataclass(frozen=True)
class ChildCallSpec:
    """A bounded, single-lifecycle child-agent invocation."""

    task_id: str
    call_id: str
    role: ChildRole
    mode: ChildMode
    objective: str
    workspace: Path
    allowed_paths: tuple[str, ...] = ()
    acceptance_criteria: tuple[str, ...] = ()
    max_turns: int = 1
    max_seconds: int = 300
    budget_usd: float | None = None
    context: dict[str, Any] = field(default_factory=dict)

    def to_prompt(self) -> str:
        allowed = "\n".join(f"- {item}" for item in self.allowed_paths) or "- Entire workspace"
        criteria = "\n".join(f"- {item}" for item in self.acceptance_criteria) or "- Produce a valid report"
        return (
            f"# AgentCall Child Invocation: {self.call_id}\n\n"
            f"Task: `{self.task_id}`\n"
            f"Role: `{self.role}`\n"
            f"Mode: `{self.mode}`\n"
            f"Max turns: `{self.max_turns}`\n"
            f"Max seconds: `{self.max_seconds}`\n\n"
            "## Objective\n\n"
            f"{self.objective}\n\n"
            "## Allowed Paths\n\n"
            f"{allowed}\n\n"
            "## Acceptance Criteria\n\n"
            f"{criteria}\n\n"
            "## Required Report Contract\n\n"
            "Return exactly one structured report with status, summary, changed_files, "
            "commands_run, tests, risks, open_questions, and next_recommended_action. "
            "Do not continue beyond this lifecycle.\n"
        )
