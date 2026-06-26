---
name: agentcall-flow
description: Coordinate plan-first collaboration only when Codex will actually start or supervise AgentCall workers through AgentCall MCP. Use when the user explicitly asks to launch AgentCall parallel workers, split work across AgentCall workers, or continue an active AgentCall worker flow. Do not use for local-only coding, ordinary review/fix work, unavailable AgentCall, or tasks where no AgentCall worker will be started.
---

# AgentCall Flow

Use this skill to run Codex as the coordinator and AgentCall workers as bounded collaborators.

## Activation Boundary

Use this workflow only when AgentCall workers are part of the actual execution. If no AgentCall worker will be started or supervised in this task, do not apply this skill, do not create phase-gate artifacts just to satisfy the workflow, and continue with the normal local Codex workflow.

If this skill is already loaded but AgentCall is unavailable or the user decides not to start workers, stop using the workflow and state that AgentCall flow is skipped because no AgentCall worker is active.

## Core Rules

1. Start with a Plan MD before launching Code work.
2. Do not skip phases: execute the Plan before Review, then create Report only after Review-driven corrections are complete and Codex judges the task ready to close.
3. Use AgentCall for Code, Review, and Fix phases only when workers are actually being launched or supervised.
4. Split work into small, independent tasks with explicit `reference_paths`, `write_paths`, `report_path`, and acceptance criteria.
5. During Fix, Codex coordinates, reviews, and integrates corrections from Review. Codex should avoid writing code directly until AgentCall Fix work is complete unless the user explicitly overrides the flow.
6. Keep the flow repository-local and tool-agnostic. Do not depend on a particular workspace model.

## Phase Gate

Use `references/phases.md` for the required phase order and completion gates.

Use `references/task-splitting.md` before routing AgentCall workers so tasks are small, parallel, and have disjoint write scopes when files may change.

Use templates in `assets/templates/` for:

- `PLAN.md`
- `REVIEW.md`
- `PR-REPORT.md`

## AgentCall Routing

When routing a worker, prefer `mcp__agentcall.agentcall_route` with:

- `objective`: one bounded Code, Review, or Fix objective.
- `acceptance_criteria`: concrete pass/fail bullets.
- `reference_paths`: files or docs the worker should read.
- `write_paths`: paths the worker may modify, or only report paths for read-only Review.
- `report_path`: a Markdown report under the active docs folder.
- `workspace`: the current repository root when needed.

If AgentCall is unavailable, skip this workflow and continue with ordinary local execution; explain the fallback without creating AgentCall flow artifacts.
