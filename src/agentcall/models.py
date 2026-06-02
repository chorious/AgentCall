from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import StrEnum
from typing import Any


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


class TaskStatus(StrEnum):
    CREATED = "created"
    RUNNING = "running"
    REPORT_READY = "report_ready"
    REVIEWING = "reviewing"
    ACCEPTED = "accepted"
    NEEDS_REVISION = "needs_revision"
    BLOCKED = "blocked"
    FAILED = "failed"


class ReviewDecision(StrEnum):
    ACCEPTED = "accepted"
    NEEDS_REVISION = "needs_revision"
    BLOCKED = "blocked"


@dataclass
class Task:
    id: str
    title: str
    status: str = TaskStatus.CREATED.value
    assigned_worker: str | None = None
    created_at: str = field(default_factory=utc_now)
    updated_at: str = field(default_factory=utc_now)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "status": self.status,
            "assigned_worker": self.assigned_worker,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Task":
        return cls(
            id=str(data["id"]),
            title=str(data["title"]),
            status=str(data.get("status", TaskStatus.CREATED.value)),
            assigned_worker=data.get("assigned_worker"),
            created_at=str(data.get("created_at", utc_now())),
            updated_at=str(data.get("updated_at", utc_now())),
        )


@dataclass
class RunRecord:
    id: str
    task_id: str
    command: list[str]
    pid: int | None = None
    status: str = "created"
    exit_code: int | None = None
    started_at: str = field(default_factory=utc_now)
    completed_at: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "task_id": self.task_id,
            "command": self.command,
            "pid": self.pid,
            "status": self.status,
            "exit_code": self.exit_code,
            "started_at": self.started_at,
            "completed_at": self.completed_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "RunRecord":
        command = data.get("command", [])
        return cls(
            id=str(data["id"]),
            task_id=str(data["task_id"]),
            command=[str(part) for part in command],
            pid=data.get("pid"),
            status=str(data.get("status", "created")),
            exit_code=data.get("exit_code"),
            started_at=str(data.get("started_at", utc_now())),
            completed_at=data.get("completed_at"),
        )


@dataclass
class Worker:
    id: str
    pid: int
    title: str
    kind: str = "claude-code"
    source: str = "window-title"
    status: str = "external"
    created_at: str = field(default_factory=utc_now)
    updated_at: str = field(default_factory=utc_now)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "pid": self.pid,
            "title": self.title,
            "kind": self.kind,
            "source": self.source,
            "status": self.status,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Worker":
        return cls(
            id=str(data["id"]),
            pid=int(data["pid"]),
            title=str(data.get("title", data["id"])),
            kind=str(data.get("kind", "claude-code")),
            source=str(data.get("source", "window-title")),
            status=str(data.get("status", "external")),
            created_at=str(data.get("created_at", utc_now())),
            updated_at=str(data.get("updated_at", utc_now())),
        )
