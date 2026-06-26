---
name: agentcall-flow
description: Coordinate plan-first AgentCall collaboration for code, review, fix, and report workflows. Use when a task should be split across AgentCall workers, when the user asks for AgentCall parallel collaboration, or when code changes need mandatory Plan, Review, Fix, and PR report phases before final delivery.
---

# AgentCall Flow

Use this skill to run Codex as the coordinator and AgentCall workers as bounded collaborators.

## Core Rules

1. Start with a Plan MD before launching Code work.
2. Do not skip phases: execute the Plan before Review, then create Report only after Review-driven corrections are complete and Codex judges the task ready to close.
3. Use AgentCall for Code, Review, and Fix phases whenever the task is large enough to split.
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

If AgentCall is unavailable, keep the same phase gates and report artifacts, then explain the fallback in the final response.
