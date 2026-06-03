from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from ..models import utc_now
from ..store import Store
from .claims import FileClaimPolicy
from .transcripts import index_transcript


@dataclass
class HookIngestionResult:
    event_type: str
    session_id: str | None = None
    status: str | None = None
    context_injection: str = ""
    decision: dict[str, Any] | None = None
    data: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "event_type": self.event_type,
            "session_id": self.session_id,
            "status": self.status,
            "context_injection": self.context_injection,
            "decision": self.decision,
            "data": self.data,
        }


class ClaudeCodeHookReceiver:
    def __init__(self, store: Store) -> None:
        self.store = store

    def ingest(self, hook_event: str, payload: dict[str, Any]) -> HookIngestionResult:
        session_id = str(
            payload.get("session_id")
            or payload.get("sessionId")
            or payload.get("transcript_path")
            or "unknown-session"
        )
        normalized = {
            "hook_event": hook_event,
            "session_id": session_id,
            "agent": payload.get("agent") or payload.get("agent_name"),
            "pid": payload.get("pid"),
            "status": payload.get("status"),
            "tool_name": payload.get("tool_name") or payload.get("toolName"),
            "transcript_path": payload.get("transcript_path"),
            "workspace": payload.get("workspace") or payload.get("cwd"),
            "raw": payload,
        }
        status = infer_status(hook_event, payload)
        decision = self._apply_policy(hook_event, payload)
        session = {
            "session_id": session_id,
            "runtime": "claude-code-session",
            "status": status,
            "agent": normalized["agent"] or "claude-code",
            "pid": normalized["pid"],
            "transcript_path": normalized["transcript_path"],
            "workspace": normalized["workspace"],
            "updated_at": utc_now(),
            "last_hook_event": hook_event,
        }
        self.store.upsert_active_session(session_id, session)
        if normalized["transcript_path"]:
            index_transcript(self.store, str(normalized["transcript_path"]), session_id=session_id)
        self.store.append_event(
            f"hook.{hook_event}",
            message=f"Claude Code hook received: {hook_event}",
            data=normalized | {"status": status, "decision": decision},
        )
        context_injection = ""
        if hook_event in {"SessionStart", "UserPromptSubmit"}:
            context_injection = self._context_injection()
        return HookIngestionResult(
            event_type=f"hook.{hook_event}",
            session_id=session_id,
            status=status,
            context_injection=context_injection,
            decision=decision,
            data=normalized,
        )

    def _apply_policy(self, hook_event: str, payload: dict[str, Any]) -> dict[str, Any] | None:
        policy = FileClaimPolicy(self.store)
        if hook_event == "PreToolUse":
            return policy.evaluate_pre_tool_use(payload).to_dict()
        if hook_event == "PostToolUse":
            return policy.observe_post_tool_use(payload).to_dict()
        if hook_event in {"Stop", "SubagentStop", "SessionEnd"}:
            session_id = str(
                payload.get("session_id")
                or payload.get("sessionId")
                or payload.get("agent_id")
                or payload.get("transcript_path")
                or "unknown-session"
            )
            released = policy.release_session(session_id)
            return {"allowed": True, "reason": "session claims released", "files": released, "conflicts": []}
        return None

    def _context_injection(self) -> str:
        project = self.store.project_state()
        active = self.store.list_active_sessions()
        return (
            "# AgentCall Context\n\n"
            f"- workspace: {self.store.root}\n"
            f"- active_sessions: {len(active)}\n"
            f"- open_risks: {len(project.get('risks', []))}\n"
            "Produce checkpoint reports when stopping, becoming idle, or completing a meaningful slice.\n"
        )


def infer_status(hook_event: str, payload: dict[str, Any]) -> str:
    explicit = payload.get("status")
    if explicit:
        return str(explicit)
    if hook_event in {"SessionStart", "UserPromptSubmit", "PreToolUse"}:
        return "running"
    if hook_event == "Notification":
        message = str(payload.get("message", "")).lower()
        if "permission" in message:
            return "needs_permission"
        if "idle" in message:
            return "idle"
        return "notified"
    if hook_event in {"Stop", "SubagentStop"}:
        return "checkpoint_due"
    if hook_event == "SessionEnd":
        return "ended"
    return "observed"
