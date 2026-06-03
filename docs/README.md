# AgentCall Docs

这里是 AgentCall 的文档索引。README 只保留当前可用入口，版本演进放在 CHANGELOG，历史计划和 review 归档到 `docs/arch`。

## 当前入口

- 项目说明：[../README.md](../README.md)
- 版本历史：[../CHANGELOG.md](../CHANGELOG.md)

## 架构与协议

- [architecture.md](architecture.md)：早期总体架构说明。
- [rust-daemon-architecture.md](rust-daemon-architecture.md)：Rust daemon 架构说明。
- [session-supervisor.md](session-supervisor.md)：session supervisor 设计。
- [sop-protocol.md](sop-protocol.md)：SOP 协作协议。
- [v2.0-architecture.md](v2.0-architecture.md)：Parent/child Agent 协作方向。
- [v3.0-mcp.md](v3.0-mcp.md)：MCP 接入说明。
- [agentapi-adapter.md](agentapi-adapter.md)：agentapi adapter 调研记录。

## 历史实现说明

- [v0.4-orchestration-roadmap.md](v0.4-orchestration-roadmap.md)
- [v0.4-implementation.md](v0.4-implementation.md)
- [v0.5-implementation.md](v0.5-implementation.md)
- [v0.5.1-architecture.md](v0.5.1-architecture.md)
- [v1.0-release-notes.md](v1.0-release-notes.md)
- [v1.0-tmux-pty-archive.md](v1.0-tmux-pty-archive.md)

## 计划与 Review 归档

- 计划归档：[arch/plan](arch/plan)
- Review 与阶段报告归档：[arch/review](arch/review)

## 当前版本提示

当前主线版本是 `v0.7.1`：Hook-aware summary binding、daemon-first hooks、Windows ConPTY DSR 修复、stop 修复、UTF-8 hook 修复和 readable wrapper。
