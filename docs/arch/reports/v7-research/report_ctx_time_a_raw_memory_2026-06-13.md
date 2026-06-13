# AgentCall Worker Report → Project Memory 蒸馏方案

日期：2026-06-13
任务：Timing A/B/C 实验组 A —— 无预编译背景下，审阅 AgentCall 仓库，提出已接受 worker report 应如何被蒸馏为 Project Memory / 可复用事实，供后续 Worker Brief 使用。

## 摘要

AgentCall v6.x 已经跑通 `route -> session -> report -> accept` 的闭环，但 report accept 后没有把报告内容结构化地回流到项目记忆。Codex 每次派新 worker 都得重新组织项目背景。本报告建议：在 report accept 后由 daemon 从报告中抽取**可复用事实（reusable facts）**，写入 daemon 拥有的 `.agentcall/context/project_memory.ndjson`，后续 `WorkerBrief` 编译时按 `path/tag/task-kind` 选择相关事实注入。整个流程应保持可审计、可预算、严格限制写入权限，避免把 report 全文 dump 进 future brief。

## 1. 已读文件

- **项目规则与产品形态**
  - `README.md`：v6.7.1 架构、worker kind（coding/report）、route/session/report 流程、projection-first 策略。
  - `AGENTS.md`：worker 纪律、route 契约、report 验收规则、frozen plan 约束。
- **当前 Worker Brief / 上下文机制**
  - `crates/agentcall-daemon/src/routes.rs`：`RouteRequest`、`create_context`、handoff prompt 生成、`pty_prompt_control_envelope`、`pty_prompt_containment`、toolchain context。
  - `crates/agentcall-daemon/src/mcp.rs`：MCP 工具面、`agentcall_route`/`agentcall_session`/`agentcall_report` 返回结构、report accept 路径。
  - `crates/agentcall-daemon/src/confidence.rs`：report 置信度计算（artifact / daemon_write / route_match）。
- **现有报告样例**
  - `.agentcall/tasks/task-0017/report.md`：带 frontmatter 的轻量报告。
  - `.agentcall/tasks/task-0014/report.md`：带 findings / priorities 的审计报告。
- **同日 v7 研究**
  - `docs/reports/report_v7_worker_brief_research_2026-06-13.md`
  - `docs/reports/report_v7_shared_context_research_2026-06-13.md`
  - `docs/reports/report_ctx_ab_a_raw_objective_2026-06-13.md`
  - `docs/reports/report_ctx_ab_b_short_context_2026-06-13.md`
  - `docs/reports/report_ctx_ab_c_model_context_2026-06-13.md`

## 2. 假设

1. **daemon 是 Project Memory 的唯一写入者**。worker 只能读，不能直接写项目记忆。
2. **只有 report accept 之后的事实才回流**。`overall=high` 优先，`medium` 可回流但需标注，`low` 不回流。
3. **Worker Brief 已有明确定义**。本报告聚焦“事实从 report 到 memory 的提取”，不重新定义 brief 全貌。
4. **提取流程先确定性后智能**。MVP 不用大模型重新理解全文，而是用结构化 frontmatter + 规则化抽取。
5. **项目记忆按 target_workspace 隔离**。不同 workspace 不共享 facts，避免路径/工具链冲突。

## 3. 建议的事实 Schema

每条事实应是自包含、可索引、可审计的最小单元：

```json
{
  "fact_id": "fact-20260613-001",
  "entry_type": "toolchain_fact",
  "fact": "此仓库使用 `cargo test --workspace` 作为标准 Rust 测试命令",
  "source_report": "docs/reports/report_v6.2-implementation-closure.md",
  "source_route": "route-1678001",
  "source_session": "session-abc",
  "accepted_at": "2026-06-13T09:30:00Z",
  "confidence": "high",
  "applies_to": {
    "workspaces": ["E:/Project/AgentCall"],
    "paths": ["crates/agentcall-daemon", "crates/agentcall-mcp", "crates/agentcall-hook"],
    "tags": ["rust", "testing", "toolchain"],
    "worker_kinds": ["coding", "report"]
  },
  "scope": {
    "file_regex": "^crates/.*\\.rs$",
    "task_keywords": ["test", "verify", "build"]
  },
  "expires": null,
  "replaces": [],
  "related_facts": ["fact-20260613-002"]
}
```

### `entry_type` 建议枚举

| 类型 | 含义 | 示例 |
|---|---|---|
| `toolchain_fact` | 已验证的工具/命令/镜像事实 | `pip install -i https://pypi.tuna.tsinghua.edu.cn/simple` |
| `project_rule` | 从 AGENTS.md 或报告中确认的项目纪律 | `Do not edit docs/v6.2-code-plan.md` |
| `user_decision` | 用户明确拍板的产品/工程决策 | `v7 不做完整 shared memory` |
| `known_pitfall` | 已踩过的坑及规避方式 | `MCP transport closed 时不要反复 tool_search` |
| `verified_path` | 已验证的重要文件/模块位置 | `report accept 逻辑在 crates/agentcall-daemon/src/mcp.rs:1777` |
| `rejected_hypothesis` | 被证据推翻的假设 | `sdk runtime 默认可用 → 错误` |
| `boundary_fact` | 关于读写边界的事实 | `report worker 不可修改 crates/agentcall-daemon/src` |

### 事实置信度

- `high`：有 daemon-observed evidence（文件写入、测试通过、日志事件）支持，或来自 AGENTS.md 等稳定来源。
- `medium`：report 有明确结论但缺少 daemon evidence，或来自单次观察。
- `low`：仅 worker 自述、推测、未验证假设 → **不写入 Project Memory**。

## 4. 报告接受后的提取流程

建议在 `accept_report_for_session` 之后追加一个可选的 `extract_facts` 阶段：

```text
agentcall_report(action=accept, session_id=...)
  -> daemon 计算 confidence
  -> 若 overall >= medium 且 accept_status=ok：
       触发 FactExtractor
         1. 读取 report.md 全文
         2. 解析 frontmatter（task_id / agent / status / 标签）
         3. 按 entry_type 规则抽取候选 facts
         4. 用 daemon events 验证每条候选 fact
         5. 写入 .agentcall/context/project_memory.ndjson
  -> session/route projection 更新为 "report_accepted_facts_extracted"
```

### 4.1 输入来源

- `report.md` 全文及 frontmatter。
- `agentcall_report` 返回的 `confidence`（overall/artifact/daemon_write/route_match）。
- daemon event store 中该 session 的事件（`file_written`、`test_passed`、`policy_block`、`permission_denied` 等）。
- route 的 `objective`、`write_paths`、`reference_paths`、`worker_kind`。

### 4.2 抽取规则（MVP 版）

| 来源区域 | 抽取方式 |
|---|---|
| frontmatter `status: completed` | 标记可回流前提；`status: blocked` 仍允许回流 pitfall/blocker 事实 |
| `# Checks` / `- [x] ...` | 把已勾选动作转成 `verified_path` 或 `toolchain_fact` |
| `# Findings` 中带 **P0/P1/P2** 的条目 | 转成 `known_pitfall` 或 `project_rule` |
| `# Recommended Next Patch` | 转成 `verified_path` + `toolchain_fact` |
| `# Failure Modes` 表格 | 转成 `known_pitfall` |
| 显式代码路径 | 用正则 `crates/[^\s]+\.rs(:\d+)?` 提取为 `verified_path` |
| 显式命令 | 用反引号命令块提取为 `toolchain_fact` |
| `AGENTS.md` / `README.md` 引用 | 标记 `project_rule`，source 仍指向原报告 |

### 4.3 写入位置

```text
<target_workspace>/.agentcall/context/project_memory.ndjson
<target_workspace>/.agentcall/context/decisions.ndjson   # 专门存 user_decision
<target_workspace>/.agentcall/context/facts/<route_id>.json   # 单次 report 提取的原始 facts（便于审计）
```

`project_memory.ndjson` 采用追加写 + 幂等 `fact_id`，旧事实不会被覆盖，而是通过 `replaces` / `superseded_by` 链接更新。

### 4.4 与 Worker Brief 的衔接

在 `agentcall_route` 编译 `WorkerBrief` 时：

1. 读取 `project_memory.ndjson`。
2. 按当前 `objective`、`write_paths`、`reference_paths`、`worker_kind` 做相关性过滤。
3. 选择最多 N 条（MVP 建议 5-10 条）置信度最高的事实。
4. 放入 `WorkerBrief.relevant_facts`。
5. 在 brief 的 `sources` 中列出每条 fact 的来源 report，供 worker 追溯。

相关性过滤规则：

- 路径前缀匹配：fact.applies_to.paths 与 write_paths/reference_paths 有交集。
- 关键词匹配：objective 与 fact.scope.task_keywords 或 fact.fact 文本有交集。
- worker_kind 匹配：fact.applies_to.worker_kinds 包含本次 worker_kind。
- 时间衰减：超过 90 天未验证的 `toolchain_fact` 自动降级为 `medium`。

## 5. 冲突与过期处理

### 5.1 冲突检测

当新 fact 与已有 fact 在以下维度矛盾时，标记为 `conflict`：

- 同一路径/命令给出相反结论。
- 同一次 route 的多个 report 给出矛盾事实。
- fact 与 `AGENTS.md` / frozen plan 明显冲突。

处理方式：

1. 新 fact 仍写入，但 `confidence` 降为 `medium`。
2. 在 `.agentcall/context/conflicts.ndjson` 中记录冲突对。
3. `WorkerBrief` 遇到冲突事实时，同时呈现双方并提示 worker“先验证再行动”。
4. supervisor（Codex）在 board 的 attention 中收到冲突提醒。

### 5.2 过期策略

| 类型 | 默认 TTL | 过期行为 |
|---|---|---|
| `toolchain_fact` | 90 天 | 降级为 medium，新 report 验证后刷新 |
| `known_pitfall` | 180 天 | 保留但标记为 stale；新版本修复后归档 |
| `user_decision` | 无 | 永久保留，除非用户显式撤销 |
| `project_rule` | 无 | 与 AGENTS.md 同步；AGENTS.md 更新后重新验证 |
| `verified_path` | 30 天 | 代码重构后路径可能失效，需重新验证 |

### 5.3 版本化

- 每条 fact  immutable，更新时生成新 `fact_id` 并通过 `replaces` 指向旧 fact。
- 保留完整历史，便于审计“worker 为什么知道/不知道某件事”。

## 6. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| 抽取器过度抽取，把 report 全文变成 facts | Project Memory 膨胀，brief 变胖 | 只抽结构化条目；单篇 report 限制最多 10 条 facts；人工/Codex review 高光 |
| 把未验证 worker 自述当事实 | 幻觉级联 | 仅 `overall >= medium` 且带 daemon evidence 时置信度可为 high；low 不回流 |
| 路径/工具链事实过期 | 后续 worker 用错命令 | TTL + 自动降级 + 要求新 report 验证 |
| 多 report 冲突未解决 | worker 收到矛盾指令 | 冲突检测 + brief 中显式标注 + supervisor attention |
| 抽取器本身成为复杂源 | 维护成本上升 | MVP 用确定性规则；大模型辅助只在必要时启用 |
| 与现有 `context_packet` 重复 | 两套上下文并存 | 把 `context_packet` 并入 `WorkerBrief`，facts 作为 brief 的一个字段 |
| 隐私/敏感信息泄露 | report 中可能含 token、路径、用户名 | 抽取前 redact 敏感字段；不上传项目记忆到外部 |

## 7. 开放问题

1. **fact 由谁提炼？**
   - 方案 A：daemon 自动抽取（本报告推荐）。
   - 方案 B：report worker 在 report 末尾输出结构化 `## Reusable Facts`。
   - 方案 C：Codex 在 `agentcall_report(action=accept)` 后显式指定要回流的 facts。
2. **用户决策如何区分？**
   - 是否需要 worker 在 report 中显式标注 `user_decision`？还是由 Codex 在 accept 时传入标签？
3. **路径匹配精度**
   - 是否引入 glob/regex，还是严格前缀匹配？MVP 用前缀是否足够？
4. **跨 workspace 事实**
   - 是否存在所有 AgentCall 项目通用的 facts（如 `cargo test --workspace`）？是否允许全局 memory？
5. **`report_path` 不在 target_workspace 内时**
   - 项目记忆应挂在 `target_workspace` 还是 `daemon_workspace`？
6. **与 plan mode 的交互**
   - plan phase 的 report 是否也回流？plan 阶段结论可能是临时的，应如何标记？
7. **事实的删除/撤销**
   - 用户或 Codex 如何显式撤销一条已接受 fact？是否需要 `agentcall_context(action=revoke_fact)`？

## 8. 无预编译背景是否让任务变慢/变难

**结论是：明显增加了时间和认知负担，但结论反而更贴近代码实际。**

变难的地方：

- 需要从头阅读 `routes.rs`、`mcp.rs`、`confidence.rs` 才能确认 report accept 流程和置信度模型，而不是依赖一份预提炼的架构摘要。
- 仓库里已有 `context_packet`、handoff prompt、worker state projection 等多个相关概念，容易混淆它们的职责边界。
- 需要同时对照多份同日的 v7 研究报告，才能对齐当前团队思路，但它们术语不完全一致（例如 `ContextPacket` vs `WorkerBrief`）。
- 无法判断哪些设计是“已被接受”、哪些是“研究候选”，所以本报告把结论限定为建议，并保留开放问题。

不变/变好的地方：

- 因为没有任何预设立场，不会强行套用一套与当前 daemon 不兼容的 memory 模型。
- 能直接观察到 `agentcall_report` 的 confidence 分 four 档、daemon 会观察 `file_written`/`test_passed`/`policy_block` 等事件，这些是实现 Project Memory 抽取器时必须对齐的真实约束。
- 本报告聚焦“report accept 后如何回流”这一具体问题，没有扩展到 Worker Brief Compiler 的全局设计，保持了范围收敛。

## 9. 最终建议

最小可行实现：

1. **在 `accept_report_for_session` 后追加 `FactExtractor`**，读取 report.md frontmatter 和 daemon events，按规则抽取最多 10 条 facts。
2. **写入 `.agentcall/context/project_memory.ndjson`**，采用 append-only + `fact_id` + `replaces` 链接。
3. **`WorkerBrief` 编译时按路径/标签/worker_kind 选择相关 facts**，限制 brief 中 `relevant_facts` 数量。
4. **事实回流与 report confidence 绑定**：`overall=high` 自动回流，`medium` 标注来源，`low` 不回流。
5. **建立冲突/过期检测**，避免 project memory 变成无人维护的垃圾堆。

这样 AgentCall 就能在保持 daemon 权威和 worker 边界的前提下，让 report accept 真正产生复利：每个 worker 不仅交付成果，还为下一个 worker 留下经过验证的上下文片段。
