from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..models import utc_now
from ..store import Store


@dataclass
class TranscriptSummary:
    transcript_path: str
    session_id: str
    messages: int = 0
    user_messages: int = 0
    assistant_messages: int = 0
    tool_uses: int = 0
    tool_results: int = 0
    last_text: str = ""
    updated_at: str = field(default_factory=utc_now)

    def to_dict(self) -> dict[str, Any]:
        return {
            "transcript_path": self.transcript_path,
            "session_id": self.session_id,
            "messages": self.messages,
            "user_messages": self.user_messages,
            "assistant_messages": self.assistant_messages,
            "tool_uses": self.tool_uses,
            "tool_results": self.tool_results,
            "last_text": self.last_text,
            "updated_at": self.updated_at,
        }


def index_transcript(store: Store, transcript_path: str, *, session_id: str | None = None) -> TranscriptSummary:
    path = Path(transcript_path)
    if not path.is_absolute():
        path = store.root / path
    summary = TranscriptSummary(
        transcript_path=str(path),
        session_id=session_id or path.stem,
    )
    if not path.exists():
        store.append_event(
            "transcript.index_failed",
            message=f"Transcript not found: {path}",
            data={"transcript_path": str(path), "session_id": summary.session_id},
        )
        return summary

    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if not line.strip():
                continue
            summary.messages += 1
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            role = str(item.get("role") or item.get("type") or "")
            if role == "user":
                summary.user_messages += 1
            if role == "assistant":
                summary.assistant_messages += 1
            content = item.get("content") or item.get("message") or item.get("text")
            scan_content(summary, content)

    transcripts = store.read_state_json("transcripts.json", {})
    transcripts[summary.session_id] = summary.to_dict()
    store.write_state_json("transcripts.json", transcripts)
    store.append_event(
        "transcript.indexed",
        message=f"Indexed transcript {summary.session_id}.",
        data=summary.to_dict(),
    )
    return summary


def scan_content(summary: TranscriptSummary, content: Any) -> None:
    if isinstance(content, str):
        if content.strip():
            summary.last_text = content.strip()[-500:]
        return
    if isinstance(content, dict):
        kind = content.get("type")
        if kind == "tool_use":
            summary.tool_uses += 1
        elif kind == "tool_result":
            summary.tool_results += 1
        text = content.get("text")
        if isinstance(text, str) and text.strip():
            summary.last_text = text.strip()[-500:]
        return
    if isinstance(content, list):
        for item in content:
            scan_content(summary, item)
