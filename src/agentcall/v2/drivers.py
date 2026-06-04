from __future__ import annotations

import json
import re
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any, Protocol

from .reports import ChildReport, REPORT_JSON_SCHEMA, ReportStatus, report_schema_text, validate_report_dict
from .types import ChildCallSpec


class AgentDriver(Protocol):
    name: str

    def invoke(self, spec: ChildCallSpec) -> ChildReport:
        """Run one bounded child lifecycle and return its report."""


class FunctionAgentDriver:
    def __init__(self, name: str, handler: Callable[[ChildCallSpec], ChildReport]) -> None:
        self.name = name
        self._handler = handler

    def invoke(self, spec: ChildCallSpec) -> ChildReport:
        return self._handler(spec)


class HeadlessJsonClaudeDriver:
    """One-shot Claude fallback using `claude -p --output-format json`.

    It expects Claude to return a JSON object matching ChildReport fields. This
    is not as capable as a supervised PTY worker, but it is useful when a bounded lifecycle is more
    important than interactive control.
    """

    name = "claude-headless-json"

    def __init__(self, claude_bin: str = "claude") -> None:
        self.claude_bin = claude_bin

    def invoke(self, spec: ChildCallSpec) -> ChildReport:
        schema_hint = (
            "Return only JSON with keys: task_id, call_id, agent, status, summary, "
            "changed_files, commands_run, tests, risks, open_questions, "
            "next_recommended_action, turns_used, metadata."
        )
        process = subprocess.run(
            [
                self.claude_bin,
                "-p",
                "--permission-mode",
                spec.mode.agent_mode_id(),
                "--output-format",
                "json",
                "--json-schema",
                json.dumps(REPORT_JSON_SCHEMA),
                f"{spec.to_prompt()}\n\n{schema_hint}",
            ],
            cwd=Path(spec.workspace),
            text=True,
            capture_output=True,
            timeout=spec.max_seconds,
            check=False,
        )
        if process.returncode != 0:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="Claude headless invocation failed.",
                risks=[process.stderr.strip()],
                next_recommended_action="Inspect stderr and retry with a narrower prompt.",
            )
        try:
            data = json.loads(process.stdout)
        except json.JSONDecodeError:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="Claude did not return valid report JSON.",
                risks=[process.stdout[:1000]],
                next_recommended_action="Retry with stricter JSON schema.",
            )
        data["task_id"] = spec.task_id
        data["call_id"] = spec.call_id
        data["agent"] = self.name
        data.setdefault("context_sufficiency", default_context_sufficiency())
        data.setdefault("metadata", {})
        validation = validate_report_dict(data)
        if not validation.ok:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="Claude returned JSON that failed the report schema.",
                risks=validation.findings,
                metadata={"raw": data},
                next_recommended_action="Retry with a narrower bounded lifecycle.",
            )
        return ChildReport.from_dict(data)


def extract_json_object(text: str) -> dict[str, Any]:
    stripped = text.strip()
    if stripped.startswith("{") and stripped.endswith("}"):
        return json.loads(stripped)
    match = re.search(r"\{.*\}", text, re.DOTALL)
    if not match:
        raise ValueError("no JSON object found")
    data = json.loads(match.group(0))
    if not isinstance(data, dict):
        raise ValueError("JSON report must be an object")
    return data


def default_context_sufficiency() -> dict[str, Any]:
    return {
        "status": "enough_to_act",
        "missing": [],
        "can_parent_resolve": True,
        "recommended_parent_action": "",
    }
