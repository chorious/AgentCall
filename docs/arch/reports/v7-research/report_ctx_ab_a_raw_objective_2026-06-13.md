# AgentCall v7 向 Claude Code PTY Worker 暴露上下文的 API / 数据模型边界建议

> 任务：A/B/C 实验组 A —— 无预编译背景，独立审阅 AgentCall 仓库，提出 v7 应如何向 Claude Code PTY worker 暴露上下文。
> 日期：2026-06-13
> 作者：report worker（无预编译背景）

## 1. 审阅范围（files_read）

- 控制面与入口：
  - `docs/README.md`
  - `docs/architecture.md`
  - `AGENTS.md`
  - `docs/rust-daemon-architecture.md`
- 核心实现：
  - `crates/agentcall-daemon/src/routes.rs`
  - `crates/agentcall-daemon/src/session.rs`
  - `crates/agentcall-daemon/src/prompt_gate.rs`
  - `crates/agentcall-daemon/src/hooks.rs`
  - `crates/agentcall-daemon/src/projection.rs`
  - `crates/agentcall-daemon/src/worker_state.rs`
  - `crates/agentcall-daemon/src/runtime_pty.rs`
  - `crates/agentcall-daemon/src/config.rs`
- 工具面：
  - `crates/agentcall-mcp/src/tools.rs`
- 近期 v7 相关研究（作为现场背景，非预编译指令）：
  - `docs/reports/report_v6.7_demo_known_issues_and_v7_questions_2026-06-13.md`
  - `docs/reports/report_v7_shared_context_research_2026-06-13.md`
  - `docs/reports/report_v7_worker_brief_research_2026-06-13.md`

## 2. 基本假设（assumptions）

1. **v7 不改变 AgentCall 的根本分工**：Codex 是 supervisor，Rust daemon 是状态权威，Claude Code 只是 daemon 拥有的 PTY utility worker。上下文暴露必须强化这条边界，而不是让 worker 绕过 supervisor。
2. **PTY worker 是“有边界的一次性任务执行者”**：它不需要长期记忆，也不需要和其他 worker 直接聊天；它需要知道“这次任务做什么、能碰什么、不能碰什么、怎么交付、怎么失败”。
3. **projection-first 仍是默认策略**：Codex 通过 compact board/session summary 监督 worker，不默认读取原始 PTY/TUI 输出。
4. **worker 可写的上下文是有限的**：worker 只能产生 report 和 scratch 产物；项目级记忆回流必须由 daemon/supervisor 在 report accept 后提炼写入。
5. **MCP 工具面应保持精简**：v7 新增的上下文能力尽量复用 `agentcall_route` / `agentcall_session` / `agentcall_report`，避免新增一堆工具。
6. **没有预编译背景**：本报告不依赖任何提前写好的 v7 plan，所有结论均来自对代码和文档的现场阅读。

## 3. 当前现状的边界问题

当前代码里已经出现了几类上下文，但边界不够清晰：

| 上下文类型 | 当前位置 | 问题 |
|---|---|---|
| `context_packet` | `routes.rs:create_context` / `.agentcall/tasks/<task>/calls/<call>/context.json` | 只包含 route 请求字段的原始投影，没有“任务简报”语义，也没有和 worker kind 绑定。 |
| handoff prompt | `routes.rs:pty_prompt` | 把 control envelope、toolchain、objective、containment、acceptance criteria 全塞进一段自然语言 prompt，难以审计、版本化和按 worker kind 裁剪。 |
| hook context injection | `hooks.rs:context_injection` | 通过 Claude hook 的 `additionalContext` 注入 supervisor update / policy block，是运行时补丁，不是结构化契约。 |
| worker state projection | `worker_state.rs` / `projection.rs` | 面向 Codex 消费，不直接暴露给 worker；worker 只能看到 handoff prompt 和自己的 PTY 输出。 |
| project memory | 无 | 不存在；worker 每次启动都靠 Codex 重新组织背景。 |

核心矛盾：worker 收到的上下文是一份“大 prompt”，而不是 daemon 可验证、可审计、可按任务类型编译的数据对象。

## 4. 建议的 API / 数据模型边界

### 4.1 四层上下文模型

```text
┌─────────────────────────────────────────────────────────────┐
│  Layer 4: Codex Supervisor Context                          │
│  - 完整对话、项目计划、用户意图、跨 route 决策               │
│  - 不直接暴露给 worker                                      │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: Project Memory （daemon-owned，长期）              │
│  - accepted report 提炼出的可复用事实                        │
│  - 决策记录、已知风险、工具链事实                            │
│  - worker 只读，由 daemon 在编译 brief 时按需选取            │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: Worker Brief （route 级，daemon 编译）             │
│  - 本次任务最小工作契约                                      │
│  - JSON schema + Markdown 可读版本                           │
│  - handoff prompt 只引用 brief 路径                         │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Runtime Injection （hook 运行时补丁）               │
│  - supervisor instruction、policy block、权限菜单提示         │
│  - 仍然保留，但只作为 brief 的补充，不是主要信息来源          │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 Worker Brief 数据模型

建议把 `context_packet` 升级为 `WorkerBrief`，作为 route 启动时 daemon 编译的产物：

```json
{
  "brief_id": "brief-route-1679261",
  "route_id": "route-1679261",
  "schema_version": 1,
  "worker_kind": "coding",
  "target_workspace": "E:/Project/AgentCall",
  "claude_cwd": "D:/guKimi",
  "objective": "...",
  "supervisor_intent": "...",
  "scope": {
    "write_paths": ["src/agentcall", "tests"],
    "reference_paths": ["docs/architecture.md", "AGENTS.md"],
    "scratch_path": ".agentcall/workspaces/route-1679261",
    "report_path": ".agents/agentcall/route-1679261-report.md"
  },
  "rules": {
    "must_follow": [
      "只修改 listed write_paths 中的文件",
      "report_path 是相对 target_workspace 的路径，实际写入 report_abs_path"
    ],
    "must_not_do": [
      "不要调用 TaskCreate 生成子 worker",
      "不要写 .agentcall/state 或修改 daemon 状态文件"
    ]
  },
  "facts": [
    {
      "kind": "toolchain",
      "fact": "使用 cargo test --workspace 运行 Rust 测试",
      "source": "accepted-report:route-1679000",
      "confidence": "high"
    }
  ],
  "repo_navigation": {
    "important_paths": ["crates/agentcall-daemon/src", "crates/agentcall-mcp/src"],
    "do_not_scan_unless_needed": ["node_modules", "target"]
  },
  "contracts": {
    "output": {
      "required_report": true,
      "changed_files_required": true,
      "tests_required": "if code changed"
    },
    "failure": {
      "when_blocked": "写 blocker report 而不是重试",
      "permission_denied": "报告具体命令/路径/原因"
    }
  },
  "sources": [
    {"path": "AGENTS.md", "included_sections": ["Worker Discipline"]},
    {"path": "docs/architecture.md", "included_sections": ["Current Runtime Circuit"]}
  ],
  "budget": {
    "max_prompt_chars": 12000,
    "estimated_chars": 3200
  }
}
```

对应 Markdown 版本放在同一目录供 worker 阅读：

```textn.agentcall/briefs/<route_id>.json
.agentcall/briefs/<route_id>.md
```

### 4.3 Project Memory 数据模型

项目级长期记忆，只在 report accept 后由 daemon 写入：

```text
.agentcall/context/project_memory.ndjson
.agentcall/context/decisions.ndjson
```

条目示例：

```json
{
  "entry_id": "fact-abc",
  "entry_type": "toolchain_fact",
  "fact": "cargo test --workspace 是此仓库标准验证命令",
  "source_report": ".agents/agentcall/route-1679000-report.md",
  "source_route": "route-1679000",
  "applies_to": {"paths": ["crates/agentcall-daemon"], "tags": ["rust", "testing"]},
  "confidence": "high",
  "created_at": "2026-06-13T..."
}
```

### 4.4 Runtime Injection 边界

保留 `hooks.rs` 的 `context_injection`，但语义收窄：

- 只注入 brief 里没有的**运行时变化**：supervisor instruction、policy block、checkpoint 请求。
- 注入内容必须在 `pending_supervisor_instructions.json` / `policy_denials.json` 里留下结构化记录。
- 不能把 brief 该包含的内容推迟到运行时补丁里。

### 4.5 MCP / Daemon API 边界

MCP 工具面保持精简，建议只扩展 `agentcall_route` 的返回和 `agentcall_session` 的 summary：

- `agentcall_route` 返回中增加：
  - `result.brief_path`
  - `result.brief.sources_count`
  - `result.brief.estimated_chars`
- `agentcall_session(summary)` 中增加：
  - `brief_id`
  - `context.sources`
  - `context.budget`

Daemon HTTP 可以更丰富，但 Codex 默认不走这里：

```text
GET  /api/briefs/{route_id}
POST /api/briefs/{route_id}/recompile
GET  /api/context/project_memory
POST /api/context/project_memory/append
GET  /api/context/decisions
POST /api/context/decisions
POST /api/context/synthesize
```

## 5. 应该暴露 / 不应该暴露给 Worker 的上下文

### 5.1 应该暴露

| 类别 | 内容 | 原因 |
|---|---|---|
| 任务目标 | `objective`、`supervisor_intent` | worker 必须知道要解决什么问题。 |
| 作用域 | `write_paths`、`reference_paths`、`scratch_path`、`report_path`（含 abs） | 明确读写边界，避免 policy deny。 |
| 行为规则 | `must_follow`、`must_not_do` | 把 AGENTS.md / 项目规则裁剪成任务相关条目。 |
| 可复用事实 | 来自 ProjectMemory 的 toolchain/decision/known_issue | 避免 worker 重复探索已验证事实。 |
| 仓库导航 | 重要路径、可忽略路径 | 减少 worker 无目的扫描。 |
| 输出契约 | report 要求、测试要求、失败报告要求 | 让 worker 知道何时停止、如何交付。 |
| 工具链提示 | 本地工具路径、已知命令、镜像配置 | 减少 PATH 缺失导致的阻塞。 |

### 5.2 不应该暴露

| 类别 | 内容 | 原因 |
|---|---|---|
| Codex 完整对话 | 父层思考、探索、反复讨论 | 噪音大，易造成上下文爆炸和行为漂移。 |
| 原始事件日志 | `.agentcall/events/*.ndjson` | 应通过 projection 消费，不是 prompt 素材。 |
| 其他 worker 的进行中状态 | 未 accept 的报告、未关闭的 route | 避免 worker 基于不稳定状态做决策。 |
| 未验证的 worker 自述 | 未经 daemon evidence 的成功声明 | 防止幻觉级联。 |
| 完整 AGENTS.md / README | 全文注入已被研究指出可能降低成功率 | 只抽取任务相关规则。 |
| 密钥 / token / 环境变量 | API key、daemon token | 安全边界，只通过环境变量或 Claude hook 可控注入。 |
| daemon 内部状态文件 | routes.json、file_claims.json 等 | worker 不应读写 daemon 状态。 |

## 6. 风险（risks）

1. **Brief 编译器本身成为复杂源**：如果 v7 追求“自动选择最相关上下文”，很容易滑向 RAG / embedding / 向量库，增加维护和调试成本。
2. **规则裁剪过度**：`must_follow` / `must_not_do` 如果漏掉关键约束，worker 会违反项目纪律。
3. **ProjectMemory 污染**：如果允许 worker 直接写项目记忆，会出现自我强化幻觉；必须限制为 report accept 后的 daemon 提炼。
4. **与现有 context_packet 的兼容性**：routes.rs 已经生成 `context_packet` 并存入 `.agentcall/tasks/...`；v7 应明确迁移路径，避免两个并行的上下文文件系统。
5. **MCP transport 不稳定**：即使上下文模型设计正确，如果 MCP 仍频繁 `Transport closed`，Codex 还是无法稳定消费。这是 v7 需要并行考虑的体验问题。
6. **Plan mode 生命周期复杂**：v7 如果支持 plan_then_auto，brief 需要分 plan-phase 和 auto-phase 两个版本，否则 plan 阶段会拿到执行阶段才需要的规则。

## 7. 开放问题（open questions）

1. WorkerBrief 的编译应该由 daemon 独立完成，还是允许 Codex 在 route 前提供一份“supervisor_intent”再由 daemon 合并？
2. `reference_paths` 目前只是建议，v7 是否应升级为 brief 中“必须预读”的上下文？如果预读文件很大，如何预算和截断？
3. ProjectMemory 的“可复用事实”由谁提炼？daemon 自动摘要、Codex 显式指定、还是 report worker 自己输出结构化 fact？
4. 是否每个 worker kind（coding/report/plan）都应该有独立的 brief schema 模板？
5. Runtime injection（supervisor instruction / policy block）是否应该也写入 brief 的历史版本，以便事后审计 worker 看到了什么？
6. 多 worker 共享的 project memory 是否需要版本化或冲突检测？当两个 report 对同一文件给出矛盾结论时如何处理？

## 8. 无预编译背景是否增加了任务难度

**是的，明显增加了难度。**

具体表现为：

- 需要从头阅读 `routes.rs`、`hooks.rs`、`projection.rs` 等较长文件才能理解当前上下文是如何生成和注入的，而不能依赖一份预先提炼的架构摘要。
- 对 v7 的命名和概念边界没有统一口径：仓库里已经存在 `context_packet`、handoff prompt、context injection、worker state projection 等多个相关但职责不清的概念，容易混淆。
- 需要同时阅读三份同日期的 v7 研究报告（shared context / worker brief / demo issues）来对齐团队当前思路，但它们之间既有重叠也有术语差异（例如 `ContextPacket` vs `WorkerBrief`）。
- 无法判断哪些设计是“已经被团队接受”的，哪些只是研究候选，因此本报告把结论限定为“建议”，并保留开放问题。

不过，无预编译背景也带来一个好处：结论更贴近代码实际，不会因为预先形成的方案而忽视当前实现中的具体约束（例如 `claude_workspace` 与 `target_workspace` 的分离、prompt gate 的 ack 机制、hook 注入的预算限制）。

## 9. 最终建议

v7 向 Claude Code PTY worker 暴露上下文的最小可行形态是：

1. **用 `WorkerBrief` 替代当前松散的 `context_packet` + handoff prompt**：
   - JSON schema 供 daemon/MCP 验证；
   - Markdown 版本供 worker 阅读；
   - 存放在 `.agentcall/briefs/<route_id>.{json,md}`。
2. **保持 daemon 为 brief 的编译者和状态权威**：
   - Codex 提供 route 请求和 supervisor_intent；
   - daemon 负责裁剪 AGENTS.md、选择 relevant facts、生成 repo navigation、写入输出契约。
3. **新增 ProjectMemory 层，但严格限制写入权限**：
   - 只接受 report accept 后的结构化 fact；
   - worker 只读，不可直接写。
4. **收窄 Runtime Injection 的语义**：
   - 只补 brief 未覆盖的运行时变化；
   - 所有注入必须留下结构化记录。
5. **MCP 工具面保持精简**：
   - 不新增独立工具，只在 `agentcall_route` / `agentcall_session` 返回中增加 brief 相关字段。

这条路径既符合 AgentCall“Codex supervisor + daemon 权威 + PTY worker 边界执行”的核心分工，又能实际降低 Codex 组织多 worker 时的上下文压力。
