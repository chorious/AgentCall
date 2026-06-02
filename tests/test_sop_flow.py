from __future__ import annotations

import sys
import textwrap

from agentcall.cli import main, strip_ansi
from agentcall.store import Store
from agentcall.v2 import AcpClaudeDriver, ChildMode, ChildReport, FunctionAgentDriver, ParentOrchestrator, ReportStatus


def test_sop_flow(tmp_path):
    root = str(tmp_path)

    assert main(["--root", root, "init"]) == 0
    assert main(["--root", root, "task", "create", "Build test worker"]) == 0

    report_script = (
        "from pathlib import Path; "
        "Path('.agentcall/tasks/task-0001/report.md').write_text("
        "'---\\ntask_id: task-0001\\nrun_id: run-0001\\nagent: test\\nstatus: done\\n---\\n\\nOK\\n', "
        "encoding='utf-8')"
    )
    assert main(["--root", root, "run", "start", "task-0001", "--", sys.executable, "-c", report_script]) == 0

    store = Store(root)
    task = store.load_task("task-0001")
    assert task.status == "report_ready"
    run_json = tmp_path / ".agentcall" / "tasks" / "task-0001" / "runs" / "run-0001" / "run.json"
    assert run_json.exists()

    assert main(["--root", root, "review", "task-0001", "--decision", "accepted", "--notes", "ok"]) == 0
    task = store.load_task("task-0001")
    assert task.status == "accepted"
    assert store.review_path("task-0001").exists()

    events = store.events("task-0001")
    assert [event["type"] for event in events] == [
        "task.created",
        "task.status_changed",
        "run.starting",
        "run.started",
        "task.status_changed",
        "run.completed",
        "task.status_changed",
        "task.status_changed",
        "review.completed",
    ]


def test_worker_registry(tmp_path):
    root = str(tmp_path)

    assert main(["--root", root, "init"]) == 0
    assert main(
        [
            "--root",
            root,
            "worker",
            "register",
            "GLM1",
            "--pid",
            "38168",
            "--title",
            "GLM1",
        ]
    ) == 0

    workers = Store(root).list_workers()
    assert len(workers) == 1
    assert workers[0].id == "GLM1"
    assert workers[0].pid == 38168
    assert workers[0].source == "window-title"


def test_task_assignment_writes_worker_inbox(tmp_path):
    root = str(tmp_path)

    assert main(["--root", root, "init"]) == 0
    assert main(["--root", root, "worker", "register", "Kimi1", "--pid", "24888", "--title", "Kimi1"]) == 0
    assert main(["--root", root, "task", "create", "Write a tiny SOP report"]) == 0
    assert main(["--root", root, "task", "assign", "task-0001", "Kimi1"]) == 0

    store = Store(root)
    task = store.load_task("task-0001")
    assert task.assigned_worker == "Kimi1"
    inbox = tmp_path / ".agentcall" / "workers" / "Kimi1" / "inbox" / "task-0001.md"
    assert inbox.exists()
    assert ".agentcall/tasks/task-0001/report.md" in inbox.read_text(encoding="utf-8")


def test_strip_ansi_for_plain_session_tail():
    text = "\x1b[32mready\x1b[0m\r\n\x1b]0;title\x07"
    assert strip_ansi(text) == "ready\r\n"


def test_v2_parent_runs_bounded_child_lifecycle_on_small_project(tmp_path):
    root = tmp_path
    project = root / "mini_project"
    project.mkdir()
    calculator = project / "calculator.py"
    calculator.write_text("def add(a, b):\n    return a - b\n", encoding="utf-8")

    store = Store(root)
    store.init()

    def child_handler(spec):
        if spec.mode == ChildMode.PLAN:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent="scripted-claude",
                status=ReportStatus.DONE.value,
                summary="Plan: fix calculator.add and run a direct Python check.",
                next_recommended_action="execute approved plan",
            )

        assert spec.mode == ChildMode.EXECUTE
        calculator.write_text("def add(a, b):\n    return a + b\n", encoding="utf-8")
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="scripted-claude",
            status=ReportStatus.DONE.value,
            summary="Fixed add implementation and verified a small check.",
            changed_files=["mini_project/calculator.py"],
            commands_run=["python -c \"from mini_project.calculator import add; assert add(2, 3) == 5\""],
            tests=["direct add(2, 3) == 5 check passed"],
            next_recommended_action="accept",
        )

    outcome = ParentOrchestrator(
        store,
        FunctionAgentDriver("scripted-claude", child_handler),
    ).run_bounded_task(
        objective="Fix mini_project calculator add bug",
        allowed_paths=("mini_project",),
        acceptance_criteria=("add(2, 3) returns 5",),
    )

    assert outcome.accepted is True
    assert calculator.read_text(encoding="utf-8") == "def add(a, b):\n    return a + b\n"
    assert store.load_task(outcome.task_id).status == "accepted"
    assert store.report_path(outcome.task_id).exists()
    assert not store.review_path(outcome.task_id).exists()
    state_events = [event for event in store.events(outcome.task_id) if event["type"] == "agent.state_changed"]
    assert len(state_events) == 4
    assert state_events[-1]["data"]["state"] == "reported"
    assert state_events[-1]["data"]["role"] == "executor"
    reports_dir = root / ".agentcall" / "tasks" / outcome.task_id / "reports"
    assert (reports_dir / f"{outcome.task_id}-planner-01.json").exists()
    assert (reports_dir / f"{outcome.task_id}-executor-02.json").exists()


def test_v2_parent_rejects_child_that_exceeds_lifecycle(tmp_path):
    store = Store(tmp_path)
    store.init()

    def child_handler(spec):
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="scripted-claude",
            status=ReportStatus.DONE.value,
            summary="Used too many turns.",
            turns_used=2,
        )

    outcome = ParentOrchestrator(
        store,
        FunctionAgentDriver("scripted-claude", child_handler),
    ).run_bounded_task(
        objective="Do bounded work",
        allowed_paths=("mini_project",),
        acceptance_criteria=("one lifecycle only",),
        max_turns=1,
    )

    assert outcome.accepted is False
    assert "child exceeded lifecycle turn limit: 2 > 1" in outcome.findings
    assert store.load_task(outcome.task_id).status == "needs_revision"
    assert store.review_path(outcome.task_id).exists()


def test_v2_parent_delegates_review_when_it_lacks_confidence(tmp_path):
    store = Store(tmp_path)
    store.init()
    review_calls = []

    def child_handler(spec):
        if spec.mode == ChildMode.PLAN:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent="scripted-claude",
                status=ReportStatus.DONE.value,
                summary="Plan looks scoped.",
            )
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="scripted-claude",
            status=ReportStatus.DONE.value,
            summary="Work completed, but parent should inspect risk.",
            changed_files=["mini_project/app.py"],
            commands_run=["pytest mini_project"],
            tests=["pytest mini_project passed"],
            risks=["medium risk diff"],
        )

    def reviewer_handler(spec):
        review_calls.append(spec)
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="scripted-reviewer",
            status=ReportStatus.DONE.value,
            summary="Risk is acceptable for the requested scope.",
            tests=["reviewed executor report and parent findings"],
            next_recommended_action="accept",
        )

    outcome = ParentOrchestrator(
        store,
        FunctionAgentDriver("scripted-claude", child_handler),
        reviewer=FunctionAgentDriver("scripted-reviewer", reviewer_handler),
    ).run_bounded_task(
        objective="Patch mini project",
        allowed_paths=("mini_project",),
        acceptance_criteria=("tests pass",),
    )

    assert outcome.accepted is True
    assert len(review_calls) == 1
    assert outcome.review_report is not None
    assert store.load_task(outcome.task_id).status == "accepted"
    assert not store.review_path(outcome.task_id).exists()


def test_v2_parent_rejects_reported_file_that_does_not_exist(tmp_path):
    store = Store(tmp_path)
    store.init()

    def child_handler(spec):
        if spec.mode == ChildMode.PLAN:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent="scripted-claude",
                status=ReportStatus.DONE.value,
                summary="Plan is bounded.",
            )
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="scripted-claude",
            status=ReportStatus.DONE.value,
            summary="Claims to have changed a missing file.",
            changed_files=["mini_project/missing.py"],
            commands_run=["pytest mini_project"],
            tests=["pytest passed"],
        )

    outcome = ParentOrchestrator(
        store,
        FunctionAgentDriver("scripted-claude", child_handler),
    ).run_bounded_task(
        objective="Patch mini project",
        allowed_paths=("mini_project",),
        acceptance_criteria=("tests pass",),
    )

    assert outcome.accepted is False
    assert "changed file does not exist in workspace: mini_project/missing.py" in outcome.findings
    assert store.review_path(outcome.task_id).exists()


def test_v2_workflow_simulation_cli(tmp_path, capsys):
    assert main(["--root", str(tmp_path), "workflow", "simulate"]) == 0

    store = Store(tmp_path)
    tasks = store.list_tasks()
    assert len(tasks) == 1
    assert tasks[0].status == "accepted"
    assert (tmp_path / ".agentcall" / "simulations" / "small_project" / "calculator.py").read_text(
        encoding="utf-8"
    ) == "def add(a, b):\n    return a + b\n"
    assert main(["--root", str(tmp_path), "workflow", "inspect", tasks[0].id]) == 0
    output = capsys.readouterr().out
    assert "reports: 2" in output
    assert "review: no" in output
    assert "executor\treported" in output


def test_v2_acp_driver_reads_structured_report_from_stdio_agent(tmp_path):
    project = tmp_path / "mini_project"
    project.mkdir()
    (project / "calculator.py").write_text("def add(a, b):\n    return a + b\n", encoding="utf-8")
    fake_agent = tmp_path / "fake_acp_agent.py"
    fake_agent.write_text(
        textwrap.dedent(
            r'''
            import json
            import sys

            def send(message):
                print(json.dumps(message), flush=True)

            current_mode = "execute"
            for line in sys.stdin:
                msg = json.loads(line)
                method = msg.get("method")
                req_id = msg.get("id")
                if method == "initialize":
                    send({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "protocolVersion": 1,
                            "agentCapabilities": {
                                "sessionCapabilities": {"modes": {}}
                            },
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
                    current_mode = msg["params"]["modeId"]
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
                    assert permission["result"]["outcome"]["optionId"] == "allow-once"
                    report = {
                        "task_id": "placeholder",
                        "call_id": "placeholder",
                        "agent": "fake-acp",
                        "status": "done",
                        "summary": "ACP fake completed one bounded lifecycle.",
                        "changed_files": [] if current_mode == "plan" else ["mini_project/calculator.py"],
                        "commands_run": [] if current_mode == "plan" else ["pytest"],
                        "tests": ["plan mode returned no file changes"] if current_mode == "plan" else ["pytest passed"],
                        "risks": [],
                        "open_questions": [],
                        "next_recommended_action": "execute approved plan" if current_mode == "plan" else "accept",
                        "turns_used": 1,
                        "metadata": {}
                    }
                    send({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "sess_fake",
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": json.dumps(report)}
                            }
                        }
                    })
                    send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})
            '''
        ),
        encoding="utf-8",
    )

    driver = AcpClaudeDriver(command=[sys.executable, str(fake_agent)])
    store = Store(tmp_path)
    store.init()

    outcome = ParentOrchestrator(store, driver).run_bounded_task(
        objective="Run fake ACP child",
        allowed_paths=("mini_project",),
        acceptance_criteria=("fake report returns done",),
    )

    assert outcome.accepted is True
    assert len(outcome.reports) == 2
    assert outcome.reports[0].changed_files == []
    assert all(report.agent == "claude-acp" for report in outcome.reports)
    assert all(report.metadata["stopReason"] == "end_turn" for report in outcome.reports)
