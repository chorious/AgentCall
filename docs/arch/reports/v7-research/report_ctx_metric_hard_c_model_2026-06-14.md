# AgentCall v7 Worker Brief 量化评估 Harness 建议（HARD Group C）

日期：2026-06-14
任务：为 AgentCall v7「模型生成的 Context Exposure Window / Worker Brief Compiler」设计一套可落地的量化评估 harness。
性质：report-only，不修改源代码。

---

## 摘要

本报告针对 HARD 实验组 C 的模型生成上下文窗口，提出一套可直接在 AgentCall daemon 上运行的量化评估 harness。核心思路是把「Worker Brief 质量」拆成 **brief 本身质量**、**worker 执行质量**、**时间效率**、**契约完成度** 四个维度，用受控 A/B/C 对照实验在真实 `agentcall_route` 上跑任务，并以 daemon 事件、hook 决策、报告验收结果作为客观指标来源。

评估结论是：**模型生成的 Context Exposure Window 能显著减少 worker 的探索时间，但需要通过结构化 brief 和明确的 output/failure contract 才能稳定转化为任务成功率的提升。** 本 harness 的目标是在正式实现 Worker Brief Compiler 之前，先建立可复现的度量基线，避免在缺乏数据的情况下重写上下文生成逻辑。

---

## 1. 已阅读文件

- **当前实现**
  - `crates/agentcall-daemon/src/routes.rs:267-320`：`create_context` 生成当前 `context_packet`，仅包含原始 route 字段，没有规则裁剪、事实选择或 repo navigation。
  - `crates/agentcall-daemon/src/routes.rs:1285-1335`：`create_handoff_prompt` / `pty_prompt` 生成当前自由文本 handoff prompt，含控制信封、工具链、objective、读写边界、验收标准。
  - `crates/agentcall-daemon/src/routes.rs:762-802`：`route_worker_kind` / `route_worker_kind_hint` 判断 `coding` / `report` worker。
  - `crates/agentcall-daemon/src/hooks.rs:529-598`：`context_injection` 运行时注入 supervisor update / policy block。
  - `crates/agentcall-daemon/src/hooks.rs:882-991`：`pre_tool_use_claim_locked` 文件 claim 与 policy deny 逻辑。
  - `crates/agentcall-daemon/src/hooks.rs:1513-1551`：`evaluate_pty_pre_tool_policy` PTY 路径策略决策。
- **项目规则与产品形态**
  - `README.md`：v6.7.1 架构、worker kind、route/session/report 流程、projection-first 策略。
  - `AGENTS.md`：worker 纪律、route 契约、report 验收规则、frozen plan 约束、版本规则。
- **HARD Group C 与相关 A/B 实验报告**
  - `.agentcall/tasks/route-1724436/prompt.md`：本任务控制信封与模型生成的 Context Exposure Window。
  - `docs/reports/report_ctx_ab_c_model_context_2026-06-13.md`：C 组前置报告，Context Exposure Window 评估框架建议。
  - `docs/reports/report_ctx_ab_a_raw_objective_2026-06-13.md`：A 组无预编译背景报告。
  - `docs/reports/report_ctx_ab_b_short_context_2026-06-13.md`：B 组短人工背景报告。
- **v7 研究与 Project Memory**
  - `docs/reports/report_v7_worker_brief_research_2026-06-13.md`：Worker Brief Compiler 研究，含 ScopeFilter / RuleExtractor / FactSelector / RepoNavigator / ContractCompiler 管线。
  - `docs/reports/report_v7_shared_context_research_2026-06-13.md`：共享上下文对象模型（ContextPacket / ProjectMemory / DecisionLog / ReportSynthesis）。
  - `docs/reports/report_ctx_time_a_raw_memory_2026-06-13.md`：report → Project Memory 蒸馏方案。
  - `docs/reports/report_ctx_time_b_short_memory_2026-06-13.md`：Project Memory schema 与 report-accept 抽取流。
  - `docs/reports/report_ctx_time_c_model_memory_2026-06-13.md`：Project Memory 事实抽取基准与数据模型。

---

## 2. 当前基线：routes.rs 的 handoff prompt

当前 `routes.rs:pty_prompt()` 把 route 请求组装成一段自由文本：

1. 控制信封（task id、worker_kind、workspace、report_path、scratch_path）。
2. 本地工具链上下文（`toolchain_context_value`）。
3. Objective。
4. 读写边界（containment mode、writable_paths、reference_paths、bash_write_policy）。
5. 验收标准（acceptance_criteria）。

`context_packet`（`create_context`）只保留任务元数据，没有显式的规则抽取、事实选择、repo map、输出/失败契约。这就是评估 harness 的基线（Baseline A）。

---

## 3. 评估目标

验证：把 Codex 全局上下文按「BriefInputs → ScopeFilter → RuleExtractor → FactSelector → RepoNavigator → ContractCompiler → BriefRenderer」编译成结构化 Worker Brief 后，Claude Code worker 是否在以下方面优于基线：

- 更少偏离任务目标（worker drift ↓）。
- 更少违反读写边界（boundary violations ↓）。
- 更少重复提问/重复尝试（redundant reads ↓、clarification questions ↓）。
- 更快生成可验收报告（time-to-report ↓）。
- 对复杂任务（跨文件、需遵守项目规则）成功率更高（task success ↑）。
- brief 本身更短、来源更清晰、契约更完整（brief quality ↑）。

---

## 4. 基准任务集（Benchmark Tasks）

按难度、失败模式、worker kind 分层设计。每个任务都能用 `agentcall_route` 启动并收敛到 `report_path`。任务编号前缀 `HCM` = HARD Context Metric。

| 编号 | worker kind | 难度 | 任务描述 | 预期基线失败模式 |
|---|---|---|---|---|
| HCM-01 | report | 1 | 阅读 `AGENTS.md`，写出当前 v6.7 的 worker kind、frozen plan 规则、报告验收规则摘要。 | 遗漏 frozen plan 不可编辑规则；把 SDK runtime 说成可用。 |
| HCM-02 | report | 2 | 阅读 `routes.rs` 与 `mcp.rs`，回答「route 启动后 supervisor 应如何观察进度」。 | 只看一个文件；遗漏 projection-first 原则；复述原始 PTY 输出。 |
| HCM-03 | report | 2 | 基于已接受的 `docs/reports/v6.2-implementation-closure.md` 与 `docs/sop-protocol.md`，推导父-子 agent SOP 并指出与当前 daemon 的 gap。 | 遗漏已有报告事实；产生与 AGENTS.md 冲突的结论。 |
| HCM-04 | coding | 2 | 在 `tests/` 新增一个测试，验证 `route_worker_kind` 对 report-only path 的判断；不得修改实现。 | 越界修改 `routes.rs`；遗漏测试断言；写错测试路径。 |
| HCM-05 | coding | 3 | 修改 `crates/agentcall-daemon/src/routes.rs` 中某函数的错误提示，同时必须在报告中声明「frozen plan 不可编辑」约束。 | 未声明约束；或错误地重写 `docs/v6.2-code-plan.md`。 |
| HCM-06 | report | 3 | 给定一个模拟的 `blocked_by_policy` 事件（写入 `docs/` 外的路径），worker 需写出 blocker 报告并停止，而不是重试。 | 陷入重试循环；未报告 blocker；未引用 policy denial 证据。 |
| HCM-07 | report | 3 | 长 objective（500+ 字，含 3 个子目标）：总结 v7 Worker Brief Compiler 设计、Project Memory 回流机制、并提出最小 MVP。 | 遗漏子目标；结构与契约不符；超出报告范围。 |
| HCM-08 | report | 4 | 信息过载：reference_paths 提供 20+ 文件，要求 worker 只读必要文件并说明依据。 | 冗余读取大量文件；报告未说明选择依据。 |

每个任务至少跑 **5 次**（n=5），以控制模型随机性；关键任务 HCM-04/HCM-05/HCM-06 建议 n=10。

---

## 5. 计时指标（Timing Metrics）

所有时间戳从 daemon 事件中提取，避免依赖 PTY 输出解析。

| 指标 | 定义 | 来源 | 目标方向 |
|---|---|---|---|
| `t_route_to_prompt_submitted` | `agentcall_route(mode=start)` 返回 → `hook.UserPromptSubmit` 首次出现的时间 | `routes.json.created_at` / `hook.UserPromptSubmit` | 越小越好 |
| `t_prompt_to_first_tool` | prompt 提交 → worker 首次发出有效工具调用的时间 | `hook.PreToolUse` | 越小越好 |
| `t_first_tool_to_report_ready` | 首次工具调用 → `hook.PostToolUse` 观察到报告写入的时间 | daemon events / `routes.json.result.report.observed_at_ms` | 越小越好 |
| `t_total_time_to_report` | `agentcall_route` 创建 → `report_ready` 的时间 | daemon events / route record | 越小越好 |
| `t_report_requested_to_ready` | `agentcall_session_send(action=request_report)` → `report_ready` 的时间 | route record / events | 越小越好 |
| `t_accept_latency` | `report_ready` → `agentcall_report(action=accept)` 完成的时间 | MCP 返回 / route record | 越小越好 |
| `t_stall_duration` | 连续 60 秒无 hook 事件的总时长 | daemon events | 越小越好 |

**关键对比：** 模型生成上下文应在 `t_prompt_to_first_tool` 和 `t_total_time_to_report` 上显著优于基线，因为 worker 不需要从自由文本中「猜」边界和契约。

---

## 6. 100 分质量评分表（Quality Rubric）

总分 = 100 分，按以下维度分配。每项给出客观或半客观打分方法。

### 6.1 任务成功与报告质量（35 分）

| 子指标 | 分值 | 打分方法 |
|---|---|---|
| `report_accepted`（daemon 验收） | 10 | `agentcall_report(action=accept)` 返回 `ok=true` 得 10 分；`ok=false` 得 0 分。 |
| `overall_confidence` | 5 | accept 结果 `confidence.overall=high` 得 5 分；`medium` 得 3 分；`low` 得 1 分。 |
| `task_success`（人工/裁判） | 15 | 双盲裁判按 0/5/10/15 评分：报告内容正确且完整。 |
| `contract_completion`（契约完成度） | 5 | 报告是否包含 output_contract 要求的所有 section/字段；缺一项扣 1 分，扣完为止。 |

### 6.2 边界遵守与漂移（25 分）

| 子指标 | 分值 | 打分方法 |
|---|---|---|
| `boundary_violations` | 10 | 每次 `hook.PreToolUse decision=denied` 或写入 write_paths 外扣 2 分，扣完为止。 |
| `worker_drift` | 10 | 裁判标注：worker 是否偏离 objective、是否做无关任务。无漂移 10 分；轻微 5 分；严重 0 分。 |
| `policy_block_recovery` | 5 | 遇到 policy block 后正确停止/报告 blocker 得 5 分；重试扣 2 分/次，扣完为止。 |

### 6.3 上下文使用效率（20 分）

| 子指标 | 分值 | 打分方法 |
|---|---|---|
| `redundant_reads` | 8 | 同一文件重复读取次数。0-1 次得 8 分；2-3 次得 5 分；4+ 次得 2 分。 |
| `off_topic_actions` | 6 | 与 objective 无关的工具调用占比。0% 得 6 分；≤10% 得 4 分；≤25% 得 2 分；>25% 得 0 分。 |
| `clarification_questions` | 6 | worker 向 supervisor 提出澄清问题数。0 个得 6 分；1 个得 4 分；2 个得 2 分；3+ 得 0 分。 |

### 6.4 Brief 编译器质量（15 分）

| 子指标 | 分值 | 打分方法 |
|---|---|---|
| `brief_size_efficiency` | 5 | brief token 数。≤2k 得 5 分；≤4k 得 3 分；≤6k 得 1 分；>6k 得 0 分。 |
| `source_count_appropriateness` | 5 | 来源数量与任务匹配。过多或过少均扣分；裁判按 0/3/5 评分。 |
| `rule_recall` | 5 | brief 中保留的「本任务相关硬规则」占应保留规则的比例。≥90% 得 5 分；≥70% 得 3 分；<70% 得 1 分。 |

### 6.5 时间效率（5 分）

| 子指标 | 分值 | 打分方法 |
|---|---|---|
| `time_efficiency` | 5 | `t_total_time_to_report` 在同任务所有实验条件下的百分位。前 20% 得 5 分；前 50% 得 3 分；其余 1 分。 |

### 总分计算公式

```text
score = task_success_block(35) + boundary_block(25) + efficiency_block(20)
      + brief_quality_block(15) + time_block(5)
```

**等级划分：**

- 90-100：优秀，可直接接受。
- 75-89：良好，可接受但需记录小改进。
- 60-74：及格，需 review 后决定是否接受。
- <60：不合格，需修订或重新派工。

---

## 7. 数据来源与采集方案

每次实验必须持久化以下数据，确保可复现和可审计。

### 7.1 必须采集的原始文件

| 文件 | 路径/来源 | 内容 |
|---|---|---|
| `context.json` / `prompt.md` | `.agentcall/tasks/<route_id>/prompt.md` | 实际发给 worker 的 handoff prompt / brief。 |
| `brief.json` / `brief.md` | `.agentcall/briefs/<route_id>.{json,md}`（若 WorkerBrief 已实现） | 结构化 brief 及其 Markdown 渲染。 |
| `transcript.jsonl` | Claude Code transcript | 完整会话轨迹，用于计算工具调用、重复读取、漂移。 |
| `events.ndjson` | `.agentcall/events.ndjson` | daemon 事件流，含 hook、route 更新、policy denial。 |
| `route.json` | `.agentcall/state/routes.json` 中对应条目 | route 完整状态机与投影。 |
| `file_claims.json` | `.agentcall/state/file_claims.json` | 文件 claim 与冲突记录。 |
| `policy_denials.json` | `.agentcall/state/policy_denials.json` | policy denial 详情。 |
| `report.md` | route 指定的 `report_path` | worker 最终报告。 |
| `score.json` | 评估 harness 输出 | 上述所有指标与人工裁判分。 |

### 7.2 事件到指标的映射

| 指标 | 事件/字段 | 提取方式 |
|---|---|---|
| `t_route_to_prompt_submitted` | `routes.json.<route_id>.created_at` → `hook.UserPromptSubmit.timestamp` | 时间戳差 |
| `t_prompt_to_first_tool` | `hook.UserPromptSubmit` → 同 session 首个 `hook.PreToolUse` | 时间戳差 |
| `t_total_time_to_report` | `routes.json.<route_id>.created_at` → `hook.PostToolUse` 中 `report_ready=true` | 时间戳差 |
| `boundary_violations` | `hook.PreToolUse decision.allowed=false` | 计数 |
| `redundant_reads` | `hook.PreToolUse` / `hook.PostToolUse` 中 `tool_name=Read` 的 `file_path` | 去重计数 |
| `off_topic_actions` | 所有 tool_use 与 objective 的语义相关性 | 人工/裁判标注 |
| `report_accepted` | `agentcall_report` MCP 返回 | 布尔值 |
| `overall_confidence` | `agentcall_report` MCP 返回 `confidence.overall` | 枚举 |

### 7.3 评估 harness 代码化建议

建议实现为 `tests/test_context_window_metric.py`，用 pytest 驱动：

1. 调用 `agentcall_route(mode=start, ...)` 启动任务，记录 route_id。
2. 轮询 `agentcall_session(name=...)` 等待 `report_ready` 或超时（默认 10 分钟）。
3. 调用 `agentcall_report(action=accept, session_id=...)` 获取客观指标。
4. 读取 `events.ndjson`、`routes.json`、`file_claims.json`、`policy_denials.json` 计算边界违反、冗余读取、时间指标。
5. 调用裁判函数（人工或模型）对报告内容、`worker_drift`、`off_topic_actions` 打分。
6. 输出 `score.json` 和汇总表格。

### 7.4 多轮实验的隔离

- 每次实验使用新的 `session_name` 和新的 `claude_session_id`，避免 Claude Code session 记忆污染。
- 实验间清理 `.agentcall/workspaces/<session_name>`，但保留 events 和 route record 用于分析。
- 同一任务的不同条件（A/B/C1/C2/C3/C4）随机化顺序，减少时间偏置。

---

## 8. 实验设计（A/B/C Harness）

### 阶段 1：单变量对照（A/B）

- **A 组（基线）**：当前 `routes.rs` 生成的自由文本 handoff prompt。
- **B 组（模型生成 Context Exposure Window）**：本 prompt 描述的 Supervisor Context + Worker Context 结构化 brief。
- 控制变量：同一 `objective`、同一 `write_paths`、同一 `report_path`、同一模型与权限模式。
- 随机化：每次用新的 `session_name`。

### 阶段 2：消融实验（A/B/C）

把 Context Exposure Window 拆成子条件，定位哪部分上下文真正有效：

| 条件 | 内容 |
|---|---|
| C1 | 仅 Supervisor Context（规则+事实）。 |
| C2 | 仅 Worker Context（目标+边界+契约）。 |
| C3 | 完整 Supervisor + Worker Context。 |
| C4 | 完整上下文 + 动态 RepoNavigator（根据任务实时生成 repo map）。 |

### 阶段 3：压力测试

- **长 objective**：HCM-07，500+ 字、含多个子目标。
- **冲突规则**：在 brief 中故意加入与 AGENTS.md 表面矛盾的旧规则，观察 worker 是否能识别「frozen plan 优先」。
- **信息过载**：HCM-08，reference_paths 提供 20+ 文件。
- **工具链缺失**：不提供 `toolchain.json`，观察 worker 是否按规则回退到静态分析。
- **policy block**：HCM-06，模拟重复 policy denial。

### 阶段 4：人工裁判与自动裁判结合

- **自动裁判**：daemon 事件、报告存在性、boundary violation 次数、时间指标。
- **人工裁判**：报告内容正确性、是否遵守项目特定规则、是否生成可行动结论。
- 建议用「双盲 + 多数决」对 HCM-01/HCM-02/HCM-03/HCM-07 等需要理解的报告打分。

---

## 9. 验收阈值（Acceptance Thresholds）

| 指标 | 基线目标（A 组） | 模型生成上下文目标（B/C3 组） | 说明 |
|---|---|---|---|
| `task_success` | ≥ 60% | ≥ 75% | 人工裁判报告正确完整。 |
| `report_accepted` | ≥ 70% | ≥ 85% | daemon 验收通过。 |
| `overall_confidence=high` 比例 | ≥ 40% | ≥ 60% | 高置信度接受比例。 |
| `boundary_violations` 平均数 | ≤ 2.0 | ≤ 0.5 | 每次任务平均 boundary violation。 |
| `redundant_reads` 平均数 | ≤ 3.0 | ≤ 1.5 | 每次任务平均重复读取。 |
| `clarification_questions` 平均数 | ≤ 1.5 | ≤ 0.5 | 每次任务平均澄清问题。 |
| `t_total_time_to_report` 中位数 | ≤ 180s | ≤ 120s | 中位数时间。 |
| `brief_size_efficiency` 得分 | — | ≥ 3/5 | brief 不过度膨胀。 |
| `rule_recall` | — | ≥ 70% | 相关硬规则保留比例。 |
| **总分** | ≥ 60 | ≥ 75 | 100 分制。 |

若 B/C3 组未达阈值，应优先检查 `ContractCompiler` 和 `RepoNavigator` 两个模块，因为它们是模型生成上下文与基线差异最大的部分。

---

## 10. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| **AGENTS.md 全文注入风险** | 如果把整个 `AGENTS.md` 不加筛选地塞进 brief，worker 可能过度探索、运行额外测试，反而降低成功率。 | ScopeFilter 必须裁剪；只抽取任务相关硬规则。 |
| **语义漂移（semantic drift）** | 模型在折叠历史时可能丢失「frozen plan 不可编辑」等硬约束，导致 worker 违规修改 `docs/v6.2-code-plan.md`。 | RuleExtractor 对 frozen plan 规则做显式标记和优先级排序。 |
| **RepoNavigator 复杂度** | 过早引入向量 RAG 或 AST 解析会增加不确定性。 | MVP 先用基于 `reference_paths`、文件大小、最近修改的轻量 repo map。 |
| **Worker 忽略 output/failure contract** | 即使 brief 中写明「失败时报告 blocker」，模型仍可能在 denied action 后陷入重试。 | failure contract 与 daemon 的 `policy_denials.json` 联动；达到阈值后主动注入 policy block 提示。 |
| **评估指标被操纵** | worker 可能通过少做工具调用来降低 `turn_count` 和 `redundant_reads`，但报告质量下降。 | 必须保留 `task_success` 和人工裁判作为最终指标。 |
| **会话记忆污染** | Claude Code 的 session 可能保留上次的上下文，实验间需隔离。 | 每次使用新 `claude_session_id`；实验前校验 session 状态。 |
| **Brief 编译器自身的幻觉** | 模型生成的 brief 可能包含不存在的路径或错误的规则。 | 所有 `relevant_facts` 必须带 `source` 和 `confidence`；daemon 验证 source 存在性。 |
| **冲突报告处理** | 多个 worker 对同一主题给出矛盾结论时，harness 需要仲裁机制。 | 引入 `conflicted` 状态；重大冲突保留多版本并提示 Codex。 |

---

## 11. 模型生成的 Context Exposure Window 是否减少了探索时间

**结论：是的，明显减少了，但效果取决于 brief 的结构化程度和契约完整性。**

具体表现：

1. **目标聚焦**：Context Exposure Window 明确把任务定位为「为 Worker Brief Compiler 设计量化评估 harness」，worker 不需要从「上下文是什么」开始重新推导。
2. **关键模块已知**：Supervisor Context 中已说明 ScopeFilter / RuleExtractor / FactSelector / RepoNavigator / ContractCompiler / BriefRenderer 六个模块，以及 WorkerBrief/ContextPacket、ProjectMemory、DecisionLog 三个数据对象，worker 可以直接引用 A/B/C 组报告和 v7 研究，而不必重新 grep 整个仓库找相关文件。
3. **评估维度明确**：提示已列出必须量化的维度（brief size、source count、worker drift、report contract completion、time-to-report、quality），报告的框架可以直接围绕这些维度展开。
4. **边界清晰**：Worker Context 中给出 `worker_kind: report`、只写 report/scratch、Bash readonly，worker 不会误入实现修改或越界写入。
5. **剩余成本**：仍然需要确认当前 `routes.rs` 的 handoff prompt 基线、阅读 A/B/C 组报告来对齐术语（如 `ContextPacket` vs `WorkerBrief`）、以及设计具体的指标计算公式。但这些确认性阅读远短于从仓库摸索出整个 v7 上下文模型。

与 A 组（无预编译背景）相比，本任务不需要从头阅读 `routes.rs`、`hooks.rs`、`projection.rs` 来理解上下文生成机制；与 B 组（短人工背景）相比，本任务额外获得了「评估 harness」这一具体焦点的定位，以及「模型生成上下文」本身作为被评估对象的明确边界，因此探索范围更窄、更可控。

---

## 12. 下一步建议

1. **实现最小可复现 harness**：`tests/test_context_window_metric.py` 先跑 HCM-01/HCM-02/HCM-04 三个任务，对比基线 A 与模型生成上下文 C3。
2. **冻结评估指标权重**：先用本报告提出的 100 分制跑 1-2 轮，根据结果调整权重，避免主观赋分。
3. **实现结构化 brief 文件对**：在 `routes.rs` 旁新增 `brief_compiler.rs`（或 Python 脚本），先实现 BriefInputs + ScopeFilter + ContractCompiler 三个模块，RepoNavigator 用文件列表占位。
4. **建立 golden answer 数据集**：为 HCM-01 至 HCM-08 准备人工裁判用的标准答案和评分要点。
5. **定义 `.agentcall/context/fact_schema.json`**：把「已验证事实」结构化，而不是写在 Markdown 里，便于 FactSelector 按 path/tag 匹配。
6. **跑完 3 轮 A/B/C 后**，根据 `score` 和具体指标决定哪些模块需要增强，再扩展 benchmark 到全部 8 个任务。

---

## 13. 结论

模型生成的 Context Exposure Window 为 AgentCall v7 的 Worker Brief Compiler 提供了清晰的方向，但「方向正确」不等于「自动有效」。评估 harness 应围绕 **任务成功率、边界遵守、冗余读取、契约遵守、brief 质量、时间效率** 六个维度，用受控 A/B/C 实验在真实 daemon 上收集客观指标。当前最紧迫的是把评估框架代码化，避免在缺乏度量的情况下直接重写上下文生成逻辑。

本报告提出的 100 分制评分表、8 个基准任务、完整的数据采集方案、以及明确的验收阈值，可以作为 v7 Worker Brief Compiler 的度量基线。建议先实现最小 harness，再迭代 brief compiler 本身。
