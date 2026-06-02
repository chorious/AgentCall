from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any


class ReportStatus(StrEnum):
    DONE = "done"
    BLOCKED = "blocked"
    FAILED = "failed"


REPORT_JSON_SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "task_id": {"type": "string"},
        "call_id": {"type": "string"},
        "agent": {"type": "string"},
        "status": {"type": "string", "enum": [item.value for item in ReportStatus]},
        "summary": {"type": "string"},
        "changed_files": {"type": "array", "items": {"type": "string"}},
        "commands_run": {"type": "array", "items": {"type": "string"}},
        "tests": {"type": "array", "items": {"type": "string"}},
        "risks": {"type": "array", "items": {"type": "string"}},
        "open_questions": {"type": "array", "items": {"type": "string"}},
        "next_recommended_action": {"type": "string"},
        "context_sufficiency": {
            "type": "object",
            "properties": {
                "status": {"type": "string"},
                "missing": {"type": "array", "items": {"type": "string"}},
                "can_parent_resolve": {"type": "boolean"},
                "recommended_parent_action": {"type": "string"},
            },
        },
        "turns_used": {"type": "integer", "minimum": 1},
        "metadata": {"type": "object"},
    },
    "required": [
        "task_id",
        "call_id",
        "agent",
        "status",
        "summary",
        "changed_files",
        "commands_run",
        "tests",
        "risks",
        "open_questions",
        "next_recommended_action",
        "context_sufficiency",
        "turns_used",
        "metadata",
    ],
    "additionalProperties": False,
}


def report_schema_text() -> str:
    return json.dumps(REPORT_JSON_SCHEMA, ensure_ascii=False, indent=2)


@dataclass
class ReportValidation:
    ok: bool
    findings: list[str] = field(default_factory=list)

    def require_ok(self) -> None:
        if not self.ok:
            raise ValueError("; ".join(self.findings))


@dataclass
class ChildReport:
    task_id: str
    call_id: str
    agent: str
    status: str
    summary: str
    changed_files: list[str] = field(default_factory=list)
    commands_run: list[str] = field(default_factory=list)
    tests: list[str] = field(default_factory=list)
    risks: list[str] = field(default_factory=list)
    open_questions: list[str] = field(default_factory=list)
    next_recommended_action: str = ""
    context_sufficiency: dict[str, Any] = field(
        default_factory=lambda: {
            "status": "enough_to_act",
            "missing": [],
            "can_parent_resolve": True,
            "recommended_parent_action": "",
        }
    )
    turns_used: int = 1
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "task_id": self.task_id,
            "call_id": self.call_id,
            "agent": self.agent,
            "status": self.status,
            "summary": self.summary,
            "changed_files": self.changed_files,
            "commands_run": self.commands_run,
            "tests": self.tests,
            "risks": self.risks,
            "open_questions": self.open_questions,
            "next_recommended_action": self.next_recommended_action,
            "context_sufficiency": self.context_sufficiency,
            "turns_used": self.turns_used,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ChildReport":
        return cls(
            task_id=str(data["task_id"]),
            call_id=str(data["call_id"]),
            agent=str(data["agent"]),
            status=str(data["status"]),
            summary=str(data.get("summary", "")),
            changed_files=[str(item) for item in data.get("changed_files", [])],
            commands_run=[str(item) for item in data.get("commands_run", [])],
            tests=[str(item) for item in data.get("tests", [])],
            risks=[str(item) for item in data.get("risks", [])],
            open_questions=[str(item) for item in data.get("open_questions", [])],
            next_recommended_action=str(data.get("next_recommended_action", "")),
            context_sufficiency=normalize_context_sufficiency(data.get("context_sufficiency")),
            turns_used=int(data.get("turns_used", 1)),
            metadata=dict(data.get("metadata", {})),
        )

    def write_json(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.to_dict(), indent=2) + "\n", encoding="utf-8")

    def write_markdown(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        frontmatter = json.dumps(self.to_dict(), ensure_ascii=False, indent=2)
        path.write_text(
            "---json\n"
            f"{frontmatter}\n"
            "---\n\n"
            f"# Report: {self.call_id}\n\n"
            f"{self.summary}\n",
            encoding="utf-8",
        )


def validate_report_contract(report: ChildReport, *, max_turns: int) -> ReportValidation:
    findings: list[str] = []
    if report.status not in {item.value for item in ReportStatus}:
        findings.append(f"invalid report status: {report.status}")
    if not report.summary.strip():
        findings.append("missing report summary")
    if report.turns_used > max_turns:
        findings.append(f"child exceeded lifecycle turn limit: {report.turns_used} > {max_turns}")
    for name in ("changed_files", "commands_run", "tests", "risks", "open_questions"):
        value = getattr(report, name)
        if not isinstance(value, list):
            findings.append(f"{name} must be a list")
    return ReportValidation(ok=not findings, findings=findings)


def validate_report_dict(data: dict[str, Any]) -> ReportValidation:
    findings = []
    for field_name in REPORT_JSON_SCHEMA["required"]:
        if field_name not in data:
            findings.append(f"missing report field: {field_name}")
    for field_name in ("changed_files", "commands_run", "tests", "risks", "open_questions"):
        if field_name in data and not isinstance(data[field_name], list):
            findings.append(f"{field_name} must be a list")
    if "context_sufficiency" in data and not isinstance(data["context_sufficiency"], dict):
        findings.append("context_sufficiency must be an object")
    if "status" in data and data["status"] not in {item.value for item in ReportStatus}:
        findings.append(f"invalid report status: {data['status']}")
    if "turns_used" in data and (not isinstance(data["turns_used"], int) or data["turns_used"] < 1):
        findings.append("turns_used must be a positive integer")
    return ReportValidation(ok=not findings, findings=findings)


def normalize_context_sufficiency(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        value = {}
    return {
        "status": str(value.get("status", "enough_to_act")),
        "missing": [str(item) for item in value.get("missing", [])],
        "can_parent_resolve": bool(value.get("can_parent_resolve", True)),
        "recommended_parent_action": str(value.get("recommended_parent_action", "")),
    }


def validate_scope(report: ChildReport, allowed_paths: tuple[str, ...]) -> ReportValidation:
    if not allowed_paths:
        return ReportValidation(ok=True)
    findings = []
    normalized_allowed = tuple(path.replace("\\", "/").rstrip("/") for path in allowed_paths)
    for changed in report.changed_files:
        normalized = changed.replace("\\", "/")
        if not any(normalized == path or normalized.startswith(f"{path}/") for path in normalized_allowed):
            findings.append(f"changed file outside allowed scope: {changed}")
    return ReportValidation(ok=not findings, findings=findings)
