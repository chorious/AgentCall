# CHANGELOG

## v6.7.1 - Runtime Start And Store-backed Board Patch

- `agentcall_daemon(action=start)` now bounds health probes by the requested `wait_seconds`, avoiding long TCP/read timeout multiplication when the daemon is slow or unhealthy.
- SQLite RuntimeStore now recovers `event_seq` and per-session event sequence from the SQLite `events` table instead of stale NDJSON logs.
- Full board event reads now go through `state.store.get_events(...)`, matching the compact projection path and avoiding direct `events.ndjson` reads on the board hot path.

## v6.7.0 - P0/P1 Control Hardening

- `SessionProjectionV1` now uses an internal `ProjectionStatus` enum while preserving the existing snake_case JSON status strings.
- Replaced FNV-style control/idempotency fingerprints with SHA-256 for control tokens, MCP idempotency keys, store command fingerprints, and policy-denial aggregation keys.
- SQLite event reads now push `session_id`, `event_types`, cursor, and limit into SQL, and hot RuntimeStore paths reuse thread-local SQLite connections.
- Hook PreToolUse path-policy evaluation is split into a pure decision function; repeated policy blocks still flow through daemon events and projection.
- Control errors now carry `ErrorCode` internally while preserving existing `status` fields for MCP callers.
- Worker summaries now derive common session actions from an enum and include a tested worker-state transition table.
- Actor panic cleanup now terminalizes the session, removes the actor handle, releases owner/workspace leases, and runs wrapper cleanup.
- Windows process fallback now attempts `taskkill /T /F` when JobObject control is unavailable, instead of reporting a no-op as successful.
- Lease install/release/acquire paths avoid holding in-memory lease locks while persisting JSON/store updates.
- Added `docs/reports/report_v6.7_p0_p1_closure_2026-06-13.md` to map the v6.6 open P0/P1 issues to v6.7 closure status.

## v6.6.0 - Error Enum, SQLite Writer Fanout, Prompt Gate Cleanup

- daemon safety-lock error codes now flow through a Rust `ErrorCode` enum before being serialized as stable snake_case JSON codes.
- SQLite RuntimeStore can fan out write requests across up to six store writer threads, matching the default worker concurrency cap; JSON remains safety-capped to one writer.
- Runtime health now reports `store_writer_threads`, `configured_store_writer_threads`, and whether the active store supports parallel writes.
- Prompt gate now auto-submits stale `prompt_pending_ack` after a short daemon grace period; `submit_pending_prompt` remains a debug/recovery signal rather than a normal Codex action.
- Split key `projection` and `worker_state` decision paths into smaller reducer/decision helpers, reducing the largest control-flow hotspots without changing public MCP shape.
- Added `docs/reports/report_v6.6_code_optimization_closure_2026-06-13.md` to track which code-audit findings were closed, partially closed, or deferred.

## v6.5.0 - Coding/Report Worker Split

- 移除 `agentcall_route` 的 `read_only` 参数和纯只读 worker 工作线。
- AgentCall 正常 worker 只剩两类：`coding` worker 使用独占 workspace lease 写实现路径；`report` worker 使用共享 report lease，只写 report/scratch。
- `report` worker 仍可产出报告，不再因为“只读”语义拒绝写 `report_path`。
- MCP 推荐 schema 不再暴露 `read_only`；daemon `RouteRequest` 拒绝未知字段，旧调用会明确失败。
- workspace lease 内部共享模式改名为 `SharedReport`，board/health 摘要字段改为 `shared_report`。
- report worker 默认拒绝 `TaskCreate`，防止报告任务漂移为子实现任务。

## v6.3.0 - Structured Safety Errors And Version Alignment

- 统一产品版本口径：Rust crates、Python package、MCP `SERVER_VERSION`、Codex plugin manifest、README/CHANGELOG 全部对齐到 `6.3.0`。
- daemon error response 改为结构化错误对象，包含 `error.code`、`category`、`details`、`hint` 和 `retryable`。
- 常见安全锁使用更准确的 HTTP 状态：workspace/owner lease 冲突为 `409`，容量满为 `429`，缺控制前置为 `428`。
- MCP bridge 不再在非 200 响应头处丢弃 daemon body，会把结构化 daemon 错误透传给 Codex。
- report-only route 使用共享 workspace lease；真正写实现路径的 route 仍保持独占 workspace lease。
- AGENTS 增加版本纪律：发布前必须确认源码版本、plugin 版本、MCP server version 和 live daemon build version 一致。

## v6.2.0 - Worker Closure And Project-Aware Supervisor Loop

- `agentcall_route` 在调用方未传 `report_path` 时自动生成唯一报告路径：`<target_workspace>/.agents/agentcall/<route_id>-<session_name>.md`。
- route/session/report projection 统一返回 report block，包含 `path`、`rel_path`、`abs_path`、`target_workspace`、`report_workspace` 与来源。
- `agentcall_session_send(action=request_report)` 升级为一等状态：写入 `report_requested`、request id、deadline，并在后续 hook/tool progress 后进入 `report_drafting`。
- `agentcall_session` worker state 新增 `report_requested`、`report_drafting`、`report_overdue`、`report_accepted`，并把 `can_wait` / `next_actions` 对齐 report 生命周期。
- `agentcall_board(root|workspace=...)` 支持 target workspace 过滤，并在响应中明确 `daemon_workspace`、`workspace_filter`、`workspace_filter_applied`。
- PTY handoff prompt 注入短工具链上下文；可从目标项目 `.agentcall/toolchain.json` 或 daemon `config/toolchain.local.json` 读取。
- real-worker smoke 新增 `--omit-report-path`，覆盖 daemon-minted report path；6 并发 smoke 验证路径唯一、report accept high、stop 后 lease 清空。

## v6.1.0 - Prompt Commit And Report Projection Closure

- 新增冻结计划 `docs/v6.1-code-plan.md`，v6.1 主线固定为 prompt commit 收敛和 report projection 运行态闭环。
- `submit_pending_prompt` 公开返回改为 `prompt_commit_signal_sent`，并显式包含 `not_completed=true`、`awaiting_hook=UserPromptSubmit`、attempt id 和 ack deadline。
- Prompt gate 状态收敛为 `prompt_pending_ack`、`prompt_missing`、`commit_signal_sent`、`prompt_submitted`、`prompt_commit_unacknowledged`、`prompt_commit_failed`，不再从运行代码发出旧的 pending/ack 名称。
- `UserPromptSubmit`、工具进展和 report write 都会关闭 prompt gate，避免真实 worker 已开始工作但 projection 仍提示 prompt 未提交。
- `agentcall_session_send` 在 prompt gate 未闭合时拒绝普通 `send/continue`，要求使用 `submit_pending_prompt` 或继续等待，不再把 supervisor 文本排队到 worker 后面。
- `agentcall_report(action=accept)` 拆分 `confidence.overall/artifact/daemon_write/route_match`；`overall=high` 需要 daemon-observed report/write evidence。
- `/api/runtime/health` 增加 build identity，`python agentcall.py verify-runtime-build` 可验证 daemon 已运行当前构建产物。

## v6.0.0 - Slim Codex Control Plane

- 新增冻结计划 `docs/v6.0-code-plan.md`，v6.0 主线收敛为 `board -> route -> session -> next action -> report`。
- 新增 `worker_state.rs` / `prompt_gate.rs`，把 route、projection、prompt gate、report 和 control 信息归一为 Codex-facing worker state。
- `agentcall_session` 默认 summary 改为 schema v2：只返回 `state`、`why`、`can_wait`、`next_actions`、report、control 和 debug refs，不再默认混入 raw terminal/events/tool payload。
- `agentcall_board(view=compact)` 默认只展示 live daemon workers 和 attention，不再把 historical sessions 伪装成 live workers。
- `agentcall_route` 推荐 schema 收窄为 PTY-first 启动入口；`runtime`、`mode`、SDK、估算字段、plan workflow 等调试/兼容字段不再出现在推荐工具面。
- `agentcall_session_send` 推荐 schema 移除 caller-supplied lease/precondition/idempotency 字段，新增 `submit_pending_prompt` 作为 prompt stuck 的产品化恢复动作。
- Projection 不再把 `SessionStart`、`pty.input_sent`、`command.accepted/completed` 当成 task started；缺 `UserPromptSubmit` 会进入 prompt gate 状态而不是静默 `working/none`。
- Hook raw payload 在写 event 前做敏感字段/大文本 redaction，降低命令、环境变量、prompt 和 Write 内容泄露到 compact/debug 常用路径的风险。

## v5.3.0 - Worker Projection Gates Checkpoint

- README、docs 索引、About 和 plugin manifest 更新到 v5.3 checkpoint 口径。
- 新增 `AGENTS.md`，给 Codex / Claude Code worker / 其他 agent 提供项目协作、验证和归档规则。
- `projection_last_session_seq` precondition 现在会在命令进入 actor 前校验，stale command 会被拒绝并记录 `command.rejected_precondition`。
- Hook 写入 route `report_path` 会更新 route/session projection 到 `report_ready`，降低 worker 已交付但 Codex 仍继续等待的问题。
- read-only route 默认拒绝 `TaskCreate`，避免审查 worker 漂移成实现 worker。
- PTY writer/reader 错误会写入 projection-visible failure event。
- Summary/MCP 热路径改用 runtime binding hot flags，减少对大 events log 的线性扫描。
- Hook recent logs 单文件上限降到 1MB。
- v0.x/v1.x/v2.x 历史计划归档到 `docs/arch/plan`，v5 worker review reports 收敛到 `docs/reports`。
- 当前仍是 checkpoint：actor panic guard、control/output channel isolation、stop/kill 语义拆分等 open gates 见 `docs/reports/v5.3-closure-status.md`。

## v4.3.0 - Recent-first Logs And Stale Session Cleanup

- 事件写入切到 `.agentcall/events/recent.ndjson`，并按大小轮转到 archive；board/session 默认只读 recent hot log。
- Hook 分类日志切到 `.agentcall/logs/hooks/<HookEvent>/recent.ndjson`，单文件超过上限自动归档。
- `PostToolUse` / `PostToolBatch` 大 payload 外置到 `.agentcall/artifacts/hooks/...`，event 内只保留摘要和 artifact 元数据。
- `active_sessions.json` 中 5 分钟无更新且不再由 live daemon 拥有的历史/unbound session 会被清理；孤儿 pending supervisor instructions 同步清理。
- Codex-facing patience 统一到 60 秒，stall 阈值调整为 300 秒，减少急躁轮询。
- `python agentcall.py logs doctor` 和 `python agentcall.py sessions cleanup` 提供日志体积与 stale session 快速诊断入口。

## v4.2.0 - Bounded Write And Policy Block Attention

- PTY route 默认生成 session scratch，并在 containment 中暴露 `writable_paths`、`scratch_path`、`bash_write_policy`。
- 写工具可写 `report_path`、session scratch 和显式 `allowed_paths`；Bash 首版仍保持 readonly-only。
- 重复 policy deny 聚合到 `policy_denials.json`，summary/board 显示 `blocked_by_policy`，不再提示 Codex继续耐心等待。
- policy deny loop 会通过 hook context 注入一次纠偏提示，要求 Claude 不要机械重试同一被拒动作。
- `agentcall_route` 增加 `read_only` 参数；显式 read-only route 不会自动授予 writable scratch。
- Board attention 展示 policy block 类别、次数和 deny reason；`doctor` 增加 scratch 目录提示。
- `.gitignore` 增加本地 Anthropic/router 启动文件忽略规则，避免把本机路由 fork/脚本提交到 main。

## v4.1.1 - Scripted Diagnostics And Release Checks

- 新增 `python agentcall.py ...` 总入口，覆盖 `doctor`、`install-hooks`、`release-check`、`daemon-health`、`paths`。
- `doctor` 检查 repo/config/cargo/python/node/plugin/Claude hooks/daemon/git，并在缺 hook event、缺 cargo、daemon timeout 时给出定位提示。
- `release-check` 固化提交前常用校验：Python compile、Board JS syntax、plugin validation、Cargo workspace tests、pytest、`git diff --check`。
- README 增加脚本入口说明，降低 Codex/Claude/human 重复记命令的上下文成本。

## v4.1.0 - Bilingual Release And Board Refresh

- README 改为中英文双语，明确 AgentCall 的产品定位、MCP/plugin 安装方式、hooks 与 `claude_workspace` / cwd 的关系。
- 插件 release 版本更新到 `4.1.0`。
- Board UI 切到 compact daemon board 数据，优先展示 live sessions、attention、routes 和 reports，减少读取大事件日志的成本。
- 根目录 review/report 文档归档到 `docs/arch/review`，保持项目根目录干净。

## v4.0.1 - AgentCall MCP Smoke Clarification

- 插件 skill 明确：不要用 `tool_search agentcall` 判断 AgentCall 是否可用。
- AgentCall MCP 可用性验收改为直接调用 `agentcall_daemon(action="status")`。
- 修正派生线程反复误判 “AgentCall 不存在” 的操作路径。

## v4.0.0 - Plugin-Provided MCP

- 新增 repo 内 Codex plugin：`plugins/agentcall`。
- 插件通过 `.codex-plugin/plugin.json` 声明 `mcpServers`，通过 `.mcp.json` 暴露 AgentCall MCP server。
- 新增 `skills/agentcall/SKILL.md`，把 AgentCall 默认协作流程、耐心策略、禁止 HTTP fallback 等规则交给 Codex。
- 新增 `.agents/plugins/marketplace.json`，允许通过 `codex plugin marketplace add E:\Project\AgentCall` 安装本地 marketplace。
- README 改为干净 UTF-8，明确 v4.0 的产品定位、hooks 配置、daemon config 和 plugin-provided MCP 安装方式。

## v3.0.1 - Daemon Hook Hardening

- `SessionStart` / `UserPromptSubmit` hook 返回 `context_injection`，把 AgentCall board discipline 注入 worker 上下文。
- HTTP body 增加 1 MiB 上限，超限返回 `413 Payload Too Large`。
- WebSocket frame 增加 64 KiB 上限，超限记录 `ws.frame_too_large` 并断开。
- daemon 增加单实例 runtime lock，避免多个 daemon 同时写共享状态。
- hook event 按类型写入 `.agentcall/logs/hooks/{HookType}.ndjson`。
- `hook.PostToolUse` 的大 stdout/stderr 会写入 artifact，事件里只保留压缩摘要。
- 新增 `scripts/align_hook_logs.py`，用于把旧 events 重新整理成分类 hook 日志。

## v3.0.0 - PTY Utility Workers

- 产品面收敛为 PTY-first：`agentcall_route(runtime=auto|pty)` 启动 Claude Code PTY utility worker。
- 移除 ACP 作为默认 runtime；`runtime=acp` 不再作为当前主线能力。
- PTY worker 默认 `permission-mode auto`；复杂任务可显式使用 `pty_workflow=plan_then_auto`。
- route 增加 `worker_kind=utility`、`containment`、prompt submit gate，降低 session 已创建但任务未送达的误判。
- hook policy 增加 PTY path enforcement，带 `allowed_paths` 的 PTY route 会参与写入边界判断。
- MCP 工具面收敛为 `agentcall_daemon / board / route / session / session_send / report`。
- 新增 [docs/v3.0-pty-utility-workers.md](docs/v3.0-pty-utility-workers.md)。

## v2.4.0 - ACP Background Supervisor

- ACP route 改为后台 supervisor 模式，默认 30 分钟 hard timeout。
- 新增 ACP invocation state、heartbeat、checkpoint_due、capacity cap 与 orphan 标记。
- 该路线在 v3.0 被移除出当前产品能力。

## v2.3.0 - PTY Plan Gate

- PTY route 支持 `plan_then_auto`。
- `ExitPlanMode` hook 会把 route 标记为 `plan_ready`。
- `agentcall_session_send` 增加 `approve_plan`、`start_auto`、`revise_plan`。

## v2.2.0 - ACP SOP Worker Gate

- ACP 收敛为 SOP worker，首版只允许 read/report 类模板。
- 新增 template-aware permission policy 与 report contract。
- 该路线在 v3.0 被移除出当前产品能力。

## v2.1.0 - ACP Child Lifecycle Binding

- ACP route 注入 `AGENTCALL_WRAPPER_SESSION` 并投影 child lifecycle。
- 修正 ACP cwd 与 PTY 一致，均使用 daemon local config 的 `claude_workspace`。

## v2.0.0 - Codex-Controlled Claude Code Cluster

- AgentCall 定位为让 Codex 指挥 Claude Code 集群协作的本地控制面。
- README 改为产品说明，明确 route-first MCP、hook-aware binding、file claim、readable wrapper 和 daemon single-writer。

## v0.8.1 - Stable MCP Bridge

- MCP stdio bridge 收敛为稳定外壳，工具实现由 daemon 动态提供。
- 新增 `agentcall_daemon(action=status|start)` bootstrap。

## v0.8a - Route-First MCP

- MCP 默认流程收敛为 `board -> route -> session/report`。
- `agentcall_delegate*` 从默认工具面移除。

## v0.7.1 - Hook-Aware Summary Binding

- 新增 wrapper session 与 Claude hook session 的 runtime binding。
- `session_summary` 拆出 `liveness_status`、`attention_status`、`report_ready`。

## v0.7 - Readable Wrapper + Low-Friction Codex Control

- PTY 输出拆成 `raw_output`、`clean_output`、`llm_summary`。
- 修复 UTF-8 chunk 边界解码，新增 `decode_health`。

## v0.6.1 - Close Daemon Single-Writer Gap

- Claude/Codex hook 客户端改为 daemon-first：POST `/api/hooks/ingest`。
- Python hook ingest 降级为 legacy/fallback。

## v0.6 - Concurrent Trust Base

- 建立 daemon single-writer 模型。
- File claim 区分 write claim 与 read observe。

## v0.5.1 - Codex Hooks And MCP

- Codex 接入 AgentCall hooks/preflight。
- Rust MCP 暴露 board、route、claims、reports 等能力。

## v0.5 - Claude Code Handoff Visualization

- 新增 Claude Code hooks、file claim、transcript index 与 PTY handoff board。

## v0.4 - Control Plane Origin

- 建立 AgentCall 控制面原型，探索 agents-as-tools 与 Claude Code handoff 两条路线。
