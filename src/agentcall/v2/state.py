from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

from .reports import ChildReport, ReportStatus
from .types import ChildCallSpec


class AgentLifecycleState(StrEnum):
    STARTING = "starting"
    RUNNING = "running"
    REPORTED = "reported"
    NEEDS_REVIEW = "needs_review"
    ACCEPTED = "accepted"
    REJECTED = "rejected"


@dataclass
class AgentSnapshot:
    task_id: str
    call_id: str
    agent: str
    state: str
    mode: str | None = None
    role: str | None = None
    needs_user: str | None = None
    evidence: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "task_id": self.task_id,
            "call_id": self.call_id,
            "agent": self.agent,
            "state": self.state,
            "mode": self.mode,
            "role": self.role,
            "needs_user": self.needs_user,
            "evidence": self.evidence,
            "metadata": self.metadata,
        }


def snapshot_from_spec(spec: ChildCallSpec, *, agent: str, state: AgentLifecycleState) -> AgentSnapshot:
    return AgentSnapshot(
        task_id=spec.task_id,
        call_id=spec.call_id,
        agent=agent,
        state=state.value,
        mode=spec.mode.value,
        role=spec.role.value,
        evidence=[f"child lifecycle {spec.call_id} entered {state.value}"],
        metadata={"max_turns": spec.max_turns, "max_seconds": spec.max_seconds},
    )


def snapshot_from_report(report: ChildReport, *, mode: str | None = None, role: str | None = None) -> AgentSnapshot:
    state = AgentLifecycleState.REPORTED
    needs_user = None
    evidence = [f"report status={report.status}", f"turns_used={report.turns_used}"]
    if report.status != ReportStatus.DONE.value or report.risks or report.open_questions:
        state = AgentLifecycleState.NEEDS_REVIEW
        needs_user = "parent_validation"
        evidence.extend(report.risks)
        evidence.extend(report.open_questions)
    return AgentSnapshot(
        task_id=report.task_id,
        call_id=report.call_id,
        agent=report.agent,
        state=state.value,
        mode=mode,
        role=role,
        needs_user=needs_user,
        evidence=evidence,
        metadata={"changed_files": report.changed_files, "tests": report.tests},
    )

