from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .models import Task, TaskStatus, Worker, utc_now


class AgentCallError(RuntimeError):
    pass


class Store:
    def __init__(self, root: Path | str = ".") -> None:
        self.root = Path(root).resolve()
        self.agent_dir = self.root / ".agentcall"
        self.tasks_dir = self.agent_dir / "tasks"
        self.workers_dir = self.agent_dir / "workers"
        self.events_path = self.agent_dir / "events.ndjson"

    def init(self) -> None:
        self.tasks_dir.mkdir(parents=True, exist_ok=True)
        self.workers_dir.mkdir(parents=True, exist_ok=True)
        self.events_path.touch(exist_ok=True)

    def require_initialized(self) -> None:
        if not self.agent_dir.exists():
            raise AgentCallError("AgentCall is not initialized. Run: agentcall init")

    def task_path(self, task_id: str) -> Path:
        return self.tasks_dir / task_id

    def task_json_path(self, task_id: str) -> Path:
        return self.task_path(task_id) / "task.json"

    def load_task(self, task_id: str) -> Task:
        path = self.task_json_path(task_id)
        if not path.exists():
            raise AgentCallError(f"Task not found: {task_id}")
        return Task.from_dict(json.loads(path.read_text(encoding="utf-8")))

    def save_task(self, task: Task) -> None:
        task.updated_at = utc_now()
        task_dir = self.task_path(task.id)
        task_dir.mkdir(parents=True, exist_ok=True)
        self.task_json_path(task.id).write_text(
            json.dumps(task.to_dict(), indent=2) + "\n",
            encoding="utf-8",
        )

    def next_task_id(self) -> str:
        self.require_initialized()
        max_seen = 0
        for child in self.tasks_dir.glob("task-*"):
            if not child.is_dir():
                continue
            try:
                max_seen = max(max_seen, int(child.name.removeprefix("task-")))
            except ValueError:
                continue
        return f"task-{max_seen + 1:04d}"

    def next_run_id(self, task_id: str) -> str:
        runs_dir = self.task_path(task_id) / "runs"
        max_seen = 0
        for child in runs_dir.glob("run-*"):
            if not child.is_dir():
                continue
            try:
                max_seen = max(max_seen, int(child.name.removeprefix("run-")))
            except ValueError:
                continue
        return f"run-{max_seen + 1:04d}"

    def create_task(self, title: str) -> Task:
        task = Task(id=self.next_task_id(), title=title)
        task_dir = self.task_path(task.id)
        task_dir.mkdir(parents=True, exist_ok=False)
        self.save_task(task)
        (task_dir / "task.md").write_text(render_task_md(task), encoding="utf-8")
        (task_dir / "runs").mkdir(exist_ok=True)
        self.append_event(
            "task.created",
            task_id=task.id,
            message=f"Task created: {title}",
            data={"title": title},
        )
        return task

    def update_task_status(self, task_id: str, status: str) -> Task:
        task = self.load_task(task_id)
        task.status = status
        self.save_task(task)
        self.append_event("task.status_changed", task_id=task_id, data={"status": status})
        return task

    def assign_task(self, task_id: str, worker_id: str) -> Task:
        worker_path = self.worker_json_path(worker_id)
        if not worker_path.exists():
            raise AgentCallError(f"Worker not found: {worker_id}")

        task = self.load_task(task_id)
        task.assigned_worker = worker_id
        self.save_task(task)

        inbox_dir = self.workers_dir / worker_id / "inbox"
        inbox_dir.mkdir(parents=True, exist_ok=True)
        prompt_path = inbox_dir / f"{task_id}.md"
        task_md = (self.task_path(task_id) / "task.md").read_text(encoding="utf-8")
        prompt_path.write_text(render_worker_prompt(task, worker_id, task_md), encoding="utf-8")
        self.append_event(
            "task.assigned",
            task_id=task_id,
            message=f"Task {task_id} assigned to {worker_id}.",
            data={"worker_id": worker_id, "inbox": str(prompt_path.relative_to(self.root))},
        )
        return task

    def list_tasks(self) -> list[Task]:
        self.require_initialized()
        tasks = []
        for path in sorted(self.tasks_dir.glob("task-*/task.json")):
            tasks.append(Task.from_dict(json.loads(path.read_text(encoding="utf-8"))))
        return tasks

    def report_path(self, task_id: str) -> Path:
        return self.task_path(task_id) / "report.md"

    def review_path(self, task_id: str) -> Path:
        return self.task_path(task_id) / "review.md"

    def append_event(
        self,
        event_type: str,
        *,
        task_id: str | None = None,
        run_id: str | None = None,
        message: str | None = None,
        data: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        self.init()
        event = {
            "id": self.next_event_id(),
            "ts": utc_now(),
            "type": event_type,
            "task_id": task_id,
            "run_id": run_id,
            "message": message,
            "data": data or {},
        }
        with self.events_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event, ensure_ascii=False) + "\n")
        return event

    def next_event_id(self) -> str:
        if not self.events_path.exists():
            return "evt-000001"
        count = 0
        with self.events_path.open("r", encoding="utf-8") as handle:
            for line in handle:
                if line.strip():
                    count += 1
        return f"evt-{count + 1:06d}"

    def events(self, task_id: str | None = None) -> list[dict[str, Any]]:
        self.require_initialized()
        items = []
        if not self.events_path.exists():
            return items
        with self.events_path.open("r", encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                event = json.loads(line)
                if task_id is None or event.get("task_id") == task_id:
                    items.append(event)
        return items

    def worker_json_path(self, worker_id: str) -> Path:
        return self.workers_dir / f"{worker_id}.json"

    def save_worker(self, worker: Worker) -> None:
        self.require_initialized()
        worker.updated_at = utc_now()
        self.workers_dir.mkdir(parents=True, exist_ok=True)
        self.worker_json_path(worker.id).write_text(
            json.dumps(worker.to_dict(), indent=2) + "\n",
            encoding="utf-8",
        )
        self.append_event(
            "worker.registered",
            message=f"Worker {worker.id} registered with PID {worker.pid}.",
            data=worker.to_dict(),
        )

    def list_workers(self) -> list[Worker]:
        self.require_initialized()
        workers = []
        for path in sorted(self.workers_dir.glob("*.json")):
            workers.append(Worker.from_dict(json.loads(path.read_text(encoding="utf-8"))))
        return workers


def render_task_md(task: Task) -> str:
    return (
        "---\n"
        f"task_id: {task.id}\n"
        f"title: {task.title}\n"
        f"status: {task.status}\n"
        "---\n\n"
        "# Objective\n\n"
        f"{task.title}\n\n"
        "# Scope\n\n"
        "- Stay inside this workspace unless explicitly instructed.\n"
        "- Write a standardized report to `report.md` when complete.\n\n"
        "# Acceptance Criteria\n\n"
        "- The worker reports what changed.\n"
        "- The worker lists tests or checks performed.\n"
        "- The worker lists blockers if any.\n"
    )


def render_worker_prompt(task: Task, worker_id: str, task_md: str) -> str:
    report_path = f".agentcall/tasks/{task.id}/report.md"
    return (
        f"# AgentCall Task Assignment: {task.id}\n\n"
        f"Worker: `{worker_id}`\n\n"
        "You are participating in an AgentCall SOP test. Work only through the shared workspace artifacts.\n\n"
        "## Required Output\n\n"
        f"Write your final report to `{report_path}` with this frontmatter:\n\n"
        "```yaml\n"
        f"task_id: {task.id}\n"
        "run_id: external\n"
        f"agent: {worker_id}\n"
        "status: done\n"
        "changed_files: []\n"
        "tests: []\n"
        "blockers: []\n"
        "```\n\n"
        "Do not modify unrelated files. Keep the report short.\n\n"
        "## Task\n\n"
        f"{task_md}\n"
    )


def task_status_from_report(report_exists: bool, exit_code: int | None) -> str:
    if report_exists:
        return TaskStatus.REPORT_READY.value
    if exit_code == 0:
        return TaskStatus.FAILED.value
    return TaskStatus.FAILED.value
