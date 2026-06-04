from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import textwrap
import time
import urllib.request
from pathlib import Path
from typing import Any

import pytest

from agentcall.v2.acp import AcpStdioClient


def test_rust_native_acp_matches_python_reference_io(tmp_path: Path) -> None:
    daemon = Path(
        os.environ.get(
            "AGENTCALL_DAEMON_EXE",
            "target/debug/agentcall-daemon.exe" if sys.platform == "win32" else "target/debug/agentcall-daemon",
        )
    )
    if not daemon.exists():
        pytest.skip("agentcall-daemon debug binary is required for ACP parity smoke")

    workspace = tmp_path / "workspace"
    workspace.mkdir()
    fake_server = write_fake_acp_server(tmp_path)
    prompt = route_prompt(
        objective="Audit ACP parity",
        task_id="task-acp",
        call_id="call-a",
        role="executor",
        phase="execute",
        workspace=str(workspace),
        allowed_paths=[str(workspace)],
        template="read-and-report",
        target_files=["src/lib.rs"],
        report_path=str(workspace / ".agentcall" / "reports" / "acp_parity.md"),
        acceptance_criteria=["same protocol transcript"],
    )

    python_log = tmp_path / "python-reference.jsonl"
    with AcpStdioClient([sys.executable, str(fake_server), str(python_log)], cwd=workspace, timeout_seconds=10) as client:
        client.initialize()
        session_id = client.new_session(workspace)
        client.set_mode(session_id, "acceptEdits")
        python_result = client.prompt(session_id, prompt)

    port = free_port()
    agentcall_root = tmp_path / "agentcall"
    config_dir = agentcall_root / "config"
    config_dir.mkdir(parents=True)
    (config_dir / "agentcall.local.json").write_text(
        json.dumps({"claude_workspace": str(workspace)}, ensure_ascii=False),
        encoding="utf-8",
    )
    daemon_proc = subprocess.Popen(
        [str(daemon), "--workspace", str(agentcall_root), "--port", str(port)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    try:
        wait_for_daemon(port)
        rust_log = tmp_path / "rust-native.jsonl"
        route = post_json(
            port,
            "/api/routes",
            {
                "objective": "Audit ACP parity",
                "workspace": str(workspace),
                "mode": "start",
                "runtime": "acp",
                "adapter_command": [sys.executable, str(fake_server), str(rust_log)],
                "timeout_seconds": 10,
                "task_id": "task-acp",
                "call_id": "call-a",
                "phase": "execute",
                "role": "executor",
                "allowed_paths": [str(workspace)],
                "template": "read-and-report",
                "target_files": ["src/lib.rs"],
                "report_path": str(workspace / ".agentcall" / "reports" / "acp_parity.md"),
                "max_writes": 1,
                "acceptance_criteria": ["same protocol transcript"],
            },
        )
        route = wait_for_route(port, route["route_id"])
    finally:
        daemon_proc.terminate()
        try:
            daemon_proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            daemon_proc.kill()

    rust_result = route["result"]["acp_result"]
    assert python_result.text() == "ACP fake completed."
    assert python_result.stop_reason == "end_turn"
    assert rust_result["text"] == python_result.text()
    assert rust_result["stop_reason"] == python_result.stop_reason
    assert normalize_client_messages(rust_log) == normalize_client_messages(python_log)


def write_fake_acp_server(tmp_path: Path) -> Path:
    fake_server = tmp_path / "fake_acp_server.py"
    fake_server.write_text(
        textwrap.dedent(
            r'''
            import json
            import sys
            from pathlib import Path

            log_path = Path(sys.argv[1])

            def log(direction, message):
                with log_path.open("a", encoding="utf-8") as handle:
                    handle.write(json.dumps({"direction": direction, "message": message}, ensure_ascii=False) + "\n")

            def send(message):
                log("server", message)
                print(json.dumps(message, ensure_ascii=False), flush=True)

            for line in sys.stdin:
                msg = json.loads(line)
                log("client", msg)
                method = msg.get("method")
                req_id = msg.get("id")
                if method == "initialize":
                    send({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "protocolVersion": 1,
                            "agentCapabilities": {"sessionCapabilities": {"modes": {}}},
                            "agentInfo": {"name": "fake-acp", "version": "0.0.1"},
                            "authMethods": []
                        }
                    })
                elif method == "session/new":
                    send({"jsonrpc": "2.0", "id": req_id, "result": {"sessionId": "sess_fake"}})
                elif method == "session/set_mode":
                    send({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "sess_fake",
                            "update": {"sessionUpdate": "current_mode_update", "modeId": msg["params"]["modeId"]}
                        }
                    })
                    send({"jsonrpc": "2.0", "id": req_id, "result": {}})
                elif method == "session/prompt":
                    send({
                        "jsonrpc": "2.0",
                        "id": 700,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "sess_fake",
                            "tool": "Read",
                            "file_path": "src/lib.rs",
                            "toolCall": {"toolCallId": "call_1"},
                            "options": [
                                {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                                {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"}
                            ]
                        }
                    })
                    permission = json.loads(sys.stdin.readline())
                    log("client", permission)
                    send({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "sess_fake",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "ACP fake completed."}
                            }
                        }
                    })
                    send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})
            '''
        ),
        encoding="utf-8",
    )
    return fake_server


def route_prompt(
    *,
    objective: str,
    task_id: str,
    call_id: str,
    role: str,
    phase: str,
    workspace: str,
    allowed_paths: list[str],
    template: str,
    target_files: list[str],
    report_path: str,
    acceptance_criteria: list[str],
) -> str:
    allowed = "\n- ".join(allowed_paths) or "Entire workspace"
    criteria = "\n- ".join(acceptance_criteria) or "Produce a valid report"
    context_packet = {
        "task_id": task_id,
        "call_id": call_id,
        "phase": phase,
        "role": role,
        "runtime": "acp",
        "workspace": workspace,
        "objective": objective,
        "allowed_paths": allowed_paths,
        "acceptance_criteria": acceptance_criteria,
        "template": template,
        "target_files": target_files,
        "report_path": report_path,
        "max_reads": None,
        "max_writes": 1,
    }
    target = "\n- ".join(target_files) or "No target files supplied"
    return (
        f"# AgentCall ACP Invocation: {call_id}\n\n"
        f"Task: `{task_id}`\n"
        f"Role: `{role}`\n"
        f"Mode: `{phase}`\n\n"
        f"## SOP Template\n\n`{template}`\n\n"
        f"## Objective\n\n{objective}\n\n"
        f"## Target Files\n\n- {target}\n\n"
        f"## Writable Report Path\n\n`{report_path}`\n\n"
        f"## Allowed Paths\n\n- {allowed}\n\n"
        f"## Acceptance Criteria\n\n- {criteria}\n\n"
        "## Context Packet\n\n"
        "Use this packet as the authoritative project context for this lifecycle.\n\n"
        "```json\n"
        f"{json.dumps(context_packet, ensure_ascii=False, indent=2, sort_keys=True)}\n"
        "```\n\n"
        "## Mode Rules\n\n"
        "- This is an ACP SOP worker, not a free implementation runtime.\n"
        "- Read only the target files or allowed paths needed for evidence.\n"
        "- Write/Edit/MultiEdit is only allowed for the single report path above.\n"
        "- Do not modify implementation files.\n"
        "- Bash write, redirect, delete, move, and copy commands are forbidden.\n"
        "- Stop after producing the report; do not continue into another lifecycle.\n\n"
        "## Required Report Contract\n\n"
        "Return exactly one structured report at the report path and in final text when possible. It must include these fields: "
        "status, summary, verdict, evidence, files_read, changed_files, risks, next_recommended_action, context_sufficiency. "
        "`changed_files` must contain only the report path. "
        "`context_sufficiency` must say whether the provided target files and criteria were enough.\n"
    )


def normalize_client_messages(log_path: Path) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for line in log_path.read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        if event["direction"] != "client":
            continue
        message = event["message"]
        if message.get("method") == "initialize":
            message = json.loads(json.dumps(message))
            message["params"]["clientInfo"]["version"] = "<version>"
        if message.get("method") == "session/new":
            message = json.loads(json.dumps(message))
            message["params"]["cwd"] = "<workspace>"
        if message.get("id") == 700 and "result" in message:
            message = {
                "jsonrpc": message["jsonrpc"],
                "id": message["id"],
                "result": message["result"],
            }
        items.append(message)
    return items


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_daemon(port: int) -> None:
    deadline = time.time() + 5
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/runtime/health", timeout=0.5) as response:
                if response.status == 200:
                    return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("daemon did not become ready")


def post_json(port: int, path: str, payload: dict[str, Any]) -> dict[str, Any]:
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.loads(response.read().decode("utf-8"))


def get_json(port: int, path: str) -> dict[str, Any]:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=15) as response:
        return json.loads(response.read().decode("utf-8"))


def wait_for_route(port: int, route_id: str) -> dict[str, Any]:
    deadline = time.time() + 15
    while time.time() < deadline:
        route = get_json(port, f"/api/routes/{route_id}")
        if route.get("status") not in {"started", "running"}:
            return route
        time.sleep(0.1)
    raise AssertionError(f"route {route_id} did not finish")
