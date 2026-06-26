# AgentCall Flow Phases

## Plan

Create the Plan MD before Code workers begin.

The plan must include:

- Goal and scope.
- Out-of-scope items.
- Worker task breakdown.
- Reference paths and allowed write paths per task.
- Required validation.
- Review criteria.
- Report paths for Code, Review, Fix, and final PR report.

Plan is complete only when the intended worker split and acceptance criteria are explicit enough to route AgentCall tasks.

## Code

Launch AgentCall Code workers only after the Plan MD exists.

Code workers should:

- Own one small objective.
- Use disjoint `write_paths` whenever they may edit files.
- Report changed paths and validation results.
- Avoid reverting unrelated changes or other workers' edits.

## Review

Review starts only after the Plan has been executed and the Code phase has produced implementation artifacts or reports to inspect.

Generate at least one Review MD. Review is read-first and should focus on:

- Whether the implementation matches the Plan.
- Correctness and behavioral risk.
- Missing tests or validation.
- Conflicts between worker outputs.
- Documentation/version impacts.
- Whether the Plan acceptance criteria were met.

Review is complete only when implementation findings are recorded and each actionable item is classified as fix-now, defer, or no-change.

## Fix

Fix starts only after Review is complete.

Use AgentCall Fix workers for actionable review items that can be split. Codex should coordinate, inspect, and integrate while Fix is active; avoid direct code writes before AgentCall Fix work is complete unless the user explicitly overrides the flow.

Fix is complete only when every fix-now item has an outcome and validation has been rerun or intentionally waived.

## Report

Report starts only after Review-driven corrections are complete and Codex judges the task ready to close.

Create the PR Report MD as the final closure record:

- What changed.
- Which AgentCall tasks ran.
- Review findings and corrections made.
- Validation commands and results.
- Documentation/version updates.
- Residual risks.

Do not create the PR Report before Review exists and correction outcomes are known.
