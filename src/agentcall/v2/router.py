from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


HANDOFF_HINTS = {
    "large",
    "big",
    "migration",
    "explore",
    "exploratory",
    "debug",
    "iterative",
    "refactor",
    "architecture",
    "spike",
    "long",
    "大",
    "迁移",
    "探索",
    "调试",
    "重构",
    "架构",
    "长期",
}

ACP_HINTS = {
    "review",
    "inspect",
    "summarize",
    "classify",
    "small",
    "bug",
    "test",
    "diff",
    "检查",
    "总结",
    "分类",
    "小",
    "测试",
    "补丁",
}


@dataclass
class RouteRecommendation:
    recommended_runtime: str
    reason: str
    required_context: list[str] = field(default_factory=list)
    expected_output: str = "ChildReport"
    checkpoint_policy: str | None = None
    confidence: float = 0.7
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
    text = f"{objective} {task_type or ''}".lower()
    handoff_score = sum(1 for hint in HANDOFF_HINTS if hint in text)
    acp_score = sum(1 for hint in ACP_HINTS if hint in text)
    if estimated_files is not None and estimated_files >= 6:
        handoff_score += 2
    if estimated_files is not None and estimated_files <= 2:
        acp_score += 1
    if needs_continuity:
        handoff_score += 3
    if expected_minutes is not None and expected_minutes >= 20:
        handoff_score += 2
    if parallel_children is not None and parallel_children >= 2:
        acp_score += 2
    if (phase or "").lower() in {"review", "audit", "summarize", "plan"}:
        acp_score += 1
    if (risk or "").lower() in {"high", "critical"}:
        acp_score += 1

    if handoff_score > acp_score:
        confidence = min(0.95, 0.55 + 0.1 * (handoff_score - acp_score))
        return RouteRecommendation(
            recommended_runtime="claude-code-session",
            reason="任务看起来需要连续上下文、探索或较大实现切片，适合 handoff session。",
            required_context=["objective", "project_state", "allowed_paths", "checkpoint_policy"],
            expected_output="CheckpointReport",
            checkpoint_policy="on stop, on idle, and after file edits",
            confidence=confidence,
            alternatives=[
                {
                    "runtime": "acp",
                    "when": "先拆成 repo-scout / plan-critic / diff-reviewer 等 bounded specialist。",
                }
            ],
            limits={"parent_must_validate_checkpoints": True},
        )

    confidence = min(0.95, 0.55 + 0.1 * max(1, acp_score - handoff_score))
    return RouteRecommendation(
        recommended_runtime="acp",
        reason="任务边界较清楚，适合一次 bounded specialist 调用并返回 ChildReport。",
        required_context=["objective", "allowed_paths", "acceptance_criteria"],
        expected_output="ChildReport",
        confidence=confidence,
        alternatives=[
            {
                "runtime": "claude-code-session",
                "when": "如果执行中发现需要连续调试、跨多文件迁移或长期上下文，再升级为 handoff。",
            }
        ],
        limits={"max_turns": 1, "parent_keeps_final_control": True},
    )
