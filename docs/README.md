# AgentCall Docs

这里是 AgentCall 的文档索引。README 只保留当前可用入口；版本演进放在根目录 [CHANGELOG.md](../CHANGELOG.md)；历史计划和 review 归档在 `docs/arch`。

## 当前入口

- [项目说明](../README.md)
- [About](about.md)
- [版本历史](../CHANGELOG.md)
- [当前 MCP/daemon 控制面](v3.0-mcp.md)
- [MCP transport 恢复](mcp-transport-recovery.md)

## 架构与主线说明

- [architecture.md](architecture.md)
- [rust-daemon-architecture.md](rust-daemon-architecture.md)
- [session-supervisor.md](session-supervisor.md)
- [sop-protocol.md](sop-protocol.md)
- [v2.0-architecture.md](v2.0-architecture.md)
- [v2.2-acp-sop-worker-gate.md](v2.2-acp-sop-worker-gate.md)
- [v3.0-mcp.md](v3.0-mcp.md)
- [agentapi-adapter.md](agentapi-adapter.md)

## 历史实现说明

- [v0.4-orchestration-roadmap.md](v0.4-orchestration-roadmap.md)
- [v0.4-implementation.md](v0.4-implementation.md)
- [v0.5-implementation.md](v0.5-implementation.md)
- [v0.5.1-architecture.md](v0.5.1-architecture.md)
- [v1.0-release-notes.md](v1.0-release-notes.md)
- [v1.0-tmux-pty-archive.md](v1.0-tmux-pty-archive.md)

## 归档

- 计划归档：[arch/plan](arch/plan)
- Review / report 归档：[arch/review](arch/review)

## 当前版本提示

当前主线版本是 `v2.2.0`：AgentCall 已收敛为 Codex 指挥 Claude Code 集群的本地控制面。v2.2 将 ACP 定义为轻量化 SOP worker：route 不再用 estimated size 猜小任务，而是由 daemon 校验 SOP contract，并在 ACP permission 与 hooks 层强制执行 report-only 写入边界。
