from __future__ import annotations

from pathlib import Path

from .orchestrator import WorkflowOutcome
from .workflows import run_small_project_workflow


def run_small_project_simulation(root: Path | str = ".") -> WorkflowOutcome:
    return run_small_project_workflow(root)
