# AgentCall 上下文窗口 / Worker Brief 质量量化评估框架提案

**任务**: 在无上下文注入（Context Injection: none）条件下，审阅 AgentCall 仓库，设计一套可落地的 Context Window / Worker Brief 质量量化评估 harness，覆盖 benchmark 任务集、时间指标、百分制质量量规、daemon/event/report 数据采集方案、验收阈值与风险。

**结论**: 本仓库已有的 `ChildReport`、`events.ndjson`、call artifacts、`context_packet` 与 `run.json` 为量化评估提供了天然埋点；建议新增一个轻量级 `agentcall-benchmark` 入口，以可重复的 scenario 矩阵驱动 simulation/headless 双模式运行，统一输出评分与 traces。无注入上下文使本任务从“设计方案”退化为“从零阅读代码推导可用指标”，显著增加了分析与验证假设的时间。

---

## 1. 已读文件（files_read）

| 路径 | 作用 |
|---|---|
| `E:\Project\AgentCall\AGENTS.md` | worker 纪律、两种 worker kind、MCP 推荐流程、版本纪律 |
| `E:\Project\AgentCall\README.md` | 架构总览、配置、脚本入口、hook/cwd 规则 |
| `E:\Project\AgentCall\docs\agentcall-protocol.md` | canonical MCP 工具、报告验收规则、禁止行为 |
| `E:\Project\AgentCall\docs\rust-daemon-architecture.md` | Rust daemon 职责、事件流、session API 形态 |
| `E:\Project\AgentCall\docs\session-supervisor.md` | PTY session 文件布局、命令示例 |
| `E:\Project\AgentCall\docs\sop-protocol.md` | task.md/report.md/review.md 契约、event stream、worker registry |
| `E:\Project\AgentCall\src\agentcall\models.py` | Task/RunRecord/Worker 数据模型 |
| `E:\Project\AgentCall\src\agentcall\store.py` | events.ndjson、call artifacts、state files、board state |
| `E:\Project\AgentCall\src\agentcall\supervisor.py` | run 生命周期、stdout/stderr/run.json |
| `E:\Project\AgentCall\src\agentcall\v2\types.py` | ChildCallSpec、mode/role、prompt 模板 |
| `E:\Project\AgentCall\src\agentcall\v2\context.py` | ContextPacket / ContextSufficiency 数据模型 |
| `E:\Project\AgentCall\src\agentcall\v2\reports.py` | ChildReport schema、校验、scope 校验 |
| `E:\Project\AgentCall\src\agentcall\v2\orchestrator.py` | plan/execute/review 三阶段工作流、事件写入 |
| `E:\Project\AgentCall\src\agentcall\v2\state.py` | agent 生命周期快照 |
| `E:\Project\AgentCall\src\agentcall\v2\drivers.py` | HeadlessJsonClaudeDriver、FunctionAgentDriver |
| `E:\Project\AgentCall\src\agentcall\v2\workflows.py` | small_project 仿真与 scripted driver |
| `E:\Project\AgentCall\src\agentcall\v2\inspection.py` | 工作流检查工具 |
| `E:\Project\AgentCall\src\agentcall\v2\transcripts.py` | transcript 消息/工具使用统计 |
| `E:\Project\AgentCall\docs\reports\report_ctx_metric_simple_a_raw_2026-06-14.md` | 同实验组 simple 版本报告，确认格式边界 |
| `E:\Project\AgentCall\docs\reports\report_ctx_ab_a_raw_objective_2026-06-13.md` | v7 上下文暴露建议，理解 brief 语义层级 |

---

## 2. 评估目标与范围

### 2.1 评估什么

1. **Context Window 效率**：worker 实际消耗的 prompt/上下文字符数、token 数、与任务完成质量之间的性价比。
2. **Worker Brief 质量**：daemon/Codex 编译出的 brief（`context_packet` / handoff prompt）是否让 worker 在限定 turn/time 内正确完成目标。
3. **运行可靠性**：policy deny、out-of-scope writes、revision 率、阻塞率。
4. **可审计性**：事件流与报告是否足以复现评分。

### 2.2 不评估什么

- 不涉及真实 LLM 的创意/开放生成质量；只评估“在 bounded task 下是否按契约交付”。
- 不修改 daemon/MCP 实现；本提案为纯 harness 与指标设计，落地时可先以 Python 脚本 + simulation driver 验证。

---

## 3. Benchmark 任务集

基于仓库已有结构，建议分 6 组 scenario，每组覆盖不同 brief 复杂度与上下文需求：

| 组号 | Scenario | Worker Kind | 关键上下文需求 | 失败模式 |
|---|---|---|---|---|
| B1 | 修复 `calculator.add` 使 `add(2,3)==5` | coding | 文件路径、测试命令、write scope | 改错文件、未验证 |
| B2 | 阅读 `AGENTS.md` 与 `README.md` 后写一份 protocol 摘要 | report | 文档引用路径、report_path、禁止写实现文件 | 漏读文件、越界写 |
| B3 | 在不启动 daemon 时诊断 `agentcall.py doctor` 输出并报告阻塞项 | report | toolchain 提示、本地路径、只读边界 | 误删配置、未识别 doctor 失败 |
| B4 | 为 `src/agentcall/v2/reports.py` 新增一个 `validate_report_contract` 的 negative test | coding | 被测文件、pytest 命令、只写 tests/ | 未写测试、改被测文件 |
| B5 | 多轮 plan-execute：先规划再修改 `models.py` 增加一个字段 | coding | plan context、prior_reports、execute scope | plan 阶段改文件、execute 不读 plan |
| B6 | 在 brief 故意缺少 `reference_paths` 时，worker 能否声明 `context_sufficiency=needs_context` | report | sufficiency 契约、父层可解决性 | 硬猜、不声明缺失 |

每组至少跑 3 次（不同随机/不同 brief 裁剪），形成可重复的 scenario matrix。

### 3.1 Brief 变体（Treatment）

为控制变量，每个 scenario 跑 4 种 brief 变体：

1. **Raw objective**：仅传 `objective`，无 `context_packet`（baseline）。
2. **Minimal brief**：`objective` + `write_paths`/`report_path` + `acceptance_criteria`。
3. **Structured brief**：完整的 `ContextPacket`（含 `relevant_files`、`decisions`、`risks`、`forbidden_actions`）。
4. **Overstuffed brief**：在 structured 基础上加入 3 倍无关文件/历史报告，测试上下文噪声。

---

## 4. 时间指标

| 指标 | 定义 | 数据来源 | 单位 |
|---|---|---|---|
| `T_route` | `agentcall_route` 返回至 worker prompt 提交的时间 | daemon events: `route.created` → `prompt_submitted` | s |
| `T_first_action` | prompt 提交到首个 tool_use/Bash 的时间 | transcript / hook events | s |
| `T_plan` | plan 子生命周期耗时 | events: `child.call_started` → `child.report_received` (role=planner) | s |
| `T_execute` | execute 子生命周期耗时 | events: `child.call_started` → `child.report_received` (role=executor) | s |
| `T_review` | review 子生命周期耗时（如触发） | events: `reviewer.call_started` → `reviewer.report_received` | s |
| `T_total` | 路由创建到 `agentcall_report(action=accept)` | events/route projection | s |
| `T_retry` | 因 policy deny / revision 导致的额外耗时 | 同一 task 多次 run 的 `T_total` 差值 | s |
| `T_blocked` | 处于 `blocked_by_policy` / `needs_user` 状态累计时间 | projection state transitions | s |

### 4.1 建议 SLO

- `T_total` P90 ≤ 300s（单轮 bounded task）。
- `T_retry` / `T_total` ≤ 25%（即 retries 不应吃掉超过 1/4 时间）。
- `T_blocked` / `T_total` ≤ 10%。

---

## 5. 质量量规（100 分制）

总分 `Q = w_correct*40 + w_brief*25 + w_efficiency*20 + w_process*15`，每个维度下设可自动/半自动打分的子项。

### 5.1 正确性（Correctness）— 40 分

| 子项 | 分值 | 评分方式 |
|---|---|---|
| 验收标准通过 | 20 | 自动检查：B1/B4 运行测试/断言通过；B2/B3/B6 人工或规则核对报告内容 |
| 无越界写 | 10 | 自动：`changed_files` 全部落在 `allowed_paths` 内（`validate_scope`） |
| 无幻觉事实 | 10 | 半自动：报告中引用的文件路径、命令、版本号与仓库实际一致 |

### 5.2 Brief 有效性（Brief Quality）— 25 分

| 子项 | 分值 | 评分方式 |
|---|---|---|
| 未声明缺失却失败 | -10/次 | 自动：`context_sufficiency.status==enough_to_act` 但任务未通过 |
| 合理声明缺失 | +10 | 自动：B6 中 `status==needs_context` 且 `can_parent_resolve==true` |
| 使用 brief 中的关键路径 | +5 | 自动：报告或 tool_use 中引用了 `relevant_files` / `reference_paths` 中的文件 |
| 未使用无关上下文 | +10 | 自动：overstuffed 变体下未引用无关文件且未受其误导 |

### 5.3 效率（Efficiency）— 20 分

| 子项 | 分值 | 评分方式 |
|---|---|---|
| Prompt 长度分 | 10 | 自动：`score = max(0, 10 - max(0, prompt_chars - budget_chars)/budget_chars*10)` |
| Turn 使用分 | 10 | 自动：`score = max(0, 10 - (turns_used - expected_turns)/expected_turns*10)` |

其中 `budget_chars` 由 brief schema 中的 `max_prompt_chars` 决定；`expected_turns` 按 scenario 设定（如 B1=1，B5=2）。

### 5.4 流程纪律（Process Discipline）— 15 分

| 子项 | 分值 | 评分方式 |
|---|---|---|
| 报告 schema 合规 | 5 | 自动：`validate_report_dict` 通过 |
| 未触发 review 即一次过 | 5 | 自动：无 `review.md`、无 `parent.rejected` 事件 |
| 未重复被禁动作 | 5 | 自动：events 中无重复 `policy_denied` 同一动作 |

### 5.5 等级划分

| 总分 | 等级 | 解读 |
|---|---|---|
| 90–100 | A | brief 质量优秀，可直接作为生产模板 |
| 75–89 | B | 可用，需针对失败子项微调 |
| 60–74 | C | 需要补充上下文或收紧 scope |
| < 60 | D | brief 设计存在根本缺陷，禁止上线 |

---

## 6. 数据采集计划（daemon / events / reports）

### 6.1 已有数据

| 来源 | 路径 | 可用字段 |
|---|---|---|
| 事件流 | `.agentcall/events.ndjson` | `ts`, `type`, `task_id`, `run_id`, `message`, `data` |
| 子调用产物 | `.agentcall/tasks/<task>/calls/<call>/` | `input.json`, `prompt.md`, `context.json` |
| 子报告 | `.agentcall/tasks/<task>/reports/<call>.json` | `ChildReport` 全字段 |
| 运行记录 | `.agentcall/tasks/<task>/runs/<run>/run.json` | `pid`, `exit_code`, `started_at`, `completed_at` |
| 任务元数据 | `.agentcall/tasks/<task>/task.json` | `status`, `assigned_worker`, `created_at` |
| transcript | `.agentcall/transcripts/<session>.jsonl` | 消息数、tool_use/tool_result 数 |
| 状态投影 | `.agentcall/state/active_sessions.json` | 实时 session state |

### 6.2 需要新增/强化的采集点

1. **Prompt 字符数/Token 数**：在 `write_call_artifacts` 时把 `prompt_chars` 与 `prompt_tokens`（tiktoken 估算）写入 `input.json`。
2. **Worker 实际使用的 tool 统计**：从 transcript 解析，或 hook 在 `hooks.rs` 侧写入事件。
3. **Brief 预算**：`ContextPacket` 增加 `budget.max_prompt_chars` / `estimated_chars`。
4. **Policy deny 明细**：每次 deny 写入 `policy_denied` 事件，含 `tool`, `path`, `reason`, `repeat_count`。
5. **Context sufficiency 来源**：`ChildReport.context_sufficiency` 已存在，但需要把“声明缺失后父层是否解决”也记为事件。

### 6.3 评估 harness 输出

每次 benchmark run 输出一个 JSON：

```json
{
  "benchmark_id": "ctx-metric-hard-a-raw",
  "scenario": "B1",
  "treatment": "structured",
  "task_id": "task-0001",
  "scores": {
    "correctness": 38,
    "brief_quality": 22,
    "efficiency": 17,
    "process": 15,
    "total": 92
  },
  "timing": {
    "T_route": 1.2,
    "T_total": 45.0,
    "T_retry": 0.0,
    "T_blocked": 0.0
  },
  "resources": {
    "prompt_chars": 3200,
    "prompt_tokens": 890,
    "turns_used": 1,
    "tool_uses": 4
  },
  "artifacts": {
    "events": ".agentcall/events.ndjson",
    "reports": [".agentcall/tasks/task-0001/reports/..."],
    "call_artifacts": ".agentcall/tasks/task-0001/calls/..."
  }
}
```

---

## 7. 验收阈值

### 7.1 Per-Scenario 阈值

| Scenario | 最低总分 | 必须通过的子项 |
|---|---|---|
| B1 | 80 | 验收标准通过、无越界写 |
| B2 | 75 | 未声明缺失却失败 = 0、报告 schema 合规 |
| B3 | 75 | 无越界写、未重复被禁动作 |
| B4 | 80 | 验收标准通过、无越界写 |
| B5 | 70 | plan 阶段未改文件、execute 通过 |
| B6 | 85 | 合理声明缺失 |

### 7.2 Global 阈值

- 6 组 scenario 平均分 ≥ 78。
- 无 D 等级任务。
- Overstuffed 变体相对 Structured 变体总分下降 ≤ 15 分（上下文噪声容忍度）。
- Raw objective 变体平均分 ≤ Structured 变体平均分 - 10（证明 brief 确实有价值）。

---

## 8. 实现路径建议

### 8.1 Phase 1：基于现有 simulation driver

利用 `src/agentcall/v2/workflows.py` 的 `run_small_project_workflow`，扩展 `build_scripted_small_project_driver` 支持 scenario B1/B4/B5 的 scripted 模拟，验证评分函数是否稳定。

### 8.2 Phase 2：Headless JSON 对照

使用 `HeadlessJsonClaudeDriver` 对真实 Claude API 跑同一矩阵，对比 simulation 与真实模型的分数漂移。

### 8.3 Phase 3：Daemon PTY worker 端到端

在 daemon 启动后，通过 `agentcall_route` + `agentcall_session_send` 驱动真实 Claude Code PTY worker，采集 transcript 与 hook 事件，完成端到端验证。

### 8.4 新增文件建议（仅报告，不动源码）

- `scripts/benchmark_context_window.py`：harness 入口。
- `scripts/benchmark_scenarios.json`：scenario 定义。
- `docs/reports/report_ctx_metric_hard_a_raw_2026-06-14.md`：本报告。

---

## 9. 风险（risks）

1. **Prompt token 估算不准确**：仓库目前没有统一 tokenizer；使用 tiktoken 估算可能与 Claude 实际计费存在偏差，建议以字符数为首要指标。
2. **Simulation driver 无法代表真实 worker**：scripted driver 总是成功，可能掩盖 brief 缺陷；必须与 headless/PTY 真实运行交叉验证。
3. **评分函数过拟合**：过度优化 `prompt_chars` 可能导致 brief 过短、worker 缺上下文；应结合 correctness 一起判断。
4. **Context sufficiency 自我报告不可信**：worker 可能把失败归咎于“缺上下文”；需要父层或 reviewer 验证其真实性。
5. **事件流格式变化**：daemon 从 NDJSON 向 SQLite 迁移中（v6.7.1），harness 需要同时支持两种 backend 读取。
6. **跨平台路径差异**：Windows 与 Linux 的 `write_paths` 校验、PTY 行为不同，benchmark 需要隔离路径标准化逻辑。
7. **MCP transport 不稳定**：真实 PTY worker 运行受 `Transport closed`、权限菜单等噪音影响，可能让 timing 指标失真。

---

## 10. 无注入上下文的影响评估

**是的，缺少上下文注入明显放慢并复杂化了本任务。**

具体表现：

- **需要从零建立指标假设**：没有预置的“应该评估什么”的提示，必须自己从 `ContextPacket`、`ChildReport`、`events.ndjson`、handoff prompt、transcript 等多处推断哪些字段可用、哪些需要新增。
- **无法确认当前最佳实践**：仓库里同时存在 v1 SOP（`task.md`/`report.md`）、v2 orchestrator（`ChildCallSpec`/`ChildReport`）和 v7 daemon 三层上下文概念，术语不一致，需要自己判断以哪一层为评估锚点。
- **无法验证假设**：没有注入的 benchmark scenario 列表或示例 brief，所有 B1–B6 均需要自己根据仓库结构构造，无法与标准答案对照。
- **增加了阅读范围**：为确认 harness 不越界，需要通读 `store.py`、`orchestrator.py`、`reports.py` 等实现文件，而非依赖一份高层摘要。

唯一的好处是：所有指标都贴近代码实际，不会因预编译指令而假设不存在的能力（例如仓库目前没有 token 计数，也没有现成的 benchmark 入口）。

---

## 11. 开放问题

1. Token 估算应使用 tiktoken 还是等 daemon 侧暴露实际 prompt token？
2. 真实 PTY worker 的 transcript 是否默认启用/保留，还是需要 harness 显式请求？
3. Overstuffed brief 的“无关文件”应由 scenario 固定列表生成，还是由算法从仓库随机采样？
4. `context_sufficiency` 目前由 worker 自我报告，是否需要 reviewer 或规则引擎进行二次判定？
5. 本 harness 是否应纳入 CI，还是仅作为研究/实验工具？

---

**报告完成时间**: 2026-06-14  
**任务 ID**: ctx-metric-hard-a-raw  
**报告路径**: `E:\Project\AgentCall\docs\reports\report_ctx_metric_hard_a_raw_2026-06-14.md`
