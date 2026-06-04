from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class RouteRecommendation:
    recommended_runtime: str
    reason: str
    required_context: list[str] = field(default_factory=list)
    expected_output: str = "CheckpointReport"
    checkpoint_policy: str | None = None
    confidence: float = 0.8
    alternatives: list[dict[str, Any]] = field(default_factory=list)
    limits: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        payload = {
            "recommended_runtime": self.recommended_runtime,
            "reason": self.reason,
            "required_context": self.required_context,
            "expected_output": self.expected_output,
            "confidence": self.confidence,
            "alternatives": self.alternatives,
            "limits": self.limits,
        }
        if self.checkpoint_policy:
            payload["checkpoint_policy"] = self.checkpoint_policy
        return payload


def route_task(
    objective: str,
    *,
    task_type: str | None = None,
    estimated_files: int | None = None,
    needs_continuity: bool = False,
    risk: str | None = None,
    phase: str | None = None,
    expected_minutes: int | None = None,
    parallel_children: int | None = None,
) -> RouteRecommendation:
    _ = (objective, task_type, estimated_files, needs_continuity, risk, phase, expected_minutes, parallel_children)
    return RouteRecommendation(
        recommended_runtime="claude-code-session",
        reason="AgentCall v3 routes all delegated work to a daemon-owned Claude Code PTY utility worker.",
        required_context=["objective", "workspace", "allowed_paths", "acceptance_criteria"],
        expected_output="CheckpointReport",
        checkpoint_policy="use board/session summary; request a report when the PTY worker reaches a stable stopping point",
        confidence=0.9,
        alternatives=[],
        limits={"parent_must_validate_report": True, "use_plan_mode_for_unclear_or_high_risk_work": True},
    )
