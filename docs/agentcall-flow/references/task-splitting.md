# Task Splitting

Split tasks so AgentCall can run safely in parallel.

## Good Split

- One worker per module, feature slice, or document set.
- Explicit `write_paths` for each Code/Fix worker.
- Read-only Review workers write only to their `report_path`.
- Acceptance criteria are observable and small.

## Avoid

- Multiple Code workers editing the same file set.
- Vague objectives such as "clean this up".
- Asking a worker to both implement and review its own change.
- Starting Fix before Review findings are written.
- Starting Report before Review-driven corrections are complete and Codex has made the closure decision.

## Routing Shape

Use this shape when calling AgentCall:

```text
objective: Implement or review one bounded task.
reference_paths:
- path/to/relevant/source
- path/to/PLAN.md
write_paths:
- path/or/report/the/worker/may/write
report_path: docs/.../worker-name.md
acceptance_criteria:
- Concrete outcome 1.
- Concrete validation or report requirement.
```

For Review, set `write_paths` to the review report path only.
