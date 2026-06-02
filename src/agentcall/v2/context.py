from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from .types import ChildCallSpec


@dataclass
class ContextSufficiency:
    status: str = "enough_to_act"
    missing: list[str] = field(default_factory=list)
    can_parent_resolve: bool = True
    recommended_parent_action: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "status": self.status,
            "missing": self.missing,
            "can_parent_resolve": self.can_parent_resolve,
            "recommended_parent_action": self.recommended_parent_action,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> "ContextSufficiency":
        data = data or {}
        return cls(
            status=str(data.get("status", "enough_to_act")),
            missing=[str(item) for item in data.get("missing", [])],
            can_parent_resolve=bool(data.get("can_parent_resolve", True)),
            recommended_parent_action=str(data.get("recommended_parent_action", "")),
        )


@dataclass
class ContextPacket:
    task_id: str
    call_id: str
    phase: str
    role: str
    runtime: str
    objective: str
    workspace: str
    allowed_paths: list[str] = field(default_factory=list)
    acceptance_criteria: list[str] = field(default_factory=list)
    relevant_files: list[str] = field(default_factory=list)
    prior_reports: list[dict[str, Any]] = field(default_factory=list)
    decisions: list[str] = field(default_factory=list)
    risks: list[str] = field(default_factory=list)
    forbidden_actions: list[str] = field(default_factory=list)
    output_contract: str = "ChildReport"
    sufficiency: ContextSufficiency = field(default_factory=ContextSufficiency)
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_spec(
        cls,
        spec: ChildCallSpec,
        *,
        runtime: str,
        prior_reports: list[dict[str, Any]] | None = None,
    ) -> "ContextPacket":
        context = spec.context or {}
        return cls(
            task_id=spec.task_id,
            call_id=spec.call_id,
            phase=spec.mode.value,
            role=spec.role.value,
            runtime=runtime,
            objective=spec.objective,
            workspace=str(spec.workspace),
            allowed_paths=list(spec.allowed_paths),
            acceptance_criteria=list(spec.acceptance_criteria),
            relevant_files=[str(item) for item in context.get("relevant_files", [])],
            prior_reports=prior_reports or [dict(item) for item in context.get("prior_reports", [])],
            decisions=[str(item) for item in context.get("decisions", [])],
            risks=[str(item) for item in context.get("risks", [])],
            forbidden_actions=[str(item) for item in context.get("forbidden_actions", [])],
            output_contract=str(context.get("output_contract", "ChildReport")),
            sufficiency=ContextSufficiency.from_dict(context.get("sufficiency")),
            metadata=dict(context.get("metadata", {})),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "task_id": self.task_id,
            "call_id": self.call_id,
            "phase": self.phase,
            "role": self.role,
            "runtime": self.runtime,
            "objective": self.objective,
            "workspace": self.workspace,
            "allowed_paths": self.allowed_paths,
            "acceptance_criteria": self.acceptance_criteria,
            "relevant_files": self.relevant_files,
            "prior_reports": self.prior_reports,
            "decisions": self.decisions,
            "risks": self.risks,
            "forbidden_actions": self.forbidden_actions,
            "output_contract": self.output_contract,
            "sufficiency": self.sufficiency.to_dict(),
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ContextPacket":
        return cls(
            task_id=str(data["task_id"]),
            call_id=str(data["call_id"]),
            phase=str(data["phase"]),
            role=str(data["role"]),
            runtime=str(data["runtime"]),
            objective=str(data["objective"]),
            workspace=str(data["workspace"]),
            allowed_paths=[str(item) for item in data.get("allowed_paths", [])],
            acceptance_criteria=[str(item) for item in data.get("acceptance_criteria", [])],
            relevant_files=[str(item) for item in data.get("relevant_files", [])],
            prior_reports=[dict(item) for item in data.get("prior_reports", [])],
            decisions=[str(item) for item in data.get("decisions", [])],
            risks=[str(item) for item in data.get("risks", [])],
            forbidden_actions=[str(item) for item in data.get("forbidden_actions", [])],
            output_contract=str(data.get("output_contract", "ChildReport")),
            sufficiency=ContextSufficiency.from_dict(data.get("sufficiency")),
            metadata=dict(data.get("metadata", {})),
        )

    def to_prompt_section(self) -> str:
        payload = json.dumps(self.to_dict(), ensure_ascii=False, indent=2)
        return (
            "## Context Packet\n\n"
            "Use this packet as the authoritative project context for this lifecycle.\n\n"
            "```json\n"
            f"{payload}\n"
            "```\n\n"
        )
