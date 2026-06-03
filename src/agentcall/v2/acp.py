from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


class AcpError(RuntimeError):
    pass


@dataclass
class AcpPromptResult:
    session_id: str
    stop_reason: str | None
    updates: list[dict[str, Any]] = field(default_factory=list)

    def text(self) -> str:
        chunks: list[str] = []
        for item in self.updates:
            update = item.get("update", {})
            if update.get("sessionUpdate") != "agent_message_chunk":
                continue
            content = update.get("content", {})
            if content.get("type") == "text":
                chunks.append(str(content.get("text", "")))
        return "".join(chunks)


class AcpStdioClient:
    """Minimal ACP client over newline-delimited JSON-RPC stdio."""

    def __init__(self, command: list[str], *, cwd: Path, timeout_seconds: int = 900) -> None:
        self.command = command
        self.cwd = cwd
        self.timeout_seconds = timeout_seconds
        self.process: subprocess.Popen[str] | None = None
        self.next_id = 0
        self.updates: list[dict[str, Any]] = []

    def __enter__(self) -> "AcpStdioClient":
        self.process = subprocess.Popen(
            self.command,
            cwd=self.cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self.process is None:
            return
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()

    def initialize(self) -> dict[str, Any]:
        return self.call(
            "initialize",
            {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile": False, "writeTextFile": False},
                    "terminal": False,
                },
                "clientInfo": {"name": "agentcall", "title": "AgentCall", "version": "0.7.1"},
            },
        )

    def new_session(self, cwd: Path) -> str:
        result = self.call("session/new", {"cwd": str(cwd.resolve()), "mcpServers": []})
        session_id = result.get("sessionId")
        if not session_id:
            raise AcpError("ACP session/new response did not include sessionId")
        return str(session_id)

    def set_mode(self, session_id: str, mode: str) -> dict[str, Any]:
        return self.call("session/set_mode", {"sessionId": session_id, "modeId": mode})

    def prompt(self, session_id: str, text: str) -> AcpPromptResult:
        before = len(self.updates)
        result = self.call(
            "session/prompt",
            {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}],
            },
        )
        return AcpPromptResult(
            session_id=session_id,
            stop_reason=result.get("stopReason"),
            updates=self.updates[before:],
        )

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}})
        while True:
            message = self._read()
            if "id" in message and message.get("method"):
                self._handle_agent_request(message)
                continue
            if message.get("method") == "session/update":
                self.updates.append(dict(message.get("params", {})))
                continue
            if message.get("id") != request_id:
                continue
            if "error" in message:
                error = message["error"]
                raise AcpError(f"ACP {method} failed: {error}")
            result = message.get("result", {})
            return result if isinstance(result, dict) else {}

    def _handle_agent_request(self, message: dict[str, Any]) -> None:
        method = str(message.get("method", ""))
        request_id = message.get("id")
        params = message.get("params", {})
        if method == "session/request_permission":
            options = params.get("options", [])
            selected = None
            for option in options:
                if str(option.get("kind", "")).startswith("allow"):
                    selected = option.get("optionId")
                    break
            if selected is None and options:
                selected = options[0].get("optionId")
            result = {"outcome": {"outcome": "selected", "optionId": selected}}
            self._write({"jsonrpc": "2.0", "id": request_id, "result": result})
            return

        self._write(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": f"Unsupported client method: {method}"},
            }
        )

    def _write(self, message: dict[str, Any]) -> None:
        if self.process is None or self.process.stdin is None:
            raise AcpError("ACP process is not running")
        self.process.stdin.write(json.dumps(message, ensure_ascii=False) + "\n")
        self.process.stdin.flush()

    def _read(self) -> dict[str, Any]:
        if self.process is None or self.process.stdout is None:
            raise AcpError("ACP process is not running")
        line = self.process.stdout.readline()
        if not line:
            stderr = ""
            if self.process.stderr is not None:
                stderr = self.process.stderr.read()
            raise AcpError(f"ACP process closed stdout. stderr: {stderr.strip()}")
        try:
            message = json.loads(line)
        except json.JSONDecodeError as exc:
            raise AcpError(f"ACP emitted invalid JSON line: {line!r}") from exc
        if not isinstance(message, dict):
            raise AcpError(f"ACP emitted non-object JSON message: {message!r}")
        return message
