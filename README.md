# AgentCall

当前版本：`v3.0.0`

AgentCall 是一个本地多 Agent 协作控制面，用来让 **Codex 指挥 Claude Code 集群协同工作**。v3.0 把产品面收敛为 PTY-first：daemon 负责拉起受控 Claude Code PTY utility worker，Codex 通过 board、route、session、report 低成本地派工、观察、追问和验收。

## 产品特点

- **Codex 主管，Claude Code 执行**：Codex 不直接长时间盯 raw terminal，而是通过 AgentCall board 和 session summary 管理多个 worker。
- **PTY-first**：`agentcall_route(runtime=auto|pty)` 只启动 Claude Code PTY utility worker。`runtime=acp` 在 v3.0 已移除，不再是可用 runtime。
- **默认 auto mode**：普通 utility worker 使用 `claude --permission-mode auto`。遇到不清楚或高风险任务时，调用方可显式请求 `pty_workflow=plan_then_auto`。
- **Plan gate 可选**：plan workflow 会让 Claude Code 先产出计划并等待批准，Codex 可用 `approve_plan`、`revise_plan`、`start_auto` 继续控制。
- **Hooks 绑定状态**：Claude/Codex hooks POST 到 daemon `/api/hooks/ingest`，并通过 `AGENTCALL_WRAPPER_SESSION` 绑定 wrapper session 与 Claude hook session。
- **Readable wrapper**：daemon 维护 raw output、clean output、llm summary 三层输出；Codex 默认读取 compact board 和 summary。
- **Patience contract**：route/session summary 会返回 `suggested_wait_seconds`、`do_not_retry_before_seconds`、`last_progress_age_seconds` 和 `patience_hint`，提醒 Codex 把 PTY worker 当作异步后台工作，而不是同步函数调用。
- **Daemon single-writer**：live events、claims、sessions、bindings、routes、summary 都由 Rust daemon 统一写入。

## 为什么移除 ACP

ACP 曾经用于 bounded child invocation，但在当前实战中存在三类问题：Codex App 原生侧栏不可见、子智能体生命周期投影不稳定、权限/进度体验难以确认。v3.0 选择先把 Claude Code PTY 集群协作做稳定，ACP 相关 Rust runtime、supervisor 和 Python driver 已从当前实现中删除。

复杂任务、长任务、并行 review、文件实现修改都走 PTY utility worker。需要先计划的任务使用 `pty_workflow=plan_then_auto`。

## 快速启动

构建 Rust 组件：

```powershell
cargo build -p agentcall-daemon -p agentcall-mcp -p agentcall-hook
```

创建本机 daemon 配置：

```powershell
Copy-Item config\agentcall.example.json config\agentcall.local.json
```

编辑 `config\agentcall.local.json`：

```json
{
  "claude_workspace": "D:\\guKimi"
}
```

`config\agentcall.local.json` 不提交到 git。`claude_workspace` 是 Claude Code PTY 的强制启动 cwd，也是 hooks 绑定的基础。route 请求里的 `workspace` 只表示任务目标，不决定 Claude Code 进程启动目录。

启动 daemon：

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

打开 board：

```text
http://127.0.0.1:3293/board
```

## Hooks 配置

安装或刷新 Claude Code hooks：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

安装或刷新 Codex hooks：

```powershell
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

Hooks 行为：

- `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Notification` / `Stop` / `SubagentStop` 都会提交到 daemon。
- `Stop` 是普通 turn end，不当作 checkpoint。
- permission notification 保留为 `needs_permission`。
- 没有可靠 wrapper binding 的 hook 标记为 `unbound`，不靠 cwd、PID、窗口标题或启动顺序猜测归属。

## MCP 工具

当前推荐工具面：

```text
agentcall_daemon
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

典型流程：

1. `agentcall_daemon(action=start)` 确保 daemon 正在运行。
2. `agentcall_board(view=compact, filter=attention)` 查看需要介入的 worker。
3. `agentcall_route(mode=start, runtime=auto, objective=..., allowed_paths=..., acceptance_criteria=...)` 启动 PTY utility worker。
4. 等待 route 返回的 `suggested_wait_seconds`，再用 `agentcall_session(name=..., include=["summary"])` 查看紧凑状态。
5. `agentcall_session_send(action=continue|request_report|revise_plan|approve_plan|start_auto)` 控制 worker。
6. `agentcall_report(action=request|accept)` 请求或接受报告。

## 常用 API

```text
GET  /api/runtime/health
GET  /api/board?view=compact&filter=attention
GET  /api/sessions
GET  /api/sessions/{name}/summary
GET  /api/sessions/{name}/output/clean
POST /api/routes
GET  /api/routes/{id}
POST /api/sessions
POST /api/sessions/{name}/input
POST /api/sessions/{name}/checkpoint
POST /api/sessions/{name}/stop
POST /api/context
POST /api/transcripts/index
POST /api/hooks/ingest
```

## 测试

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
```

## 文档

- [CHANGELOG](CHANGELOG.md)
- [文档索引](docs/README.md)
- [v3.0 PTY Utility Workers](docs/v3.0-pty-utility-workers.md)
- [MCP transport recovery](docs/mcp-transport-recovery.md)
