# Review / Report 归档

这里归档 AgentCall 的 review、plan review、debt audit 和 worker report。当前入口请看 [docs/README.md](../../README.md)；版本变化请看 [CHANGELOG.md](../../../CHANGELOG.md)。

## OPUS Review

这些 review 主要用于记录方向调整和风险判断：

| 文档 | 主题 |
|---|---|
| [plan_review_v3.0_OPUS.md](plan_review_v3.0_OPUS.md) | v3.0 PTY-only utility workers：砍掉 ACP 主线、保留 path-scoped PreToolUse deny、压缩控制面 |
| [plan_review_v2.4_OPUS.md](plan_review_v2.4_OPUS.md) | ACP background supervisor：30 分钟上限、heartbeat、orphan 风险 |
| [plan_review_v2.3_OPUS.md](plan_review_v2.3_OPUS.md) | PTY plan gate：plan/auto 双阶段、ExitPlanMode 信号 |
| [acp_sop_gate_review_OPUS.md](acp_sop_gate_review_OPUS.md) | ACP SOP gate：模板化、边界和权限风险 |
| [acp_python_vs_rust_OPUS.md](acp_python_vs_rust_OPUS.md) | Python vs Rust ACP：transport parity、cwd、binding/lifecycle 缺口 |
| [plan_review_v0.8_OPUS.md](plan_review_v0.8_OPUS.md) | v0.8 route-first 与 Python 债务 |
| [python_debt_audit_OPUS.md](python_debt_audit_OPUS.md) | Python 债务审计 |

## v0.6.1 / v0.7 Reports

| 文档 | 主题 |
|---|---|
| [OPUS_Review.md](OPUS_Review.md) | 早期 OPUS review |
| [v0.6.1-review.md](v0.6.1-review.md) | v0.6.1 review |
| [v0.7-plan-review-opus.md](v0.7-plan-review-opus.md) | v0.7 plan review |
| [report_v0.6.1_single_writer_closure.md](report_v0.6.1_single_writer_closure.md) | single-writer gap 收口 |
| [report_v0.6.1_daemon_modules.md](report_v0.6.1_daemon_modules.md) | daemon 模块拆分 |
| [report_v0.6.1_hook_daemon_ingest.md](report_v0.6.1_hook_daemon_ingest.md) | hook daemon-first ingest |
| [report_v0.6.1_concurrency_acceptance.md](report_v0.6.1_concurrency_acceptance.md) | 并发验收 |
