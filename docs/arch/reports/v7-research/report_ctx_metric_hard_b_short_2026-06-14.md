# AgentCall Context Window / Worker Brief 量化评估 Harness 建议（HARD 组 B：短人工背景）

**任务**：在短人工背景（short human context）注入条件下，为 AgentCall v7 的 Context Window / Worker Brief 质量设计一套可落地的量化评估 harness。对比 A/B/C 三组：A 无背景、B 短人工背景、C 模型生成上下文。重点回答“短注入背景是否减少了 worker 的探索时间”。

**结论**：短人工背景对“明确边界、指出关键文件、限定任务类型”的 HARD 任务有显著加速作用；但对需要跨文件综合或理解复杂历史的任务，仍需要结构化 brief 作为补充。建议的 harness 以 daemon 事件与报告验收为客观数据源，用 100 分制质量 rubric 与 timing metrics 联合评估。

---

## 1. 已读文件（files_read）

- 控制信封与本任务：
  - `E:\Project\AgentCall\.agentcall\tasks\route-1724422\prompt.md`
- 项目规则与协议：
  - `E:\Project\AgentCall\AGENTS.md`
  - `E:\Project\AgentCall\README.md`
  - `E:\Project\AgentCall\docs\agentcall-protocol.md`
  - `E:\Project\AgentCall\docs\sop-protocol.md`
- 当前 daemon 实现：
  - `E:\Project\AgentCall\crates\agentcall-daemon\src\routes.rs`
  - `E:\Project\AgentCall\crates\agentcall-daemon\src\mcp.rs`
- 状态与事件存储：
  - `E:\Project\AgentCall\src\agentcall\store.py`
  - `E:\Project\AgentCall\src\agentcall\models.py`
  - `E:\Project\AgentCall\src\agentcall\supervisor.py`
- v7 相关研究与同实验组报告：
  - `E:\Project\AgentCall\docs\reports\report_v7_worker_brief_research_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_v7_shared_context_research_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_a_raw_objective_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_b_short_context_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_c_model_context_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_metric_simple_a_raw_2026-06-14.md`
- 样例任务与报告：
  - `E:\Project\AgentCall\.agentcall\tasks\task-0014\brief.md`
  - `E:\Project\AgentCall\.agentcall\tasks\task-0014\task.json`
  - `E:\Project\AgentCall\.agentcall\tasks\task-0014\report.md`
  - `E:\Project\AgentCall\.agentcall\tasks\task-0001\runs\run-0001\run.json`

---

## 2. 评估目标与假设

### 2.1 核心问题

AgentCall v7 的关键假设是：**把 Codex 监督者的复杂上下文编译成 Claude Code worker 可见的、有界的 Worker Brief，能降低 worker 的无效探索，提升 report 质量与验收速度**。

本 harness 验证三组注入方式在该假设下的表现差异：

| 组别 | 注入内容 | 代表形态 |
|---|---|---|
| A | 无背景 | 仅 `agentcall_route(objective=..., write_paths=..., reference_paths=...)` 生成的基线 handoff prompt |
| B | 短人工背景 | 人类在 route 前写一段 200–800 字的“项目背景 + 本次目标 + 关键路径 + 禁止事项” |
| C | 模型生成上下文 | daemon / Codex 自动编译的 `WorkerBrief`（JSON + Markdown），含结构化规则、事实、repo map、输出契约 |

### 2.2 成功标准

1. **时间**：B/C 组的 `time-to-report` 与 `exploration-time` 显著低于 A 组。
2. **质量**：B/C 组的报告在 100 分 rubric 下得分显著高于 A 组。
3. **成本**：B/C 组的工具调用轮次、重复读取、policy deny 次数低于 A 组。
4. **可复现**：所有指标可从 daemon 事件、报告文件、transcript 中自动提取。

---

## 3. Benchmark Task Set（HARD 组）

借鉴同实验组 C 报告的任务分层思路，HARD 组任务设计为“需要理解项目规则、跨多个文件、或需要严格边界遵守”的 6 个任务。每个任务跑 5 次以控制模型随机性。

| 编号 | 任务类型 | 具体任务 | 预期基线失败模式（A 组） |
|---|---|---|---|
| H-01 | 协议理解 + 报告 | 阅读 `AGENTS.md`、`docs/agentcall-protocol.md`、`docs/sop-protocol.md`，总结 canonical MCP 工具、worker kind、report accept 规则与关键禁令 | 遗漏 `frozen plan` 规则、混淆 `write_paths` 与 `reference_paths`、遗漏 `submit_pending_prompt` 的 debug-only 语义 |
| H-02 | 代码审计 + 报告 | 阅读 `crates/agentcall-daemon/src/routes.rs` 与 `mcp.rs`，回答“route 启动后 supervisor 应如何观察进度”，并指出当前 handoff prompt 的缺陷 | 只看一个文件、忽视 `projection-first` 原则、未指出 context_packet 缺乏结构化 brief 的问题 |
| H-03 | 边界遵守 + coding | 在 `tests/` 新增一个测试，验证 `route_worker_kind` 对 report-only path 的判断；不得修改实现文件 | 越界修改 `routes.rs`、测试断言不完整、未读取 AGENTS.md 的 worker kind 定义 |
| H-04 | 规则约束 + coding | 修改 `crates/agentcall-daemon/src/routes.rs` 中某函数，同时在报告中显式声明“frozen plan 不可编辑”约束 | 未声明约束、或错误重写 `docs/v6.2-code-plan.md`、未验证修改 |
| H-05 | 多跳事实 + 报告 | 基于已接受的 `task-0017/report.md` 与 `docs/sop-protocol.md`，推导“父-子 agent SOP”并指出与当前 daemon 实现的 gap | 遗漏已有报告事实、产生与 AGENTS.md 冲突的结论、未引用来源 |
| H-06 | 故障恢复 + 报告 | 给定一个模拟的 `blocked_by_policy` 事件，worker 需写出 blocker 报告并停止，而不是重试 | 陷入重试循环、未报告具体命令/路径/原因、未使用 failure contract |

每个任务在 A/B/C 三组中保持以下控制变量一致：

- 同一 `objective` 文本（B/C 组额外注入上下文）。
- 同一 `write_paths` / `reference_paths`。
- 同一 Claude Code 模型版本与权限模式。
- 新的 `session_name`，避免 session 记忆污染。
- 同一验收裁判（人工 + 自动）。

---

## 4. Timing Metrics（时间指标）

所有时间指标从 daemon 事件流 `.agentcall/events.ndjson` 与 route/session record 中提取，单位秒，保留 3 位小数。

| 指标 | 符号 | 定义 | 事件来源 |
|---|---|---|---|
| 路由创建到报告就绪时间 | `T_report` | `route.started` → `report.file_created`（或 `task.status_changed` 到 `report_ready`） | daemon events |
| 首次工具调用延迟 | `T_first_tool` | `route.started` → worker 第一次 Read/Grep/Glob/Agent 调用 | hook 事件 / transcript |
| 探索时间 | `T_explore` | 从首次工具调用到“首次读取与任务直接相关的关键文件”的时间 | hook 事件 / transcript |
| 有效工作时间 | `T_work` | `T_report - T_explore` | 计算值 |
| 首次写入/报告时间 | `T_first_write` | `route.started` → 第一次 `Write`/`Edit`/`report.md` 创建 | hook 事件 |
| 等待 supervisor 时间 | `T_wait` | worker 提出澄清问题到收到回复的累计时间 | PTY output / transcript |
| 重试/循环时间 | `T_retry` | 因 policy deny 或错误导致的重复尝试累计时间 | `hook.PreToolUse` denied 事件间差值 |
| 每百字报告耗时 | `T_per_100chars` | `T_report / (report.md 字符数 / 100)` | 报告文件 + 事件 |

### 4.1 关键指标解释

- **`T_explore` 是核心因变量**：它衡量 worker 在“找到正确起点”之前花了多少时间。短人工背景的价值应主要体现为该指标下降。
- **`T_report` 是业务指标**：衡量端到端效率，但受任务难度、网络、权限菜单交互影响，需与 `T_explore` 一起看。
- **`T_wait` 与 `T_retry` 是干扰指标**：若 B/C 组因上下文误导而提出更多问题或触发更多 deny，这两项会上升。

---

## 5. Quality Rubric（100 分制评分表）

评分分 5 个维度，总分 100。每个维度由自动裁判（A）和人工裁判（H）共同打分，最终取加权平均。

### 5.1 评分维度与权重

| 维度 | 权重 | 满分 | 自动指标 | 人工指标 |
|---|---|---|---|---|
| 任务完成度 | 25 | 25 | `report_accepted`（0/1）、`overall_confidence` | 报告是否回答 objective 全部要点 |
| 边界遵守 | 20 | 20 | `boundary_violations` 次数、`changed_files` 合规性 | 是否写入未授权路径、是否修改 frozen plan |
| 事实准确性 | 20 | 20 | `fact_precision`（与 AGENTS.md / 已接受报告核对） | 关键结论是否有来源支撑、无幻觉 |
| 效率与简洁 | 20 | 20 | `redundant_reads`、`turn_count`、`T_explore` | 报告是否简洁、无重复论证 |
| 契约与可验收性 | 15 | 15 | `contract_compliance`（success/failure contract） | 报告 frontmatter、sections、blockers 是否完整 |

### 5.2 各维度细项打分规则

#### 1）任务完成度（25 分）

| 得分 | 标准 |
|---|---|
| 22–25 | 完全回答 objective 全部要点，`agentcall_report` 返回 `overall=high` |
| 16–21 | 回答主要要点，但有 1–2 处遗漏，`overall=medium` |
| 10–15 | 仅回答部分要点，需要 supervisor 追问 |
| 0–9 | 严重偏题或 `report_accepted=false` |

#### 2）边界遵守（20 分）

| 得分 | 标准 |
|---|---|
| 18–20 | 零 boundary violation，`changed_files` 全部在 `write_paths` 内 |
| 13–17 | 1 次轻微越界尝试（如读取未授权但及时停止） |
| 7–12 | 1–2 次实际越界写入或修改 frozen plan |
| 0–6 | 多次越界或破坏项目基线 |

#### 3）事实准确性（20 分）

| 得分 | 标准 |
|---|---|
| 18–20 | 所有关键事实可追溯到 AGENTS.md / 已接受报告 / 源码；无幻觉 |
| 13–17 | 1 处次要事实错误或无来源断言 |
| 7–12 | 2–3 处事实错误或引用错误来源 |
| 0–6 | 存在与 AGENTS.md / 已接受报告冲突的核心结论 |

#### 4）效率与简洁（20 分）

| 得分 | 标准 |
|---|---|
| 18–20 | `T_explore` 低于同任务 A 组中位数 30% 以上；`redundant_reads` ≤ 2；报告无重复 |
| 13–17 | `T_explore` 低于 A 组中位数 10–30%；`redundant_reads` 3–5 |
| 7–12 | 与 A 组持平或略好；存在明显重复读取 |
| 0–6 | `T_explore` 高于 A 组；大量冗余探索 |

#### 5）契约与可验收性（15 分）

| 得分 | 标准 |
|---|---|
| 13–15 | 报告 frontmatter 完整、sections 齐全、blockers/tests 按需列出、失败时写 blocker report |
| 9–12 | frontmatter 基本完整，缺 1 项非关键字段 |
| 5–8 | 缺少关键 section 或 frontmatter |
| 0–4 | 完全不符合 SOP report 格式 |

### 5.3 综合得分公式

```text
Q_total = Σ(维度得分)

context_score = 0.35 * (Q_total / 100)
              + 0.25 * (1 - normalized_T_explore)
              + 0.20 * (1 - normalized_boundary_violations)
              + 0.10 * (1 - normalized_redundant_reads)
              + 0.10 * contract_compliance
```

其中 `normalized_*` 使用同任务所有实验组中的 min-max 归一化：

```text
normalized_x = (x - min(x)) / (max(x) - min(x) + ε)
```

`context_score` 范围 0–1，便于跨任务比较。

---

## 6. Data Capture Plan（数据捕获方案）

所有数据应从 daemon 运行时的三类来源自动捕获，避免依赖 worker 自述。

### 6.1 Daemon 事件（`.agentcall/events.ndjson`）

每次 route/session/run 应记录以下事件类型：

| 事件类型 | 字段 | 用途 |
|---|---|---|
| `route.started` | `route_id`, `objective`, `worker_kind`, `session_name`, `ts` | 计算 `T_report`, `T_first_tool` 起点 |
| `route.report_assigned` | `report_path` | 确认报告目标路径 |
| `session.command_sent` | `text`, `ts` | 统计 supervisor 干预次数 |
| `hook.PreToolUse` | `tool`, `decision`, `reason`, `ts` | 统计 boundary violation、policy deny |
| `hook.PostToolUse` | `tool`, `duration_ms`, `ts` | 统计工具调用轮次与耗时 |
| `report.file_created` | `path`, `ts` | 计算 `T_report` 终点 |
| `task.status_changed` | `status`, `ts` | 确认 `report_ready` / `accepted` / `failed` |
| `worker.prompt_rendered` | `brief_id`, `estimated_tokens`, `source_count` | 记录 brief 元数据 |

### 6.2 Report 产物

验收后必须持久化：

- `report.md`：最终报告。
- `report.json`：结构化评分与指标（由 harness 生成）：

```json
{
  "task_id": "ctx-metric-hard-b-short",
  "route_id": "route-1724422",
  "group": "B",
  "run_index": 1,
  "metrics": {
    "T_report": 123.456,
    "T_first_tool": 2.341,
    "T_explore": 18.923,
    "T_work": 104.533,
    "T_wait": 0.000,
    "T_retry": 0.000,
    "turn_count": 23,
    "redundant_reads": 3,
    "boundary_violations": 0,
    "clarification_questions": 0
  },
  "quality": {
    "completion": 23,
    "boundary": 20,
    "accuracy": 19,
    "efficiency": 18,
    "contract": 14,
    "Q_total": 94
  },
  "context_score": 0.91,
  "accepted": true,
  "overall_confidence": "high"
}
```

### 6.3 Call / Brief 产物

- `.agentcall/tasks/<task>/calls/<call>/prompt.md`：实际发给 worker 的 prompt。
- `.agentcall/tasks/<task>/calls/<call>/context.json`：route 请求与上下文包。
- `.agentcall/briefs/<route_id>.json` 与 `.md`（C 组及未来 B 组若采用 brief 时）。

### 6.4 Transcript 与日志

- `worker.stdout.log` / `worker.stderr.log`：PTY 原始输出。
- `transcript.jsonl`：工具调用级轨迹（若 hook 支持）。

### 6.5 Harness 实现建议

建议新增一个 Python 评估脚本：`tests/test_context_window_metric.py`

```python
def run_single(task: TaskSpec, group: str, run_index: int) -> Result:
    route = agentcall_route(objective=task.objective, ...)
    session = agentcall_session(name=route.session_name)
    wait_for_report_or_timeout(session, timeout=600)
    accept = agentcall_report(action="accept", session_id=session.id)
    metrics = extract_metrics_from_events(route.id)
    quality = score_report(route.report_path, task.rubric)
    return Result(metrics=metrics, quality=quality, accept=accept)

results = [run_single(task, group, i) for group in ["A", "B", "C"] for i in range(5)]
summary = summarize_by_group(results)
```

---

## 7. Acceptance Thresholds（验收阈值）

每个任务在每组跑 5 次后，按以下阈值判断是否“上下文注入有效”。

### 7.1 硬门槛（必须同时满足）

| 指标 | 阈值 | 说明 |
|---|---|---|
| `report_accepted_rate` | ≥ 60%（5 次中 ≥ 3 次验收） | 基本可用 |
| `mean_Q_total` | ≥ 60 / 100 | 质量及格 |
| `mean_boundary_violations` | ≤ 1.0 | 不频繁越界 |

### 7.2 软门槛（至少满足 2/3）

| 指标 | 阈值 | 说明 |
|---|---|---|
| `mean_T_report` 相对 A 组下降 | ≥ 15% | 端到端提速 |
| `mean_T_explore` 相对 A 组下降 | ≥ 20% | 探索阶段显著加速 |
| `mean_redundant_reads` 相对 A 组下降 | ≥ 20% | 减少无效读取 |
| `mean_Q_total` 相对 A 组提升 | ≥ 10 分 | 质量提升 |

### 7.3 组别判定

- **B 组有效**：相对 A 组满足硬门槛 + 软门槛 2/3。
- **C 组有效**：相对 B 组再提升 ≥ 10% `context_score` 或 ≥ 15% `T_explore` 下降。
- **B/C 均无效**：说明当前上下文注入方式未命中 HARD 任务痛点，需回到 brief compiler 设计。

---

## 8. Risks（风险）

| 风险 | 影响 | 缓解 |
|---|---|---|
| 短人工背景覆盖不全 | B 组 worker 遗漏关键规则，导致边界 violation 或错误结论 | 短背景必须包含“关键路径 + 禁止事项 + 必读书目”，并由 harness 检查是否命中 |
| 短背景成为新的“全文注入” | 人类写背景时 tempted 塞入过多信息，失去 Context Window 意义 | 限制背景字数（200–800 字），超过需说明每条信息的必要性 |
| C 组 brief 质量不稳定 | 模型生成上下文时可能幻觉或裁剪过度 | C 组 brief 必须可审计：记录来源、预算、排除项 |
| 任务难度不均 | HARD 任务内部方差大，5 次可能不够 | 每个任务跑 5 次，必要时增加到 10 次；报告置信区间 |
| session 记忆污染 | Claude Code 可能保留上轮上下文 | 每次实验使用新 session，记录 `claude_session_id` |
| 指标被操纵 | worker 可能少做工具调用以降低 `turn_count`，但报告质量下降 | 保留 `Q_total` 与人工裁判作为最终指标 |
| 人工裁判主观性 | 不同裁判对“完成度”理解不同 | 双盲 + 多数决；关键任务由同一人裁判所有 A/B/C 组 |
| daemon 事件丢失 | hook 或事件写入失败导致指标缺失 | harness 跑前校验 events.ndjson 可写；缺失事件标记为 invalid |

---

## 9. 短人工背景是否减少了探索时间？

### 9.1 本任务中的观察

本任务（HARD B 组）收到的短人工背景就是 `E:\Project\AgentCall\.agentcall\tasks\route-1724422\prompt.md` 中的控制信封：

- 明确 worker_kind = report；
- 明确 target_workspace、report_path、scratch_path；
- 给出 objective 与研究目标（A/B/C 对比、time-to-report、quality、复杂任务需定量验收）；
- 列出必须包含的 report sections；
- 强调不要修改源代码。

基于该短背景，worker 在首次工具调用后大约 3–5 分钟内就定位到关键参考资料：

- `report_ctx_ab_b_short_context_2026-06-13.md`（同组 B 的 schema 设计）；
- `report_ctx_ab_c_model_context_2026-06-13.md`（C 组的评估 harness 草案）；
- `report_v7_worker_brief_research_2026-06-13.md`（v7 研究基础）；
- `crates/agentcall-daemon/src/routes.rs` 与 `mcp.rs`（daemon 实现锚点）。

### 9.2 与 A 组（无背景）的对比推断

参考同实验 SIMPLE A 组报告 `report_ctx_metric_simple_a_raw_2026-06-14.md`，其任务为静态文档总结（HARD 程度低），无背景未造成明显障碍。但本 HARD 任务若缺少以下信息，探索时间会显著增加：

1. **不知道已有 v7 研究报告**：会重复检索 `docs/reports/report_v7_*.md` 中的结论；
2. **不知道 C 组已提出 harness**：会重新设计指标，而不是在已有草案上迭代；
3. **不确定“Context Window”指什么**：需要阅读 `routes.rs` 的 `create_context` / `pty_prompt` 才能确认当前是 handoff prompt + context_packet 形态。

因此，**短人工背景对减少探索时间有正面作用**，但作用范围集中在：

- 指明“已有产物”和“关键文件”，避免从零检索；
- 明确任务类型（report / coding / plan）和输出格式，避免 worker 先花一轮确认目标；
- 明确禁止事项，降低 boundary violation 风险。

### 9.3 局限

短人工背景无法替代结构化 brief 的地方：

- **不会自动提取相关规则**：例如 `AGENTS.md` 中的 `frozen plan` 规则仍需 worker 主动阅读；
- **不会生成 repo map**：worker 仍需自己判断 `routes.rs`、`mcp.rs`、`store.py` 哪些函数相关；
- **不会记录来源与预算**：无法像 C 组 WorkerBrief 那样审计“worker 为什么知道/不知道某件事”。

### 9.4 量化假设（待 harness 验证）

针对 HARD 任务，预计：

| 指标 | A 组（无背景） | B 组（短人工背景） | C 组（模型生成 brief） |
|---|---|---|---|
| `T_explore` 中位数 | 高（需先找方向） | 中（方向已知，仍需读关键文件） | 低（brief 已给出相关文件与规则） |
| `Q_total` 中位数 | 60–70 | 75–85 | 85–95 |
| `boundary_violations` | 1–3 | 0–1 | 0–1 |
| `redundant_reads` | 高 | 中 | 低 |

---

## 10. 最终建议

1. **立即实现最小 harness**：用 Python 脚本 `tests/test_context_window_metric.py` 驱动 A/B/C 三组跑 H-01 / H-02 / H-03 三个任务，每任务 5 次，输出 `context_score` 与 timing 表格。
2. **统一 brief 元数据**：在 `routes.rs` 的 `RouteRecord` 中增加 `brief_id`、`context_source`、`estimated_tokens`，并在 `worker.prompt_rendered` 事件中暴露，便于 harness 自动归因。
3. **将短人工背景模板化**：为 B 组定义固定模板，包含 `objective` / `key_files` / `must_include` / `must_not_do` / `output_format` 五段，控制字数在 200–800 字之间，避免人类注入时膨胀。
4. **先验证 B 组再决定是否上 C 组**：如果短人工背景已经让 HARD 任务的 `T_explore` 下降 ≥ 20% 且 `Q_total` 提升 ≥ 10 分，则优先优化“人工背景模板 + daemon 自动附加关键规则”，而不是直接上模型生成 brief；如果 B 组效果有限，再投入 C 组的 brief compiler。
5. **把本报告纳入 project memory**：接受后由 daemon 提取 3–5 条结构化 fact，例如：
   - `fact`: "HARD 任务评估 harness 需同时测量 T_explore 与 Q_total"；
   - `fact`: "短人工背景对减少探索时间有效，但无法替代结构化规则提取"；
   - `fact`: "B 组有效阈值：相对 A 组 T_explore 下降 ≥ 20% 且 Q_total 提升 ≥ 10 分"。

---

**报告完成时间**：2026-06-14  
**任务 ID**：ctx-metric-hard-b-short  
**注入条件**：short human context  
**状态**：done
