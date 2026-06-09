# AgentCall Handoff：v5.0 架构重构接力

## 当前状态

工作目录：

```text
E:\Project\AgentCall
```

本轮没有修改源码，只新增/保留了研究与报告材料。

新文档：

- `docs/v5.0-architecture-refresh.md`
- `docs/HANDOFF-next-session.md`

性能报告：

- `docs/reports/perf_audit_mcp_control.md`
- `docs/reports/perf_audit_state_logs.md`
- `docs/reports/perf_audit_pty_io.md`

参考源码：

```text
.agentcall/research/upstreams/zellij
.agentcall/research/upstreams/vector
.agentcall/research/upstreams/crush
```

注意：`.agentcall/` 不是主工程源码，参考仓库不应提交。

## 关键判断

MCP 慢的核心不是 MCP bridge 本身重，而是：

```text
Codex 轻量状态请求
  -> MCP stdio 单通道
  -> daemon board/session
  -> full board / session_summary / logs / routes / transcripts
```

也就是说，轻查询接到了重审计路径上。

当前最重要的架构原则：

```text
Hot path 只读 projection。
Cold path 才读 logs/transcripts/archives。
```

## 需要优先读的文件

AgentCall 当前代码：

- `crates/agentcall-mcp/src/protocol.rs`
- `crates/agentcall-mcp/src/daemon_client.rs`
- `crates/agentcall-mcp/src/tools.rs`
- `crates/agentcall-daemon/src/mcp.rs`
- `crates/agentcall-daemon/src/summary.rs`
- `crates/agentcall-daemon/src/state.rs`
- `crates/agentcall-daemon/src/session.rs`
- `crates/agentcall-daemon/src/hooks.rs`

参考材料：

- `docs/v5.0-architecture-refresh.md`
- `docs/reports/perf_audit_mcp_control.md`
- `docs/reports/perf_audit_state_logs.md`
- `docs/reports/perf_audit_pty_io.md`

参考项目重点：

- Zellij：`zellij-server/src/pty.rs`、`thread_bus.rs`、`background_jobs.rs`
- Vector：`src/topology`、`src/internal_events`、`lib/vector-buffers`
- Crush：`internal/hooks`、`internal/permission`、`internal/session`、`internal/pubsub`

## 推荐开工顺序

### Step 1：建立 MCP timing log

先让问题可测：

- 在 `agentcall-mcp` 中记录每个 `tools/call` 的耗时。
- 字段包括工具名、daemon耗时、响应字节数、序列化耗时、stdout 写回耗时、total。
- 写到 `.agentcall/logs/mcp/recent.ndjson`。

这一步风险小，能立刻区分是 MCP bridge、daemon、序列化还是 stdout 卡。

### Step 2：实现 board attention fast path

目标：

```text
agentcall_board(view=compact, filter=attention)
```

不再调用 full `session_summary()`。

短期可以新增轻量函数：

```text
attention_items_fast()
session_attention_projection()
```

只读 runtime binding、hook-derived status、policy block、last progress。

### Step 3：拆 session_summary 默认行为

默认 `agentcall_session` 只返回 projection 字段：

- liveness_status
- attention_status
- patience
- binding
- report_ready
- pending_interaction

只有 `include=clean_tail` 才清洗 output。

只有 `include=plan` 才读 transcript/plan artifact。

### Step 4：compact board 不再先 full

`view=compact` 必须独立构造，不再先构造 full board。

默认不要返回：

- exited sessions
- legacy sessions
- full routes
- full reports
- recent_events

### Step 5：把 cleanup 从 GET 热路径移走

`board_state()` / `runtime_health()` 不应在 GET 查询路径做重写入或长时间持锁。

可以先保留轻量惰性清理，但重清理迁到 background maintenance。

## 当前 Git 状态注意

上一轮 `git status --short` 显示：

```text
?? docs/reports/
```

也就是说性能报告目录是未跟踪文件。新 session 开始前应重新跑：

```powershell
git status --short
```

确认是否要把 `docs/reports/*.md` 和这两份新文档一起提交。

不要提交：

```text
.agentcall/research/upstreams/**
```

如果 `.agentcall` 未被 ignore，需要先检查 `.gitignore`。

## 风险提醒

- `zellij` 参考仓库在 Windows 下曾遇到长路径 checkout 问题；源码大体可读，不影响参考。
- `agentcall_board(section=sessions)` 当前仍会返回大量 exited/legacy sessions，容易造成大响应。
- Claude worker 完成报告后容易继续执行用户尾部误触发文本，例如 “fix P0 issues”。后续应设计 report complete 后的 stronger stop/finished state。
- 不要把 Python 重新引回 live state 写路径。
- 不要恢复 ACP 作为默认路线。

## 可直接交给下个 Agent 的任务

```text
请基于 docs/v5.0-architecture-refresh.md 和 docs/HANDOFF-next-session.md，
优先实现 P0-3 MCP timing log，然后实现 P0-1 board attention fast path。
要求：
1. 不改 ACP。
2. 不让 Python 写 live state。
3. 不让 compact/attention board 扫 transcript、legacy sessions、full reports。
4. 增加 focused tests。
5. 跑 cargo test -p agentcall-mcp -p agentcall-daemon，必要时 cargo test --workspace。
```

## 一句话总结

AgentCall 下一阶段不是继续扩 worker 功能，而是把 daemon 从“每次查询现场考古”改成“事件进入时更新投影，Codex 查询时只读投影”。
