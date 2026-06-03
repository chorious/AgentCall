from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..models import utc_now
from ..store import Store


WRITE_TOOLS = {"Edit", "MultiEdit", "Write", "NotebookEdit"}


@dataclass
class ClaimDecision:
    allowed: bool
    reason: str
    files: list[str] = field(default_factory=list)
    conflicts: list[dict[str, Any]] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "allowed": self.allowed,
            "reason": self.reason,
            "files": self.files,
            "conflicts": self.conflicts,
        }


def normalize_workspace_path(path: str) -> str:
    normalized = path.replace("\\", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    return normalized


def extract_tool_files(payload: dict[str, Any]) -> list[str]:
    tool_name = str(payload.get("tool_name") or payload.get("toolName") or "")
    tool_input = payload.get("tool_input") or payload.get("toolInput") or {}
    if not isinstance(tool_input, dict):
        return []
    candidates: list[str] = []
    for key in ("file_path", "path", "notebook_path"):
        value = tool_input.get(key)
        if isinstance(value, str):
            candidates.append(value)
    if tool_name == "MultiEdit":
        value = tool_input.get("file_path")
        if isinstance(value, str):
            candidates.append(value)
    return sorted({normalize_workspace_path(item) for item in candidates if item})


class FileClaimPolicy:
    def __init__(self, store: Store) -> None:
        self.store = store

    def evaluate_pre_tool_use(self, payload: dict[str, Any]) -> ClaimDecision:
        tool_name = str(payload.get("tool_name") or payload.get("toolName") or "")
        if tool_name not in WRITE_TOOLS:
            return ClaimDecision(allowed=True, reason="tool does not claim files")
        session_id = session_id_from_payload(payload)
        files = extract_tool_files(payload)
        if not files:
            return ClaimDecision(allowed=True, reason="write tool did not expose file path")
        claims = self.store.file_claims()
        conflicts = []
        for file_path in files:
            claim = claims.get(file_path)
            if claim and claim.get("session_id") != session_id and claim.get("status") == "active":
                conflicts.append({"file": file_path, "claim": claim})
        if conflicts:
            return ClaimDecision(
                allowed=False,
                reason="file claim conflict",
                files=files,
                conflicts=conflicts,
            )
        for file_path in files:
            claims[file_path] = {
                "file": file_path,
                "session_id": session_id,
                "tool_name": tool_name,
                "status": "active",
                "claimed_at": claims.get(file_path, {}).get("claimed_at", utc_now()),
                "updated_at": utc_now(),
            }
        self.store.write_file_claims(claims)
        self.store.append_event(
            "file_claim.acquired",
            message=f"{session_id} claimed {', '.join(files)}.",
            data={"session_id": session_id, "files": files, "tool_name": tool_name},
        )
        return ClaimDecision(allowed=True, reason="file claims acquired", files=files)

    def observe_post_tool_use(self, payload: dict[str, Any]) -> ClaimDecision:
        tool_name = str(payload.get("tool_name") or payload.get("toolName") or "")
        files = extract_tool_files(payload)
        if not files:
            return ClaimDecision(allowed=True, reason="no file paths observed")
        session_id = session_id_from_payload(payload)
        claims = self.store.file_claims()
        for file_path in files:
            claim = claims.get(file_path, {})
            if claim.get("session_id") == session_id:
                claim["updated_at"] = utc_now()
                claim["last_tool_name"] = tool_name
                claims[file_path] = claim
        self.store.write_file_claims(claims)
        self.store.append_event(
            "file_claim.observed_write",
            message=f"{session_id} wrote {', '.join(files)}.",
            data={"session_id": session_id, "files": files, "tool_name": tool_name},
        )
        return ClaimDecision(allowed=True, reason="write observed", files=files)

    def release_session(self, session_id: str) -> list[str]:
        claims = self.store.file_claims()
        released = []
        for file_path, claim in list(claims.items()):
            if claim.get("session_id") == session_id and claim.get("status") == "active":
                claim["status"] = "released"
                claim["released_at"] = utc_now()
                claims[file_path] = claim
                released.append(file_path)
        self.store.write_file_claims(claims)
        if released:
            self.store.append_event(
                "file_claim.released",
                message=f"{session_id} released {', '.join(released)}.",
                data={"session_id": session_id, "files": released},
            )
        return released


def session_id_from_payload(payload: dict[str, Any]) -> str:
    return str(
        payload.get("session_id")
        or payload.get("sessionId")
        or payload.get("agent_id")
        or payload.get("transcript_path")
        or "unknown-session"
    )


def summarize_claims_for_board(claims: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        {"file": file_path, **dict(claim)}
        for file_path, claim in sorted(claims.items(), key=lambda item: item[0])
    ]
