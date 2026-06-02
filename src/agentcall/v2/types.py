from __future__ import annotations

import json
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
        context_packet = self.context.get("context_packet") if self.context else None
        context_section = ""
        if context_packet:
            context_section = (
                "## Context Packet\n\n"
                "Use this packet as the authoritative project context for this lifecycle.\n\n"
                "```json\n"
                f"{json.dumps(context_packet, ensure_ascii=False, indent=2)}\n"
                "```\n\n"
            )
        mode_rules = ""
        if self.mode == ChildMode.PLAN:
            mode_rules = (
                "## Mode Rules\n\n"
                "- This is PLAN mode.\n"
                "- Do not modify files.\n"
                "- Do not run commands that change files.\n"
                "- In the report, `changed_files` must be an empty array.\n"
                "- `commands_run` and `tests` must describe only checks actually performed.\n"
                "- If you need execution, set `next_recommended_action` to `execute approved plan`.\n\n"
            )
        elif self.mode == ChildMode.EXECUTE:
            mode_rules = (
                "## Mode Rules\n\n"
                "- This is EXECUTE mode.\n"
                "- Make only the scoped changes needed to satisfy the acceptance criteria.\n"
                "- `changed_files` must list only files actually changed in this lifecycle.\n\n"
            )
        elif self.mode == ChildMode.REVIEW:
            mode_rules = (
                "## Mode Rules\n\n"
                "- This is REVIEW mode.\n"
                "- Do not modify files.\n"
                "- Return findings only when revision or blocker handling is needed.\n\n"
            )
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
            f"{context_section}"
            f"{mode_rules}"
            "## Required Report Contract\n\n"
            "Return exactly one structured report with status, summary, changed_files, "
            "commands_run, tests, risks, open_questions, and next_recommended_action. "
            "Include context_sufficiency with status, missing, can_parent_resolve, "
            "and recommended_parent_action. "
            "Do not continue beyond this lifecycle.\n"
        )
