# Review / Report 归档

AgentCall 的 review、plan-review、debt audit 与 worker report 归档。
当前入口见 [docs/README.md](../../README.md);版本历史见 [CHANGELOG.md](../../../CHANGELOG.md)。

## OPUS Review 链(ACP / 控制面演进)

按时间与依赖顺序:

| 文档 | 主题 |
|---|---|
| [plan_review_v0.8_OPUS.md](plan_review_v0.8_OPUS.md) | v0.8 route-first + Python 债清理,切分 v0.8a/v0.8b |
| [python_debt_audit_OPUS.md](python_debt_audit_OPUS.md) | Python 只作胶水的判定标准 + 双写/无界审计 |
| [acp_python_vs_rust_OPUS.md](acp_python_vs_rust_OPUS.md) | Python vs Rust ACP 实现对比:传输层对齐、驱动层缺失、Windows `npx` 缺口 |
| [acp_sop_gate_review_OPUS.md](acp_sop_gate_review_OPUS.md) | ACP 重定位为 SOP 模板门:模板四元组 + 权限"牙齿" |
| [plan_review_v2.3_OPUS.md](plan_review_v2.3_OPUS.md) | PTY 两阶段 plan→auto:自铸 session-id、两层禁写、ExitPlanMode 信号 |
| [plan_review_v2.4_OPUS.md](plan_review_v2.4_OPUS.md) | ACP background supervisor:无人值守矛盾、孤儿 boot-id、可恢复降级 |

## v0.6.1 / v0.7 归档

| 文档 | 主题 |
|---|---|
| [OPUS_Review.md](OPUS_Review.md) | 早期 OPUS review |
| [v0.6.1-review.md](v0.6.1-review.md) | v0.6.1 review |
| [v0.7-plan-review-opus.md](v0.7-plan-review-opus.md) | v0.7 plan review |
| [report_v0.6.1_single_writer_closure.md](report_v0.6.1_single_writer_closure.md) | single-writer gap 收口 |
| [report_v0.6.1_daemon_modules.md](report_v0.6.1_daemon_modules.md) | daemon 模块拆分 |
| [report_v0.6.1_hook_daemon_ingest.md](report_v0.6.1_hook_daemon_ingest.md) | hook daemon-first ingest |
| [report_v0.6.1_concurrency_acceptance.md](report_v0.6.1_concurrency_acceptance.md) | 并发验收 |
