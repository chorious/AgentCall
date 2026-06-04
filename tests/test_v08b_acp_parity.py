from __future__ import annotations

import json
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
    daemon = Path("target/debug/agentcall-daemon.exe" if sys.platform == "win32" else "target/debug/agentcall-daemon")
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
        allowed_paths=["src"],
        acceptance_criteria=["same protocol transcript"],
    )

    python_log = tmp_path / "python-reference.jsonl"
    with AcpStdioClient([sys.executable, str(fake_server), str(python_log)], cwd=workspace, timeout_seconds=10) as client:
        client.initialize()
        session_id = client.new_session(workspace)
        client.set_mode(session_id, "acceptEdits")
        python_result = client.prompt(session_id, prompt)

    port = free_port()
    daemon_proc = subprocess.Popen(
        [str(daemon), "--workspace", str(tmp_path / "agentcall"), "--port", str(port)],
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
                "allowed_paths": ["src"],
                "acceptance_criteria": ["same protocol transcript"],
            },
        )
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
    allowed_paths: list[str],
    acceptance_criteria: list[str],
) -> str:
    allowed = "\n- ".join(allowed_paths) or "Entire workspace"
    criteria = "\n- ".join(acceptance_criteria) or "Produce a valid report"
    return (
        f"# AgentCall ACP Invocation: {call_id}\n\n"
        f"Task: `{task_id}`\n"
        f"Role: `{role}`\n"
        f"Mode: `{phase}`\n\n"
        f"## Objective\n\n{objective}\n\n"
        f"## Allowed Paths\n\n- {allowed}\n\n"
        f"## Acceptance Criteria\n\n- {criteria}\n\n"
        "## Required Report Contract\n\n"
        "Return exactly one structured report with status, summary, changed_files, commands_run, tests, risks, open_questions, and next_recommended_action. "
        "Include context_sufficiency with status, missing, can_parent_resolve, and recommended_parent_action. "
        "Do not continue beyond this lifecycle.\n"
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
