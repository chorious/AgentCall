from __future__ import annotations

import sys

from agentcall.cli import main, strip_ansi
from agentcall.store import Store


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
