from __future__ import annotations

import json
from dataclasses import dataclass, field

from ..store import Store
from .reports import ChildReport


@dataclass
class WorkflowInspection:
    task_id: str
    status: str
    reports: list[ChildReport] = field(default_factory=list)
    states: list[dict] = field(default_factory=list)
    review_exists: bool = False

    def to_lines(self) -> list[str]:
        lines = [
            f"task_id: {self.task_id}",
            f"status: {self.status}",
            f"reports: {len(self.reports)}",
            f"review: {'yes' if self.review_exists else 'no'}",
        ]
        if self.reports:
            lines.append("report_list:")
            for report in self.reports:
                lines.append(f"- {report.call_id}\t{report.agent}\t{report.status}\t{report.summary}")
        if self.states:
            lines.append("state_timeline:")
            for state in self.states:
                lines.append(
                    f"- {state.get('call_id')}\t{state.get('agent')}\t"
                    f"{state.get('role') or '-'}\t{state.get('state')}"
                )
        return lines


def inspect_workflow(store: Store, task_id: str) -> WorkflowInspection:
    task = store.load_task(task_id)
    reports = []
    reports_dir = store.task_path(task_id) / "reports"
    if reports_dir.exists():
        for path in sorted(reports_dir.glob("*.json")):
            reports.append(ChildReport.from_dict(json.loads(path.read_text(encoding="utf-8"))))

    states = [
        event["data"]
        for event in store.events(task_id)
        if event.get("type") == "agent.state_changed"
    ]
    return WorkflowInspection(
        task_id=task_id,
        status=task.status,
        reports=reports,
        states=states,
        review_exists=store.review_path(task_id).exists(),
    )

