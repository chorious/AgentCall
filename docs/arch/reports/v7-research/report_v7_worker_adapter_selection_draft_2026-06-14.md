# AgentCall v7 Worker Adapter 选型研究草案

日期：2026-06-14  
作者：Codex synthesis draft  
状态：初稿；AgentCall report worker 因全局 `capacity_exceeded` 未能启动，后续应由 worker 复核。

## Summary

AgentCall 后续如果要支持 Claude Code 以外的执行体，核心不是“哪个 AI coding tool 更会写代码”，而是“哪个执行体最容易被 AgentCall daemon 深度控制和观测”。

当前最值得优先研究的短名单：

1. **Cline SDK / CLI**：最接近 AgentCall 的 deep adapter 需求。它同时提供 IDE/CLI/SDK/headless/JSON/MCP/plugin/multi-agent team 线索，适合做第一候选。
2. **OpenHands SDK / Local GUI / REST**：适合重型、API-first、sandbox-first 的替代运行时研究，但可能过重。
3. **Aider**：适合作为轻量 terminal coding worker baseline，便于 PTY adapter smoke，但状态/生命周期接口较弱。
4. **Gemini CLI / OpenCode / Codex CLI**：适合作为模型和 CLI 能力对照，不宜直接作为首个深度集成目标。
5. **OpenClaw**：更像 personal assistant gateway / memory / workspace 参考，不是 Claude Code 的直接替代。
6. **Roo Code**：历史上接近 Cline，但官方 README 显示 Roo Code Extension 已关闭；只作为历史/社区 fork 参考。
7. **PiAgent**：快速公开搜索未确认到稳定、明确的工程主体；需要用户提供精确 repo / 产品链接后再评估。

## Selection Principle

AgentCall 的 worker adapter 选型应按以下五个平面评分：

| Plane | 问题 |
|---|---|
| Control Plane | 能否启动、提交 prompt、stop/interrupt/kill、设置 cwd、设置 env、控制 timeout？ |
| State Plane | 能否获得 running/waiting/report_ready/blocked/permission 等状态，而不是只靠 TUI？ |
| Safety Plane | 能否限制写路径、权限菜单、命令执行、敏感信息、并发隔离？ |
| Artifact Plane | 能否稳定写 report、暴露 diff/test/log、支持 report contract？ |
| Integration Plane | 是否有 SDK/JSON/headless/API/plugin/hook/MCP，可被 Rust daemon 深度接入？ |

写代码能力是第二层。第一层是能不能成为 daemon-owned worker。

## Candidate Matrix

| Candidate | Fit | Strength | Risk | Initial Verdict |
|---|---:|---|---|---|
| Claude Code | 5/5 | 当前基线；PTY 可视；hook 成熟；真实生产效果好 | 私有实现、TUI 变化、权限交互、状态接口不完整 | 继续主线 baseline |
| Cline | 5/5 | GitHub README 明确有 CLI/headless/JSON、SDK、MCP、plugin、team、Kanban、多模型 | 新 SDK/CLI 需要实测；状态语义可能不同 | **第一候选** |
| OpenHands | 4/5 | SDK、Local GUI、REST API、sandbox、model-agnostic、lifecycle 更系统 | 重，可能和 AgentCall daemon 职责重叠 | **第二候选** |
| Aider | 3/5 | 成熟 terminal pair-programming；Git 工作流强；轻量 | 缺少强状态/权限/hook；更像交互工具 | PTY baseline |
| Gemini CLI | 3/5 | 官方 CLI、MCP、Google Search、大上下文；Zed ACP 生态 | 实战稳定性需测；状态/权限接口不明 | 对照候选 |
| Codex CLI | 3/5 | 强模型，CLI/桌面生态，可能有插件/MCP | Codex 监督 Codex 容易递归；受 OpenAI product surface 限制 | 谨慎研究 |
| OpenCode | 3/5 | terminal-native coding agent 方向值得查 | 需确认具体项目与 API | 待复核 |
| Copilot CLI | 2/5 | 企业生态强 | 深度控制/本地观测可能弱，闭源 | 不优先 |
| OpenClaw | 2/5 | gateway、workspace、shared memory、assistant orchestration 值得学 | 不是 coding PTY worker；可能被服务条款/模型接入限制影响 | 架构参考 |
| Roo Code | 2/5 | modes、MCP、Cline 系血缘 | 官方扩展已 shutdown，维护风险 | 不作为主候选 |
| PiAgent | ? | 用户提及，可能有价值 | 未定位到明确 repo/docs | 等链接 |

## Why Cline Looks Promising

Cline 的公开 README 显示几个 AgentCall 需要的关键点：

- CLI：terminal interactive + fully headless for CI/CD。
- JSON output：可用于 daemon 解析事件，而不是只读 TUI。
- SDK：`@cline/sdk` 可构建自定义 agents / integrations。
- MCP：CLI 可管理 MCP servers。
- Plugins/lifecycle hooks：适合接入 AgentCall policy/logging/audit。
- Team/Kanban：说明它已经考虑多 agent / worktree / dependency chains。
- Rules/Skills：与 AgentCall 的 AGENTS.md / skills 路线相似。

这比“另一个 PTY TUI”更重要。Cline 可能允许 AgentCall 直接从 PTY wrapper 进化到 SDK/JSON adapter。

## Why OpenHands Is Different

OpenHands 更像一个完整 agent runtime，而不是一个简单 worker。它的 Local GUI 有 REST API，SDK 有 sandbox/lifecycle/model routing 等能力。优点是接口更深，缺点是会和 AgentCall daemon 争夺控制面。

适合研究的问题：

- AgentCall 是否应该只做 supervisor，底层 worker runtime 交给 OpenHands？
- OpenHands 的 sandbox/lifecycle 是否能替代我们的 PTY/hook 体系？
- REST/WebSocket projection 是否比 TUI cleaner 更稳定？

短期不建议直接替换 Claude Code，但适合做 v8 级别重型 adapter。

## Why Aider Is Useful But Not Enough

Aider 是成熟 terminal pair-programming 工具，适合快速接入 PTY、验证“非 Claude Code worker 能不能完成 report/coding contract”。但它不是为多 worker daemon supervision 设计的，状态接口、权限模型、report contract 都需要 AgentCall 外包一层。

它适合做 Adapter Conformance Suite 的 baseline：

- 能否启动？
- 能否写 report？
- 能否只改指定文件？
- 能否 stop？
- 能否稳定输出 diff？

## Adapter Conformance Suite

任何候选进入主线前，必须通过这些测试：

| Level | Test | Pass Criteria |
|---|---|---|
| L0 Startup | daemon 启动 worker | 10s 内可观测 running |
| L1 Prompt | 自动提交 prompt | 不需要人工 submit_pending_prompt |
| L2 Report | 写指定 report_path | daemon-observed write + route match |
| L3 Stop | stop/interrupt/kill | 5s 内释放 lease |
| L4 Path Safety | 只能写 write_paths/report/scratch | 越界写被 deny 或由 adapter 阻断 |
| L5 State | 暴露 working/waiting/blocked/report_ready | 不靠 raw TUI |
| L6 Permission | 权限菜单/deny 可结构化处理 | 不反复重试同一 denied action |
| L7 Concurrent | 6 worker 并发 | 不串 cwd/session/report |
| L8 Long Task | 30min 内可持续观测 | heartbeat/progress/report |
| L9 Windows | `D:\guKimi` cwd + `E:\...` target workspace | 路径不混、不乱码 |

## Recommended Next Steps

1. **Cline first smoke**
   - 安装 CLI。
   - 跑 headless JSON report-only 任务。
   - 检查是否能给定 cwd、prompt、report_path。
   - 检查 JSON event 是否能映射到 AgentCall projection。

2. **Aider baseline smoke**
   - 跑同一 report-only 和 single-file coding 任务。
   - 作为最小 PTY adapter 对照。

3. **OpenHands API study**
   - 跑 Local GUI/SDK/REST API。
   - 看能否由 daemon 以 API 创建/查询/停止 agent run。

4. **Gemini CLI / OpenCode follow-up**
   - 只做 CLI conformance，不进入深度集成。

5. **OpenClaw architecture read**
   - 不做 worker adapter，专门研究 memory/workspace/handoff/gateway。

## Open Questions

- PiAgent 指的是哪个具体 repo / 产品？
- Cline SDK 是否能完全 bypass TUI，并提供稳定事件流？
- OpenHands 是否能在 Windows 本机稳定跑，还是更适合 WSL/Linux？
- AgentCall 是否愿意引入多个 adapter crate，还是先做 `worker_adapter` trait + one external candidate？

## Sources

- Cline GitHub: https://github.com/cline/cline
- Roo Code GitHub: https://github.com/RooCodeInc/Roo-Code
- OpenHands GitHub: https://github.com/OpenHands/OpenHands
- Aider GitHub: https://github.com/Aider-AI/aider
- Gemini CLI news / docs ecosystem references
- OpenClaw overview / Claude Code design comparison research
