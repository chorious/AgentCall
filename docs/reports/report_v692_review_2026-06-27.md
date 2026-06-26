# v6.9.2 Review

Date: 2026-06-27

## Review Inputs

- `docs/reports/report_v692_plan_2026-06-26.md`
- Compact board regression evidence: live daemon had 0 active PTY sessions while compact board previously returned hundreds of historical projection workers.
- v6.9.2 implementation diff.

## Findings

| Severity | Area | Finding | Required Fix | Status |
| --- | --- | --- | --- | --- |
| P1 | Compact board | Cold projection rows still needed a current-session gate; `needs_attention` alone could revive historical workers. | Gate compact board rows through daemon live sessions and add audit/test coverage. | Fixed |
| P1 | Report policy | Folder-level report write scope allowed report workers to edit unrelated existing files in report folders. | Enforce report/review created-artifact semantics through file claims and exact report/scratch roots. | Fixed |
| P2 | Coding routes | Real Claude coding workers could still target the canonical workspace without a PR/worktree signal. | Require linked worktree branch for Claude coding routes and project PR closure fields. | Fixed |
| P2 | Skill maintenance | `agentcall-flow` lived outside the repo and release did not force a skill update decision. | Add repo source, generator check, and runtime-release skill decision gate. | Fixed |

## Validation Gaps

No additional fix-now gaps remain after runtime-release validation. `workspace_contract.v1` is still a projection over existing route containment; a future resolver refactor should make it the single path authority.

## Fix Decisions

All review findings were fixed in v6.9.2. The full path-resolver/TaskRun API cleanup is intentionally deferred beyond this patch release.
