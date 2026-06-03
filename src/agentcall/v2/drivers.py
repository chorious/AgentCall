from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any, Protocol

from .reports import ChildReport, REPORT_JSON_SCHEMA, ReportStatus, report_schema_text, validate_report_dict
from .types import ChildCallSpec
from .acp import AcpError, AcpStdioClient


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


class AcpClaudeDriver:
    """ACP Claude driver boundary.

    The real adapter is a stdio JSON-RPC server:
    `npx -y @agentclientprotocol/claude-agent-acp`.
    This class captures the process boundary and prompt/report contract. Full
    JSON-RPC event handling belongs in the Rust daemon once the UI consumes the
    same AgentState stream.
    """

    name = "claude-acp"

    def __init__(self, command: list[str] | None = None, timeout_seconds: int = 900) -> None:
        self.command = resolve_command(command or ["npx", "-y", "@agentclientprotocol/claude-agent-acp"])
        self.timeout_seconds = timeout_seconds

    def command_line(self) -> list[str]:
        return list(self.command)

    def invoke(self, spec: ChildCallSpec) -> ChildReport:
        prompt = (
            f"{spec.to_prompt()}\n\n"
            "Return only a JSON report object matching this JSON Schema. "
            "Do not include markdown fences.\n\n"
            f"{report_schema_text()}"
        )
        try:
            with AcpStdioClient(self.command, cwd=spec.workspace, timeout_seconds=self.timeout_seconds) as client:
                initialize = client.initialize()
                session_id = client.new_session(spec.workspace)
                client.set_mode(session_id, spec.mode.agent_mode_id())
                result = client.prompt(session_id, prompt)
        except AcpError as exc:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="ACP invocation failed before a valid report was returned.",
                risks=[str(exc)],
                next_recommended_action="Inspect ACP transport logs and retry with a scripted driver.",
            )

        text = result.text()
        try:
            data = extract_json_object(text)
        except ValueError:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="ACP agent completed without returning valid report JSON.",
                risks=[text[:1000]],
                metadata={"stopReason": result.stop_reason, "initialize": initialize},
                next_recommended_action="Retry with a stricter report prompt or JSON schema.",
            )
        data["task_id"] = spec.task_id
        data["call_id"] = spec.call_id
        data["agent"] = self.name
        data.setdefault("context_sufficiency", default_context_sufficiency())
        data.setdefault("metadata", {})
        data["metadata"].setdefault("stopReason", result.stop_reason)
        data["metadata"].setdefault("acpUpdates", len(result.updates))
        validation = validate_report_dict(data)
        if not validation.ok:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent=self.name,
                status=ReportStatus.FAILED.value,
                summary="ACP agent returned a report that failed schema validation.",
                risks=validation.findings,
                metadata={"raw": data, "stopReason": result.stop_reason},
                next_recommended_action="Retry with a stricter child prompt.",
            )
        return ChildReport.from_dict(data)


class HeadlessJsonClaudeDriver:
    """One-shot Claude fallback using `claude -p --output-format json`.

    It expects Claude to return a JSON object matching ChildReport fields. This
    is not as capable as ACP, but it is useful when a bounded lifecycle is more
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


def resolve_command(command: list[str]) -> list[str]:
    if not command:
        return command
    executable = command[0]
    if os.path.isabs(executable) or any(sep in executable for sep in ("\\", "/")):
        return command
    candidates = [executable]
    if os.name == "nt":
        candidates = [executable, f"{executable}.cmd", f"{executable}.exe"]
    for candidate in candidates:
        resolved = shutil.which(candidate)
        if resolved:
            return [resolved, *command[1:]]
    return command
