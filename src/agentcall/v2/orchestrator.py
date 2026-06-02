from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from ..models import TaskStatus
from ..store import Store
from .context import ContextPacket
from .drivers import AgentDriver
from .reports import ChildReport, ReportStatus, validate_report_contract, validate_scope
from .state import AgentLifecycleState, snapshot_from_report, snapshot_from_spec
from .types import ChildCallSpec, ChildMode, ChildRole


@dataclass
class WorkflowOutcome:
    task_id: str
    accepted: bool
    reports: list[ChildReport] = field(default_factory=list)
    review_report: ChildReport | None = None
    findings: list[str] = field(default_factory=list)


class ParentOrchestrator:
    def __init__(self, store: Store, driver: AgentDriver, reviewer: AgentDriver | None = None) -> None:
        self.store = store
        self.driver = driver
        self.reviewer = reviewer

    def run_bounded_task(
        self,
        *,
        objective: str,
        allowed_paths: tuple[str, ...],
        acceptance_criteria: tuple[str, ...],
        max_turns: int = 1,
    ) -> WorkflowOutcome:
        self.store.require_initialized()
        task = self.store.create_task(objective)
        reports: list[ChildReport] = []

        plan = self._invoke_child(
            task_id=task.id,
            call_number=1,
            role=ChildRole.PLANNER,
            mode=ChildMode.PLAN,
            objective=f"Plan only. Do not modify files. {objective}",
            allowed_paths=allowed_paths,
            acceptance_criteria=acceptance_criteria,
            max_turns=max_turns,
        )
        reports.append(plan)
        plan_findings = self._validate(plan, allowed_paths=(), max_turns=max_turns)
        if plan.changed_files:
            plan_findings.append("planner reported changed_files; plan mode must not modify files")
        if plan_findings:
            return self._fail(task.id, reports, plan_findings, event_type="parent.plan_rejected")

        self.store.append_event(
            "parent.plan_accepted",
            task_id=task.id,
            message="Parent accepted child plan without review artifact.",
            data={"call_id": plan.call_id},
        )

        execute = self._invoke_child(
            task_id=task.id,
            call_number=2,
            role=ChildRole.EXECUTOR,
            mode=ChildMode.EXECUTE,
            objective=objective,
            allowed_paths=allowed_paths,
            acceptance_criteria=acceptance_criteria,
            max_turns=max_turns,
            context={"prior_reports": [plan.to_dict()], "plan_call_id": plan.call_id},
        )
        reports.append(execute)
        findings = self._validate(execute, allowed_paths=allowed_paths, max_turns=max_turns)
        needs_reviewer = bool(findings or execute.risks or execute.open_questions or execute.status != ReportStatus.DONE.value)
        review_report = None

        if needs_reviewer and self.reviewer is not None:
            review_report = self._invoke_review(task.id, execute, findings, allowed_paths, acceptance_criteria, max_turns)
            reports.append(review_report)
            if review_report.status == ReportStatus.DONE.value and not review_report.risks and not review_report.open_questions:
                findings = []
            else:
                findings = findings + review_report.risks + review_report.open_questions

        if findings or execute.status != ReportStatus.DONE.value:
            return self._fail(task.id, reports, findings or ["child did not complete cleanly"], review_report=review_report)

        self.store.update_task_status(task.id, TaskStatus.ACCEPTED.value)
        self.store.append_event(
            "parent.accepted",
            task_id=task.id,
            message="Parent accepted report without writing review.md.",
            data={"call_id": execute.call_id, "reports": [report.call_id for report in reports]},
        )
        return WorkflowOutcome(task_id=task.id, accepted=True, reports=reports, review_report=review_report)

    def _invoke_child(
        self,
        *,
        task_id: str,
        call_number: int,
        role: ChildRole,
        mode: ChildMode,
        objective: str,
        allowed_paths: tuple[str, ...],
        acceptance_criteria: tuple[str, ...],
        max_turns: int,
        context: dict | None = None,
    ) -> ChildReport:
        call_id = f"{task_id}-{role.value}-{call_number:02d}"
        spec = ChildCallSpec(
            task_id=task_id,
            call_id=call_id,
            role=role,
            mode=mode,
            objective=objective,
            workspace=self.store.root,
            allowed_paths=allowed_paths,
            acceptance_criteria=acceptance_criteria,
            max_turns=max_turns,
            context=context or {},
        )
        spec = self._prepare_child_call(spec, runtime=self.driver.name)
        self.store.append_event(
            "child.call_started",
            task_id=task_id,
            message=f"{role.value} child started in {mode.value} mode.",
            data={
                "call_id": call_id,
                "driver": self.driver.name,
                "max_turns": max_turns,
                "context_packet": str(self.store.call_path(task_id, call_id).relative_to(self.store.root)),
            },
        )
        self.store.append_event(
            "agent.state_changed",
            task_id=task_id,
            message=f"{call_id} is running.",
            data=snapshot_from_spec(spec, agent=self.driver.name, state=AgentLifecycleState.RUNNING).to_dict(),
        )
        report = self.driver.invoke(spec)
        self._persist_report(report)
        self.store.append_event(
            "child.report_received",
            task_id=task_id,
            message=f"Child report received: {report.status}.",
            data={"call_id": report.call_id, "agent": report.agent, "status": report.status},
        )
        self.store.append_event(
            "agent.state_changed",
            task_id=task_id,
            message=f"{report.call_id} reported {report.status}.",
            data=snapshot_from_report(report, mode=mode.value, role=role.value).to_dict(),
        )
        return report

    def _invoke_review(
        self,
        task_id: str,
        execute: ChildReport,
        findings: list[str],
        allowed_paths: tuple[str, ...],
        acceptance_criteria: tuple[str, ...],
        max_turns: int,
    ) -> ChildReport:
        assert self.reviewer is not None
        call_id = f"{task_id}-reviewer-03"
        spec = ChildCallSpec(
            task_id=task_id,
            call_id=call_id,
            role=ChildRole.REVIEWER,
            mode=ChildMode.REVIEW,
            objective=(
                "Review the executor report and parent findings. Do not modify files. "
                "Return done with empty risks/open_questions only if the work is acceptable."
            ),
            workspace=self.store.root,
            allowed_paths=allowed_paths,
            acceptance_criteria=acceptance_criteria,
            max_turns=max_turns,
            context={"prior_reports": [execute.to_dict()], "executor_report": execute.to_dict(), "parent_findings": findings},
        )
        spec = self._prepare_child_call(spec, runtime=self.reviewer.name)
        self.store.append_event(
            "reviewer.call_started",
            task_id=task_id,
            message="Parent delegated audit to reviewer child.",
            data={
                "call_id": call_id,
                "driver": self.reviewer.name,
                "context_packet": str(self.store.call_path(task_id, call_id).relative_to(self.store.root)),
            },
        )
        self.store.append_event(
            "agent.state_changed",
            task_id=task_id,
            message=f"{call_id} is running.",
            data=snapshot_from_spec(spec, agent=self.reviewer.name, state=AgentLifecycleState.RUNNING).to_dict(),
        )
        report = self.reviewer.invoke(spec)
        self._persist_report(report)
        self.store.append_event(
            "reviewer.report_received",
            task_id=task_id,
            message=f"Reviewer report received: {report.status}.",
            data={"call_id": report.call_id, "status": report.status},
        )
        self.store.append_event(
            "agent.state_changed",
            task_id=task_id,
            message=f"{report.call_id} reported {report.status}.",
            data=snapshot_from_report(report, mode=ChildMode.REVIEW.value, role=ChildRole.REVIEWER.value).to_dict(),
        )
        return report

    def _prepare_child_call(self, spec: ChildCallSpec, *, runtime: str) -> ChildCallSpec:
        packet = ContextPacket.from_spec(spec, runtime=runtime)
        context = {**spec.context, "context_packet": packet.to_dict()}
        prepared = ChildCallSpec(
            task_id=spec.task_id,
            call_id=spec.call_id,
            role=spec.role,
            mode=spec.mode,
            objective=spec.objective,
            workspace=spec.workspace,
            allowed_paths=spec.allowed_paths,
            acceptance_criteria=spec.acceptance_criteria,
            max_turns=spec.max_turns,
            max_seconds=spec.max_seconds,
            budget_usd=spec.budget_usd,
            context=context,
        )
        prompt = prepared.to_prompt()
        call_dir = self.store.write_call_artifacts(
            spec.task_id,
            spec.call_id,
            input_data={"spec": spec_to_dict(prepared), "driver": runtime},
            prompt=prompt,
            context=packet.to_dict(),
        )
        self.store.append_context_index(
            {
                "task_id": spec.task_id,
                "call_id": spec.call_id,
                "role": spec.role.value,
                "mode": spec.mode.value,
                "runtime": runtime,
                "path": str(call_dir.relative_to(self.store.root)),
            }
        )
        return prepared

    def _validate(self, report: ChildReport, *, allowed_paths: tuple[str, ...], max_turns: int) -> list[str]:
        findings = []
        findings.extend(validate_report_contract(report, max_turns=max_turns).findings)
        findings.extend(validate_scope(report, allowed_paths).findings)
        findings.extend(self._validate_changed_files_exist(report))
        findings.extend(self._validate_context_sufficiency(report))
        if report.status == ReportStatus.DONE.value and not report.tests and report.changed_files:
            findings.append("changed files reported without tests/checks")
        return findings

    def _validate_context_sufficiency(self, report: ChildReport) -> list[str]:
        sufficiency = report.context_sufficiency or {}
        status = str(sufficiency.get("status", "enough_to_act"))
        if status == "enough_to_act":
            return []
        missing = ", ".join(str(item) for item in sufficiency.get("missing", [])) or "unspecified context"
        if sufficiency.get("can_parent_resolve", True):
            return [f"child needs parent-resolvable context before acceptance: {missing}"]
        return [f"child requires human clarification before acceptance: {missing}"]

    def _validate_changed_files_exist(self, report: ChildReport) -> list[str]:
        findings = []
        for changed in report.changed_files:
            path = Path(changed)
            if path.is_absolute():
                findings.append(f"changed file must be workspace-relative: {changed}")
                continue
            if ".." in path.parts:
                findings.append(f"changed file cannot escape workspace: {changed}")
                continue
            if not (self.store.root / path).exists():
                findings.append(f"changed file does not exist in workspace: {changed}")
        return findings

    def _persist_report(self, report: ChildReport) -> None:
        reports_dir = self.store.task_path(report.task_id) / "reports"
        report.write_json(reports_dir / f"{report.call_id}.json")
        if report.call_id.endswith("executor-02"):
            report.write_markdown(self.store.report_path(report.task_id))

    def _fail(
        self,
        task_id: str,
        reports: list[ChildReport],
        findings: list[str],
        *,
        event_type: str = "parent.rejected",
        review_report: ChildReport | None = None,
    ) -> WorkflowOutcome:
        self.store.update_task_status(task_id, TaskStatus.NEEDS_REVISION.value)
        self.store.append_event(
            event_type,
            task_id=task_id,
            message="Parent rejected child lifecycle output.",
            data={"findings": findings, "reports": [report.call_id for report in reports]},
        )
        review_path = self.store.review_path(task_id)
        review_path.write_text(
            "# Review Notes\n\n"
            "Parent found issues that require revision:\n\n"
            + "\n".join(f"- {finding}" for finding in findings)
            + "\n",
            encoding="utf-8",
        )
        return WorkflowOutcome(
            task_id=task_id,
            accepted=False,
            reports=reports,
            review_report=review_report,
            findings=findings,
        )


def spec_to_dict(spec: ChildCallSpec) -> dict:
    return {
        "task_id": spec.task_id,
        "call_id": spec.call_id,
        "role": spec.role.value,
        "mode": spec.mode.value,
        "objective": spec.objective,
        "workspace": str(spec.workspace),
        "allowed_paths": list(spec.allowed_paths),
        "acceptance_criteria": list(spec.acceptance_criteria),
        "max_turns": spec.max_turns,
        "max_seconds": spec.max_seconds,
        "budget_usd": spec.budget_usd,
        "context": spec.context,
    }
