# AgentCall Docs

这是 AgentCall 的文档索引。当前主线是 `coding/report worker split`：Codex 通过 AgentCall MCP/daemon 指挥 Claude Code PTY utility workers，daemon 负责状态权威、hook-aware projection、bounded write policy、compact board 和结构化安全锁错误。正常 worker 只剩 `coding` 与 `report` 两类；`read_only` route 线已移除。

## 当前入口

- [项目 README](../README.md)
- [CHANGELOG](../CHANGELOG.md)
- [AGENTS.md](../AGENTS.md)
- [Architecture](architecture.md)
- [About AgentCall](about.md)
- [v6.2 Code Plan](v6.2-code-plan.md)
- [v6.1 Code Plan](v6.1-code-plan.md)
- [v6.0 Code Plan](v6.0-code-plan.md)
- [v5.3 Code Plan](v5.3-code-plan.md)
- [v5.3 Closure Status](reports/v5.3-closure-status.md)
- [v5 Implementation Deep Audit](reports/v5-implementation-deep-audit.md)
- [MCP transport recovery](mcp-transport-recovery.md)

## 当前主线设计

- [v5.2 Code Plan](v5.2-code-plan.md)
- [v5.1 Code Plan](v5.1-code-plan.md)
- [v5.0 Code Plan](v5.0-code-plan.md)
- [v5.0 Architecture Refresh](v5.0-architecture-refresh.md)
- [v4.3 Observability Hygiene](v4.3-observability-hygiene.md)
- [v4.2 Readable TUI Control](v4.2-readable-tui-control.md)
- [v4.0 Plugin Provided MCP](v4.0-plugin-provided-mcp.md)
- [v3.0 PTY Utility Workers](v3.0-pty-utility-workers.md)
- [v3.0 MCP / Daemon Control Plane](v3.0-mcp.md)

## 架构参考

- [rust-daemon-architecture.md](rust-daemon-architecture.md)
- [session-supervisor.md](session-supervisor.md)
- [agentcall-protocol.md](agentcall-protocol.md)
- [agentcall-supervisor-skill.md](agentcall-supervisor-skill.md)
- [agentapi-adapter.md](agentapi-adapter.md)
- [sop-protocol.md](sop-protocol.md)

## Reports

最新实现审计和真实 worker 报告放在 [reports](reports)：

- [v5.3-closure-status.md](reports/v5.3-closure-status.md)
- [v5-implementation-deep-audit.md](reports/v5-implementation-deep-audit.md)
- [review_v52_functional_implementation.md](reports/review_v52_functional_implementation.md)
- [review_v52_code_robustness.md](reports/review_v52_code_robustness.md)
- [perf_audit_mcp_control.md](reports/perf_audit_mcp_control.md)
- [perf_audit_pty_io.md](reports/perf_audit_pty_io.md)
- [perf_audit_state_logs.md](reports/perf_audit_state_logs.md)

## Legacy / Archived Direction

这些文档用于追溯历史设计，不代表当前默认产品能力：

- 计划归档：[arch/plan](arch/plan)
- Review / report 归档：[arch/review](arch/review)

特别注意：

- v2.x ACP 文档是历史方向；v3.0 以后默认路线是 PTY utility workers。
- v1.0 tmux/PTY prototype 已归档。
- v0.x 文档用于理解项目起源和 daemon single-writer 演化。
