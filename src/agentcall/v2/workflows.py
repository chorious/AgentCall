from __future__ import annotations

from pathlib import Path
import shlex

from ..store import Store
from .drivers import AcpClaudeDriver, AgentDriver, FunctionAgentDriver, HeadlessJsonClaudeDriver
from .orchestrator import ParentOrchestrator, WorkflowOutcome
from .reports import ChildReport, ReportStatus
from .types import ChildCallSpec, ChildMode


def build_scripted_small_project_driver(calculator_path: Path) -> FunctionAgentDriver:
    def child_handler(spec: ChildCallSpec) -> ChildReport:
        if spec.mode == ChildMode.PLAN:
            return ChildReport(
                task_id=spec.task_id,
                call_id=spec.call_id,
                agent="simulated-claude-acp",
                status=ReportStatus.DONE.value,
                summary="Plan: fix calculator.add and run a direct Python assertion.",
                next_recommended_action="execute approved plan",
            )

        calculator_path.write_text("def add(a, b):\n    return a + b\n", encoding="utf-8")
        return ChildReport(
            task_id=spec.task_id,
            call_id=spec.call_id,
            agent="simulated-claude-acp",
            status=ReportStatus.DONE.value,
            summary="Fixed calculator.add and verified the result.",
            changed_files=[".agentcall/simulations/small_project/calculator.py"],
            commands_run=[
                "python -c \"from pathlib import Path; ns={}; "
                "exec(Path('.agentcall/simulations/small_project/calculator.py').read_text(), ns); "
                "assert ns['add'](2, 3) == 5\""
            ],
            tests=["direct add(2, 3) == 5 check passed"],
            next_recommended_action="accept",
        )

    return FunctionAgentDriver("simulated-claude-acp", child_handler)


def prepare_small_project(store: Store) -> Path:
    project = store.agent_dir / "simulations" / "small_project"
    project.mkdir(parents=True, exist_ok=True)
    calculator = project / "calculator.py"
    calculator.write_text("def add(a, b):\n    return a - b\n", encoding="utf-8")
    return calculator


def run_small_project_workflow(
    root: Path | str = ".",
    *,
    driver: AgentDriver | None = None,
    reviewer: AgentDriver | None = None,
    max_turns: int = 1,
) -> WorkflowOutcome:
    store = Store(root)
    store.init()
    calculator = prepare_small_project(store)
    driver = driver or build_scripted_small_project_driver(calculator)

    return ParentOrchestrator(store, driver, reviewer=reviewer).run_bounded_task(
        objective=(
            "Fix the small_project calculator add bug. The file is "
            "`.agentcall/simulations/small_project/calculator.py`; change add(a, b) "
            "so add(2, 3) returns 5."
        ),
        allowed_paths=(".agentcall/simulations/small_project",),
        acceptance_criteria=("add(2, 3) returns 5",),
        max_turns=max_turns,
    )


def build_small_project_driver(
    *,
    kind: str,
    calculator_path: Path,
    acp_command: str | None = None,
    claude_bin: str = "claude",
) -> AgentDriver:
    if kind == "scripted":
        return build_scripted_small_project_driver(calculator_path)
    if kind == "headless-json":
        return HeadlessJsonClaudeDriver(claude_bin=claude_bin)
    if kind == "acp":
        command = split_command(acp_command or "npx -y @agentclientprotocol/claude-agent-acp")
        return AcpClaudeDriver(command=command)
    raise ValueError(f"Unknown v2 driver: {kind}")


def run_small_project_workflow_with_driver(
    root: Path | str = ".",
    *,
    driver_kind: str = "scripted",
    acp_command: str | None = None,
    claude_bin: str = "claude",
    max_turns: int = 1,
) -> WorkflowOutcome:
    store = Store(root)
    store.init()
    calculator = prepare_small_project(store)
    driver = build_small_project_driver(
        kind=driver_kind,
        calculator_path=calculator,
        acp_command=acp_command,
        claude_bin=claude_bin,
    )
    return run_small_project_workflow(root, driver=driver, max_turns=max_turns)


def split_command(command: str) -> list[str]:
    return shlex.split(command, posix=False)
