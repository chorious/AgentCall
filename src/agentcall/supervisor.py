from __future__ import annotations

import json
import subprocess
from pathlib import Path

from .models import RunRecord, TaskStatus, utc_now
from .store import Store, task_status_from_report


class Supervisor:
    def __init__(self, store: Store) -> None:
        self.store = store

    def run(self, task_id: str, command: list[str]) -> RunRecord:
        self.store.require_initialized()
        self.store.load_task(task_id)
        run_id = self.store.next_run_id(task_id)
        run_dir = self.store.task_path(task_id) / "runs" / run_id
        run_dir.mkdir(parents=True, exist_ok=False)

        stdout_path = run_dir / "stdout.log"
        stderr_path = run_dir / "stderr.log"
        record = RunRecord(id=run_id, task_id=task_id, command=command, status="starting")
        self._write_record(run_dir, record)

        self.store.update_task_status(task_id, TaskStatus.RUNNING.value)
        self.store.append_event(
            "run.starting",
            task_id=task_id,
            run_id=run_id,
            message="Worker command is starting.",
            data={"command": command},
        )

        with stdout_path.open("wb") as stdout_handle, stderr_path.open("wb") as stderr_handle:
            process = subprocess.Popen(
                command,
                cwd=self.store.root,
                stdout=stdout_handle,
                stderr=stderr_handle,
                shell=False,
            )
            record.pid = process.pid
            record.status = "running"
            self._write_record(run_dir, record)
            self.store.append_event(
                "run.started",
                task_id=task_id,
                run_id=run_id,
                message=f"Worker PID {process.pid} started.",
                data={"pid": process.pid},
            )
            exit_code = process.wait()

        record.exit_code = exit_code
        record.status = "completed" if exit_code == 0 else "failed"
        record.completed_at = utc_now()
        self._write_record(run_dir, record)

        report_exists = self.store.report_path(task_id).exists()
        next_status = task_status_from_report(report_exists, exit_code)
        self.store.update_task_status(task_id, next_status)
        self.store.append_event(
            "run.completed",
            task_id=task_id,
            run_id=run_id,
            message=f"Worker exited with code {exit_code}.",
            data={
                "exit_code": exit_code,
                "report_exists": report_exists,
                "stdout": str(stdout_path.relative_to(self.store.root)),
                "stderr": str(stderr_path.relative_to(self.store.root)),
            },
        )
        return record

    def _write_record(self, run_dir: Path, record: RunRecord) -> None:
        (run_dir / "run.json").write_text(
            json.dumps(record.to_dict(), indent=2) + "\n",
            encoding="utf-8",
        )
