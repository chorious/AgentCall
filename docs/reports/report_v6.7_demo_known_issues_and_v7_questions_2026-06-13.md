# AgentCall v6.7 Demo Known Issues And v7.0 Questions

> 日期：2026-06-13
> 性质：实战观察记录，不是 v7.0 plan。
> 依据：v6.7 提交后两轮 6-worker AgentCall PTY demo、6 份代码健壮性反查报告、daemon health/board/session/report 投影。

## Summary

v6.7 后的核心结论比较明确：

- **6 并发 report-only PTY worker 主链已经能跑通。**
- route 能拉起 6 个 worker，prompt gate 能进入 `prompt_submitted`，worker 能写 report，daemon 能以 `daemon_write=high / route_match=high` 接受报告，stop 后 worker 和 lease 能清理到 0。
- 这说明当前最小可用目标已经成立：Codex 可以通过 AgentCall 组织 Claude Code worker 做并发报告/审查型工作。
- 这轮暴露出不少工程债，但大部分不是当前卡点。它们更像 v7.0 方向选择的候选压力源。

我的判断：**当前真正卡体验的不是这些底层边角问题，而是 v7.0 到底要把 AgentCall 变成什么样的协作产品。**

## What The Demo Proved

### 1. 6 并发 report worker 可用

两轮 demo 都能拉起 6 个 daemon-owned Claude Code PTY worker。

第二轮“代码健壮性反查”结果：

- 6/6 route start 成功。
- 6/6 使用 `SharedReport` workspace lease。
- 6/6 prompt gate 进入 `prompt_submitted`。
- 6/6 report 最终落盘。
- 6/6 report accept 成功，confidence 全部为 high。
- stop 后 daemon health 显示：
  - `active_pty_sessions=0`
  - `runtime_workers=0`
  - active owner leases = 0
  - active workspace leases = 0

这证明 v6.7 对“报告型并发 worker”的基础控制闭环是成立的。

### 2. daemon 本体稳定性好于 MCP transport

MCP `agentcall_board` 仍然出现 `Transport closed`。但 daemon HTTP 直连 `/api/routes` 与 `/api/mcp/call` 能稳定完成同一套控制流程。

这说明当前故障边界更偏向：

- Codex plugin/MCP stdio transport 生命周期；
- MCP 重载/热更新；
- MCP bridge 与 daemon 的连接恢复；

而不是 daemon route/session/report 内核。

### 3. prompt gate 比以前好多了，但仍会触发恢复路径

第二轮里观察到一次：

```text
prompt_gate.submit_pending_prompt_sent
```

随后才看到：

```text
hook.UserPromptSubmit
```

这不是失败，因为 worker 最终都进入 working 并写出报告。但它说明真实路径里仍会出现“daemon 补提交 pending prompt”的情况。

该问题暂时不是卡点，但值得保留为 v7.0 的体验观察项：用户会担心“prompt 到底有没有进去”，Codex 也会因此不安。

## Known Issues Found But Not Current Blockers

| 问题 | 当前严重度 | 为什么不是当前卡点 | 什么时候会变成卡点 |
|---|---:|---|---|
| MCP transport `Transport closed` | P1/P0 候选 | daemon HTTP 直连可用，当前测试可绕开 | 如果 v7.0 目标是让普通 Codex session 稳定使用 AgentCall MCP，这就是核心卡点 |
| lease reserve -> install 两段式 TOCTOU | P1/P0 候选 | report-only worker 使用 shared lease，未触发 exclusive 写冲突 | 大规模 coding worker 同 workspace 并发写时会变危险 |
| `renew_owner_lease` 没有业务调用 | P1 | 当前 demo 都是数分钟内完成，不会碰 30 分钟 TTL | 长任务、plan mode、慢审查 worker 超过 30 分钟后会影响 stop/kill/control |
| stop/kill lease 释放依赖 waiter | P1 | 本轮 stop 后资源回收到 0 | Claude Code 子进程挂死、waiter 不返回、Windows kill 失败时会占容量 |
| daemon 结构化错误到 MCP 后可能压成文本 | P1 | 当前直连 daemon 可读结构化 JSON | Codex 只通过 MCP 使用时，会继续出现“只看到 400/Transport closed”的糟糕体验 |
| HTTP API 仍有部分错误统一 400 | P1 | 当前常见 safety lock 已经比以前好 | 调试复杂并发、安全锁、权限问题时，错误原因仍可能不够可操作 |
| Projection/TUI 中文启发式薄弱 | P1/P2 | 当前 report worker 主要靠 hook/report 状态，不靠中文 TUI 猜测 | 如果 v7.0 要强调“可视化/自然交互/权限菜单”，这会变成明显体验问题 |
| debug/raw view 预算不如 summary/events 严格 | P2 | 默认流程不读 raw/debug | Codex 或用户误用 raw/debug 查问题时仍可能拖慢上下文 |
| `runtime-release` 实际构建 debug 产物 | P2/P1 | 本地开发主线接受 debug daemon | 如果要对外发布二进制或 Release 页面，这个语义需要收紧 |
| pytest/Windows ACL 临时目录问题 | closed/观察 | v6.7 已用 `tests/conftest.py` 隔离 `.pytest_tmp_<pid>` | 如果不同安全主体反复跑测试，历史毒目录仍可能需要人工清理 |
| Windows stdout 编码页 `cp932` | P2 | 显式 UTF-8 输出即可绕过 | CLI/脚本如果默认打印中文，会偶发 `UnicodeEncodeError` |

## Issues From Worker Reports Worth Keeping

### A. Error Contract

报告指出 MCP bridge 仍可能把 daemon structured error 压成纯文本。

建议保留为候选 v7.0 问题，但不是现在必须先修：

- 当前 daemon 内部已经有更多结构化错误码。
- 当前直连 daemon 可验证核心流程。
- 但 Codex 正常使用路径最终还是 MCP，如果 MCP 错误不可读，用户会继续觉得“AgentCall 莫名其妙”。

### B. Lease And Long Task Control

报告指出两点是真问题：

- exclusive lease reserve/install 不是单个原子区间；
- owner lease 没有 heartbeat/renew 调用。

我认为这两个不一定要马上修，但它们决定 AgentCall 能否从“短报告并发”走向“长任务工程协作”。

如果 v7.0 仍主打 report/review worker，可以暂缓。

如果 v7.0 主打 long-running coding worker，这两个就是底座问题。

### C. Prompt Gate And Patience

目前 prompt gate 可以工作，但用户/父层仍会感受到：

- prompt 是否真的提交，需要靠 projection 解释；
- worker 安静读代码时，Codex 容易焦虑；
- `submit_pending_prompt_sent` 这种状态对人类仍然偏底层。

这不是 correctness 问题，更像产品体验问题。

### D. TUI / Projection / 中文

v6.7 的正确方向是 projection-first，不默认读 TUI。

但如果 v7.0 继续把 PTY 作为产品核心，TUI 的作用不会消失：

- 权限菜单；
- Claude Code 卡住的视觉证据；
- 计划模式输出；
- 用户现场可视化；
- 中文场景。

所以 TUI cleaner 不必成为 v7.0 主线，但“交互状态结构化”仍可能是主线。

## My Current Read

我不建议 v7.0 继续把所有发现的 issue 都当作待修 bug 排队。

这些问题可以分成三类：

1. **底层正确性债**
   - lease 原子性；
   - lease heartbeat；
   - stop/kill 兜底；
   - typed error 全链路。

2. **Codex 使用体验债**
   - MCP transport/reload；
   - low-friction session summary；
   - prompt gate 可解释性；
   - 权限/菜单交互。

3. **产品方向债**
   - AgentCall 到底是“并发报告 worker”；
   - 还是“长任务 coding worker supervisor”；
   - 还是“Codex/Claude/GPT worker 协作总线”；
   - 还是“项目上下文共享与任务分发层”。

我感觉 v7.0 的核心不是“再修 10 个 P1”，而是要选一个主轴。

## v7.0 Core Questions

下面不是计划，只是给你思考 v7.0 的问题清单。

### 1. v7.0 要解决的是 MCP 可用性，还是 daemon 能力？

如果目标是“普通 Codex 线程稳定调用 AgentCall”，那 v7.0 应该优先解决：

- MCP transport closed；
- MCP restart/rebind；
- plugin-provided MCP server 生命周期；
- daemon token 注入；
- MCP 错误透传。

这会让产品从“我能用 HTTP 直连证明 daemon 很好”变成“任何 Codex session 都能舒服地用 AgentCall”。

### 2. v7.0 要不要正式承认 AgentCall 的第一主场是 report/review worker？

现在最稳的是 report-only shared worker。

如果承认这一点，v7.0 可以主打：

- 并发审查；
- 报告验收；
- 项目上下文包；
- 多 worker 合成；
- Codex 主管低压力。

这条路线不需要马上修所有 coding worker 的 lease 问题。

### 3. v7.0 要不要冲 long-running coding worker？

如果要，必须正面处理：

- owner lease heartbeat；
- exclusive workspace lease 原子性；
- stop/kill 兜底；
- 文件写边界；
- worker 长时间无输出时的状态解释；
- plan/auto mode 生命周期。

这条路线更硬核，但也更容易把 v7.0 拖成系统工程大重构。

### 4. v7.0 要不要做“共享上下文层”？

你前面提到第二层问题：AgentCall 的分享上下文。

这可能比继续修边角更重要。

候选能力：

- route 自动生成 context packet；
- worker 报告统一进入项目知识面；
- Codex 能按项目/任务读取 compact context；
- 多 worker 之间不是直接聊天，而是通过 daemon 管理的 report/context/decision log 协作；
- 每个 worker 不再重新读一遍项目背景。

这条路线会直接改善“子智能体上下文不足”的根问题。

### 5. v7.0 要不要把 TUI 继续降级？

现在事实是：

- TUI cleaner 很难完美；
- projection/report/hook 比 TUI 可靠；
- 但权限菜单、plan mode、现场可视化仍离不开 TUI。

v7.0 可以选择：

- 不再追求 clean TUI 完美；
- 只把 TUI 用作 interaction detector；
- 所有可验收状态都必须来自 hook/report/daemon projection。

## Suggested Decision Frame

我建议你考虑 v7.0 时先回答一句话：

> v7.0 要让 AgentCall 从“能并发拉 Claude Code 干活”变成什么？

我看到三个可选答案：

1. **可靠入口版**
   - 重点修 MCP transport/reload/error UX。
   - 目标：每个 Codex session 都能稳定使用 AgentCall。

2. **协作上下文版**
   - 重点做 context packet、report synthesis、project memory。
   - 目标：Codex 能组织多个 worker，不再靠巨长 prompt 和人工记忆。

3. **长任务工程版**
   - 重点修 lease heartbeat、atomic lease、stop/kill、plan/auto lifecycle。
   - 目标：Claude Code worker 可以跑很久、可中断、可恢复、可治理。

我的倾向：**v7.0 更适合选 1 或 2，不适合一上来选 3。**

原因很简单：当前 6 并发 report worker 已经证明“并发干活”成立；下一步最有产品收益的，不是继续把底层锁修到完美，而是让 Codex 更容易、更稳定、更低上下文成本地使用这套能力。
