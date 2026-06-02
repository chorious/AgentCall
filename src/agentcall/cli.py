from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

from .models import ReviewDecision, TaskStatus, Worker
from .sessions import SessionManager
from .store import AgentCallError, Store
from .supervisor import Supervisor
from .v2.inspection import inspect_workflow
from .v2.workflows import run_small_project_workflow_with_driver


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agentcall")
    parser.add_argument("--root", default=".", help="Workspace root. Defaults to current directory.")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("init", help="Initialize .agentcall in this workspace.")

    task = sub.add_parser("task", help="Manage tasks.")
    task_sub = task.add_subparsers(dest="task_command", required=True)
    task_create = task_sub.add_parser("create", help="Create a task.")
    task_create.add_argument("title")
    task_assign = task_sub.add_parser("assign", help="Assign a task to a registered worker.")
    task_assign.add_argument("task_id")
    task_assign.add_argument("worker_id")
    task_sub.add_parser("list", help="List tasks.")
    task_status = task_sub.add_parser("status", help="Show task status.")
    task_status.add_argument("task_id")

    run = sub.add_parser("run", help="Start supervised worker runs.")
    run_sub = run.add_subparsers(dest="run_command", required=True)
    run_start = run_sub.add_parser("start", help="Start a worker command for a task.")
    run_start.add_argument("task_id")
    run_start.add_argument("worker_command", nargs=argparse.REMAINDER)

    review = sub.add_parser("review", help="Write a review artifact for a task.")
    review.add_argument("task_id")
    review.add_argument("--decision", choices=[d.value for d in ReviewDecision], required=True)
    review.add_argument("--reviewer", default="cleverGPT")
    review.add_argument("--notes", default="")

    events = sub.add_parser("events", help="Show events.")
    events.add_argument("task_id", nargs="?")

    workflow = sub.add_parser("workflow", help="Run v2 bounded parent/child workflows.")
    workflow_sub = workflow.add_subparsers(dest="workflow_command", required=True)
    workflow_simulate = workflow_sub.add_parser("simulate", help="Run a small-project v2 lifecycle simulation.")
    workflow_simulate.add_argument(
        "--driver",
        choices=["scripted", "headless-json", "acp"],
        default="scripted",
        help="Child driver to use. Defaults to deterministic scripted simulation.",
    )
    workflow_simulate.add_argument(
        "--acp-command",
        default=None,
        help="ACP stdio command string. Used only with --driver acp.",
    )
    workflow_simulate.add_argument("--claude-bin", default="claude", help="Claude CLI path for headless-json.")
    workflow_simulate.add_argument("--max-turns", type=int, default=1, help="Lifecycle turn limit per child call.")
    workflow_inspect = workflow_sub.add_parser("inspect", help="Inspect a v2 workflow task.")
    workflow_inspect.add_argument("task_id")

    worker = sub.add_parser("worker", help="Register and inspect external workers.")
    worker_sub = worker.add_subparsers(dest="worker_command", required=True)
    worker_register = worker_sub.add_parser("register", help="Register an externally launched worker PID.")
    worker_register.add_argument("worker_id")
    worker_register.add_argument("--pid", type=int, required=True)
    worker_register.add_argument("--title", required=True)
    worker_register.add_argument("--kind", default="claude-code")
    worker_register.add_argument("--source", default="window-title")
    worker_sub.add_parser("list", help="List registered workers.")

    session = sub.add_parser("session", help="Manage tmux-like PTY sessions.")
    session_sub = session.add_subparsers(dest="session_command", required=True)
    session_start = session_sub.add_parser("start", help="Start a named PTY session.")
    session_start.add_argument("name")
    session_start.add_argument("--cols", type=int, default=100)
    session_start.add_argument("--rows", type=int, default=40)
    session_start.add_argument("session_command_args", nargs=argparse.REMAINDER)
    session_sub.add_parser("list", help="List sessions.")
    session_status = session_sub.add_parser("status", help="Show session status.")
    session_status.add_argument("name")
    session_send = session_sub.add_parser("send", help="Send text to a session.")
    session_send.add_argument("name")
    session_send.add_argument("text")
    session_send.add_argument("--no-enter", action="store_true")
    session_tail = session_sub.add_parser("tail", help="Print recent session output.")
    session_tail.add_argument("name")
    session_tail.add_argument("--lines", type=int, default=80)
    session_tail.add_argument("--plain", action="store_true", help="Strip ANSI escape sequences.")
    session_stop = session_sub.add_parser("stop", help="Request a session stop.")
    session_stop.add_argument("name")
    return parser


def main(argv: list[str] | None = None) -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if hasattr(sys.stderr, "reconfigure"):
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = build_parser()
    args = parser.parse_args(argv)
    store = Store(args.root)

    try:
        if args.command == "init":
            store.init()
            store.append_event("workspace.initialized", message="AgentCall workspace initialized.")
            print(f"Initialized {store.agent_dir}")
            return 0

        if args.command == "task":
            return handle_task(args, store)

        if args.command == "run":
            return handle_run(args, store)

        if args.command == "review":
            return handle_review(args, store)

        if args.command == "events":
            return handle_events(args, store)

        if args.command == "workflow":
            return handle_workflow(args, store)

        if args.command == "worker":
            return handle_worker(args, store)

        if args.command == "session":
            return handle_session(args, store)

    except AgentCallError as exc:
        print(f"agentcall: {exc}", file=sys.stderr)
        return 2

    parser.error("unknown command")
    return 2


def handle_task(args: argparse.Namespace, store: Store) -> int:
    if args.task_command == "create":
        task = store.create_task(args.title)
        print(task.id)
        return 0

    if args.task_command == "assign":
        task = store.assign_task(args.task_id, args.worker_id)
        print(f"{task.id}\tassigned_worker={task.assigned_worker}")
        return 0

    if args.task_command == "list":
        for task in store.list_tasks():
            worker = task.assigned_worker or "-"
            print(f"{task.id}\t{task.status}\t{worker}\t{task.title}")
        return 0

    if args.task_command == "status":
        task = store.load_task(args.task_id)
        report = "yes" if store.report_path(task.id).exists() else "no"
        review = "yes" if store.review_path(task.id).exists() else "no"
        print(f"id: {task.id}")
        print(f"title: {task.title}")
        print(f"status: {task.status}")
        print(f"assigned_worker: {task.assigned_worker or '-'}")
        print(f"report: {report}")
        print(f"review: {review}")
        return 0

    raise AgentCallError(f"Unknown task command: {args.task_command}")


def handle_run(args: argparse.Namespace, store: Store) -> int:
    if args.run_command != "start":
        raise AgentCallError(f"Unknown run command: {args.run_command}")

    command = normalize_remainder(args.worker_command)
    if not command:
        raise AgentCallError("Missing worker command after --")
    record = Supervisor(store).run(args.task_id, command)
    print(f"{record.id}\tpid={record.pid}\texit={record.exit_code}\tstatus={record.status}")
    return int(record.exit_code or 0)


def handle_review(args: argparse.Namespace, store: Store) -> int:
    store.require_initialized()
    task = store.load_task(args.task_id)
    store.update_task_status(task.id, TaskStatus.REVIEWING.value)
    path = store.review_path(task.id)
    text = render_review_md(
        task_id=task.id,
        decision=args.decision,
        reviewer=args.reviewer,
        notes=args.notes,
    )
    path.write_text(text, encoding="utf-8")
    if args.decision == ReviewDecision.ACCEPTED.value:
        status = TaskStatus.ACCEPTED.value
    elif args.decision == ReviewDecision.NEEDS_REVISION.value:
        status = TaskStatus.NEEDS_REVISION.value
    else:
        status = TaskStatus.BLOCKED.value
    store.update_task_status(task.id, status)
    store.append_event(
        "review.completed",
        task_id=task.id,
        message=f"Review decision: {args.decision}",
        data={"decision": args.decision, "reviewer": args.reviewer},
    )
    print(str(path.relative_to(Path(args.root).resolve())))
    return 0


def handle_events(args: argparse.Namespace, store: Store) -> int:
    for event in store.events(args.task_id):
        bits = [event["id"], event["ts"], event["type"]]
        if event.get("task_id"):
            bits.append(event["task_id"])
        if event.get("run_id"):
            bits.append(event["run_id"])
        if event.get("message"):
            bits.append(event["message"])
        print("\t".join(bits))
    return 0


def handle_workflow(args: argparse.Namespace, store: Store) -> int:
    if args.workflow_command == "simulate":
        try:
            outcome = run_small_project_workflow_with_driver(
                store.root,
                driver_kind=args.driver,
                acp_command=args.acp_command,
                claude_bin=args.claude_bin,
                max_turns=args.max_turns,
            )
        except ValueError as exc:
            raise AgentCallError(str(exc)) from exc
        status = "accepted" if outcome.accepted else "needs_revision"
        print(f"task_id: {outcome.task_id}")
        print(f"status: {status}")
        print(f"reports: {len(outcome.reports)}")
        if outcome.findings:
            print("findings:")
            for finding in outcome.findings:
                print(f"- {finding}")
        return 0 if outcome.accepted else 1

    if args.workflow_command == "inspect":
        inspection = inspect_workflow(store, args.task_id)
        for line in inspection.to_lines():
            print(line)
        return 0

    raise AgentCallError(f"Unknown workflow command: {args.workflow_command}")


def handle_worker(args: argparse.Namespace, store: Store) -> int:
    if args.worker_command == "register":
        worker = Worker(
            id=args.worker_id,
            pid=args.pid,
            title=args.title,
            kind=args.kind,
            source=args.source,
        )
        store.save_worker(worker)
        print(f"{worker.id}\tpid={worker.pid}\ttitle={worker.title}\tsource={worker.source}")
        return 0

    if args.worker_command == "list":
        for worker in store.list_workers():
            print(f"{worker.id}\tpid={worker.pid}\ttitle={worker.title}\tkind={worker.kind}\tsource={worker.source}")
        return 0

    raise AgentCallError(f"Unknown worker command: {args.worker_command}")


def handle_session(args: argparse.Namespace, store: Store) -> int:
    manager = SessionManager(store)

    if args.session_command == "start":
        command = normalize_remainder(args.session_command_args)
        record = manager.start(args.name, command, cols=args.cols, rows=args.rows)
        print(
            f"{record.name}\tstatus={record.status}\tworker_pid={record.worker_pid}"
            f"\tchild_pid={record.child_pid}\tcommand={' '.join(record.command)}"
        )
        return 0

    if args.session_command == "list":
        for record in manager.list():
            print(
                f"{record.name}\t{record.status}\tworker_pid={record.worker_pid}"
                f"\tchild_pid={record.child_pid}\tcommand={' '.join(record.command)}"
            )
        return 0

    if args.session_command == "status":
        record = manager.load(args.name)
        print(f"name: {record.name}")
        print(f"status: {record.status}")
        print(f"worker_pid: {record.worker_pid}")
        print(f"child_pid: {record.child_pid}")
        print(f"command: {' '.join(record.command)}")
        return 0

    if args.session_command == "send":
        manager.send(args.name, args.text, enter=not args.no_enter)
        print(f"sent\t{args.name}\tchars={len(args.text)}")
        return 0

    if args.session_command == "tail":
        for line in manager.tail(args.name, lines=args.lines):
            if args.plain:
                line = strip_ansi(line)
            print(line)
        return 0

    if args.session_command == "stop":
        manager.stop(args.name)
        print(f"stop requested\t{args.name}")
        return 0

    raise AgentCallError(f"Unknown session command: {args.session_command}")


def normalize_remainder(parts: list[str]) -> list[str]:
    if parts and parts[0] == "--":
        return parts[1:]
    return parts


def strip_ansi(text: str) -> str:
    return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)", "", text)


def render_review_md(*, task_id: str, decision: str, reviewer: str, notes: str) -> str:
    return (
        "---\n"
        f"task_id: {task_id}\n"
        f"decision: {decision}\n"
        f"reviewer: {reviewer}\n"
        "---\n\n"
        "# Review Notes\n\n"
        f"{notes or 'No notes.'}\n"
    )


if __name__ == "__main__":
    raise SystemExit(main())
