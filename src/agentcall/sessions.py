from __future__ import annotations

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .models import utc_now
from .store import AgentCallError, Store


@dataclass
class SessionRecord:
    name: str
    command: list[str]
    worker_pid: int | None
    child_pid: int | None
    status: str
    created_at: str
    updated_at: str

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SessionRecord":
        return cls(
            name=str(data["name"]),
            command=[str(part) for part in data.get("command", [])],
            worker_pid=data.get("worker_pid"),
            child_pid=data.get("child_pid"),
            status=str(data.get("status", "unknown")),
            created_at=str(data.get("created_at", utc_now())),
            updated_at=str(data.get("updated_at", utc_now())),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "command": self.command,
            "worker_pid": self.worker_pid,
            "child_pid": self.child_pid,
            "status": self.status,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }


class SessionManager:
    def __init__(self, store: Store) -> None:
        self.store = store
        self.sessions_dir = self.store.agent_dir / "sessions"

    def session_dir(self, name: str) -> Path:
        return self.sessions_dir / name

    def state_path(self, name: str) -> Path:
        return self.session_dir(name) / "state.json"

    def input_path(self, name: str) -> Path:
        return self.session_dir(name) / "input.ndjson"

    def output_path(self, name: str) -> Path:
        return self.session_dir(name) / "output.log"

    def start(self, name: str, command: list[str], *, cols: int = 100, rows: int = 40) -> SessionRecord:
        self.store.require_initialized()
        if not command:
            raise AgentCallError("Missing session command after --")
        session_dir = self.session_dir(name)
        if self.state_path(name).exists():
            existing = self.load(name)
            if existing.status in {"starting", "running"} and existing.worker_pid:
                raise AgentCallError(f"Session already exists: {name}")

        session_dir.mkdir(parents=True, exist_ok=True)
        self.input_path(name).touch(exist_ok=True)
        self.output_path(name).touch(exist_ok=True)
        (session_dir / "worker.stdout.log").touch(exist_ok=True)
        (session_dir / "worker.stderr.log").touch(exist_ok=True)

        record = SessionRecord(
            name=name,
            command=command,
            worker_pid=None,
            child_pid=None,
            status="starting",
            created_at=utc_now(),
            updated_at=utc_now(),
        )
        self.save(record)

        env = os.environ.copy()
        src_path = str((Path(__file__).resolve().parents[1]))
        env["PYTHONPATH"] = src_path + os.pathsep + env.get("PYTHONPATH", "")
        args = [
            sys.executable,
            "-m",
            "agentcall.session_worker",
            "--root",
            str(self.store.root),
            "--name",
            name,
            "--cols",
            str(cols),
            "--rows",
            str(rows),
            "--command-json",
            json.dumps(command),
        ]
        flags = 0
        if os.name == "nt":
            flags = subprocess.CREATE_NEW_PROCESS_GROUP
        with (session_dir / "worker.stdout.log").open("ab") as stdout, (session_dir / "worker.stderr.log").open("ab") as stderr:
            proc = subprocess.Popen(
                args,
                cwd=self.store.root,
                env=env,
                stdout=stdout,
                stderr=stderr,
                stdin=subprocess.DEVNULL,
                creationflags=flags,
            )
        record.worker_pid = proc.pid
        record.updated_at = utc_now()
        self.save(record)
        self.store.append_event(
            "session.started",
            message=f"Session {name} started.",
            data={"name": name, "worker_pid": proc.pid, "command": command},
        )
        return record

    def load(self, name: str) -> SessionRecord:
        path = self.state_path(name)
        if not path.exists():
            raise AgentCallError(f"Session not found: {name}")
        return SessionRecord.from_dict(json.loads(path.read_text(encoding="utf-8")))

    def save(self, record: SessionRecord) -> None:
        session_dir = self.session_dir(record.name)
        session_dir.mkdir(parents=True, exist_ok=True)
        record.updated_at = utc_now()
        self.state_path(record.name).write_text(
            json.dumps(record.to_dict(), indent=2) + "\n",
            encoding="utf-8",
        )

    def list(self) -> list[SessionRecord]:
        self.store.require_initialized()
        if not self.sessions_dir.exists():
            return []
        return [
            SessionRecord.from_dict(json.loads(path.read_text(encoding="utf-8")))
            for path in sorted(self.sessions_dir.glob("*/state.json"))
        ]

    def send(self, name: str, text: str, *, enter: bool = True) -> None:
        self.load(name)
        payload = text + ("\r" if enter else "")
        event = {"ts": utc_now(), "type": "input", "text": payload}
        with self.input_path(name).open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event, ensure_ascii=False) + "\n")
        self.store.append_event(
            "session.input",
            message=f"Input queued for session {name}.",
            data={"name": name, "chars": len(payload), "enter": enter},
        )

    def stop(self, name: str) -> None:
        self.load(name)
        event = {"ts": utc_now(), "type": "stop"}
        with self.input_path(name).open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event) + "\n")
        self.store.append_event(
            "session.stop_requested",
            message=f"Stop requested for session {name}.",
            data={"name": name},
        )

    def tail(self, name: str, lines: int = 80) -> list[str]:
        self.load(name)
        path = self.output_path(name)
        if not path.exists():
            return []
        data = path.read_text(encoding="utf-8", errors="replace").splitlines()
        return data[-lines:]
