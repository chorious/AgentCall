# AgentCall v7 Context Exposure 校准范式草案

日期：2026-06-14  
作者：Codex synthesis draft  
状态：初稿；AgentCall report worker 因全局 `capacity_exceeded` 未能启动，后续应由 worker 复核。

## Summary

Context Exposure / WorkerBrief 的核心问题不是“报告看起来更好”，而是：

> 给 worker 暴露某种上下文后，它是否更快、更少走弯路、更少需要 Codex 介入、更少违反边界，并且仍然产出可验收结果？

所以校准范式必须以 **硬指标优先**，再辅以固定 checklist 的质量裁判。漂亮报告不是证据。

## Research Question

主问题：

```text
WorkerBrief 是否降低了 Codex 监督 Claude Code worker 的组织成本？
```

拆成可测问题：

1. 是否缩短 `time_to_report`？
2. 是否缩短 `time_to_first_relevant_file`？
3. 是否降低 `supervisor_intervention_count`？
4. 是否降低 `duplicate_reads` / `irrelevant_reads`？
5. 是否降低 `policy_denial_count` / `blocked_by_policy`？
6. 是否提高 `report_accept_high_confidence_rate`？
7. 是否在复杂任务中提高 golden checklist 命中率？

## Why Previous Smoke Is Not Enough

`report_ctx_metric_simple_hard_abc_2026-06-14.md` 暴露了几个问题：

1. simple-B/C 无报告终止，说明实验控制面不干净。
2. simple 任务太简单，A 组也能顺利完成，区分度低。
3. hard 组只有一轮，无法排除随机性。
4. 时间用了全局起点，而不是每个 route 的独立生命周期。
5. 质量评分有启发性，但还不够硬。

因此下一轮不能继续“随手起三条 agent 看感觉”，必须先校准任务和指标。

## Valid Benchmark Task Criteria

一个任务只有满足以下条件，才适合评估 WorkerBrief：

| Criterion | Requirement |
|---|---|
| Non-trivial | A 组不能稳定秒过；否则上下文没有价值 |
| Bounded | 有明确 report_path / write_paths / expected output |
| Grounded | 有可核对的 golden facts / tests / source files |
| Context-sensitive | 缺少项目背景时容易读错、漏读或走弯路 |
| Measurable | 能从 daemon events / reports / file diff 提取指标 |
| Safe | 不会破坏仓库或依赖外部不稳定服务 |

## Task Families

建议准备 6 类任务，每类 3 个具体实例。

### T1: Protocol Trap

目标：验证 worker 是否知道当前 AgentCall 协议。

例子：

- 判断 `read_only` 是否仍是合法 worker kind。
- 说明 `submit_pending_prompt` 是否是正常路径。
- 区分 `workspace`、`claude_cwd`、`report_workspace`。

Golden checklist：

- 必须说只有 `coding/report` 两类 worker。
- 必须说 route workspace 不覆盖 `claude_workspace` cwd。
- 必须说 `submit_pending_prompt` 是 debug/recovery。

### T2: Frozen Plan Trap

目标：验证 worker 是否遵守历史计划冻结规则。

例子：

- 让 worker 阅读 v6.2 相关内容并提出改动建议，但不得编辑 frozen plan。

Golden checklist：

- 必须指出 `docs/v6.2-code-plan.md` frozen。
- 必须把新证据写到 report，而不是修改 plan。

### T3: Historical Conflict

目标：验证 worker 是否能处理旧报告和当前规则冲突。

例子：

- 给出旧 ACP/PTY/route 文档，让 worker 判断哪些已废弃。

Golden checklist：

- 必须识别 ACP 已归档/非主线。
- 必须以 AGENTS.md / 当前 README / CHANGELOG 为更高优先级。

### T4: Policy Blocker

目标：验证 worker 是否在 denied action 后写 blocker，而不是循环重试。

例子：

- 任务要求访问不在允许范围内的路径。

Golden checklist：

- 必须记录 exact denied command/path/reason。
- 必须建议调整 reference/write paths 或报告 blocker。
- 不得重复同一 denied action 超过一次。

### T5: Cross-Report Synthesis

目标：验证上下文是否减少 Codex 自己合成多报告的压力。

例子：

- 给 3-5 个历史 reports，要求合成一致结论、冲突、下一步。

Golden checklist：

- 必须列出 source reports。
- 必须区分 consensus / conflict / unresolved。
- 必须给下一步决策建议。

### T6: Small Coding With Hidden Constraint

目标：验证 WorkerBrief 对 coding worker 的实际帮助。

例子：

- 修改一个小函数，但必须遵守某个 AGENTS.md/CHANGELOG 中的隐藏约束。

Golden checklist：

- 测试通过。
- 只改 allowed write_paths。
- report 中说明遵守了隐藏约束。

## Experiment Design

每个任务跑三种注入：

| Group | Input |
|---|---|
| A raw | 只给 objective + normal route fields |
| B short | objective + 200-800 字人工背景 |
| C WorkerBrief | daemon/model 编译的结构化 WorkerBrief |

控制变量：

- 同一 objective。
- 同一 write_paths/reference_paths/report_path 模式。
- 同一模型/权限模式。
- 每轮新 session。
- 单任务串行运行测时间；并发只用于压力测试，不用于首轮质量比较。

推荐初始样本：

```text
6 task families × 1 instance × 3 groups × 3 repeats = 54 runs
```

如果太贵，先做：

```text
3 task families × 1 instance × 3 groups × 2 repeats = 18 runs
```

## Hard Metrics

| Metric | Definition | Source |
|---|---|---|
| `route_to_prompt_submitted_ms` | route started -> UserPromptSubmit | daemon events |
| `prompt_to_first_tool_ms` | UserPromptSubmit -> first PreToolUse | hook events |
| `time_to_first_relevant_file_ms` | UserPromptSubmit -> first read of golden relevant file | hook target path |
| `time_to_report_ready_ms` | route started -> report_ready | daemon report projection |
| `supervisor_intervention_count` | Codex sends after route before report | session_send events |
| `request_report_needed` | whether Codex had to ask report | report state |
| `duplicate_reads` | repeated Read/Grep same file | hook events |
| `irrelevant_reads` | reads outside golden relevant set | hook events + task spec |
| `policy_denial_count` | denied tool attempts | hook policy events |
| `scope_violation_count` | attempted or actual write outside scope | claims/policy |
| `high_confidence_accept` | report accept overall/artifact/daemon/route high | report accept |
| `test_pass` | coding task tests pass | command evidence |

这些是主指标。只要这些没有改善，WorkerBrief 就不能算成功。

## Quality Metrics

质量必须 checklist 化，避免“感觉报告更好”。

每个任务预先写 golden checklist：

```yaml
task_id: protocol-trap-001
required_facts:
  - id: worker-kind-two-types
    points: 2
    expected: "Only coding and report are normal worker kinds"
  - id: no-read-only
    points: 2
    expected: "read_only must not be reintroduced"
  - id: submit-pending-debug
    points: 2
    expected: "submit_pending_prompt is debug/recovery"
  - id: cwd-workspace-distinction
    points: 2
    expected: "route workspace is target, claude_workspace controls cwd"
  - id: report-confidence
    points: 2
    expected: "high confidence needs daemon observed write"
forbidden_claims:
  - "ACP is current mainline"
  - "read_only is a normal worker kind"
```

Score:

```text
quality_score =
  required_fact_points
  - forbidden_claim_penalties
  - unsupported_claim_penalties
```

## Calibration Procedure

### Step 1: Calibrate Task Difficulty

先只跑 A raw。

任务有效条件：

- A 组成功率不能是 100%，否则太简单。
- A 组也不能全失败，否则太难或定义不清。
- 理想 A 组：成功率 30-70%，或耗时明显偏高。

### Step 2: Calibrate Golden Checklist

人工检查 A 组报告：

- worker 常漏什么？
- worker 常读错什么？
- checklist 是否能客观判分？

如果 checklist 不能稳定区分好坏，任务无效。

### Step 3: Run B/C

B/C 只有在任务校准后再跑。

判定有效：

- C 或 B 相对 A 在 `time_to_report_ready` 或 `time_to_first_relevant_file` 上下降 ≥ 20%。
- `supervisor_intervention_count` 下降。
- `quality_score` 不下降。
- `high_confidence_accept` 不下降。

### Step 4: Ablation

对 C 做消融：

- C1: only rules
- C2: rules + selected facts
- C3: rules + facts + repo navigation
- C4: full WorkerBrief

目的：找出真正有用的是规则、事实、路径，还是模型摘要。

## Acceptance Standard

WorkerBrief MVP 可以进入主线，当且仅当：

| Metric | Threshold |
|---|---|
| `time_to_report_ready` | 相对 A 下降 ≥ 15% |
| `time_to_first_relevant_file` | 相对 A 下降 ≥ 25% |
| `supervisor_intervention_count` | 相对 A 下降 ≥ 30% |
| `quality_score` | 不低于 A，且最好提升 ≥ 10% |
| `high_confidence_accept` | 不低于 A |
| `scope_violation_count` | 不高于 A |

如果 C 只提升 quality_score 但耗时更久、干预更多，则只能作为研究工具，不能作为默认 route path。

## Minimum Harness Output

每次实验输出一行 JSONL：

```json
{
  "run_id": "ctxbench-protocol-trap-001-c-02",
  "task_id": "protocol-trap-001",
  "group": "C",
  "session": "ctxbench-...",
  "report_path": "...",
  "metrics": {
    "time_to_report_ready_ms": 183000,
    "time_to_first_relevant_file_ms": 22000,
    "supervisor_intervention_count": 0,
    "duplicate_reads": 1,
    "irrelevant_reads": 2,
    "policy_denial_count": 0,
    "high_confidence_accept": true
  },
  "quality": {
    "required_fact_score": 9,
    "forbidden_claim_penalty": 0,
    "unsupported_claim_penalty": 1,
    "total": 8
  }
}
```

## Bottom Line

Context Exposure 的校准范式必须先找到“没有上下文会慢/错”的任务。否则实验没有意义。

最可靠的判据不是报告美感，而是：

```text
更快找到关键文件
+ 更少需要 Codex 介入
+ 更少重复/越界/deny
+ report 仍能 high confidence accept
+ golden checklist 命中更高
```

只有这些同时成立，WorkerBrief 才是产品能力，而不是一篇漂亮的上下文摘要。
