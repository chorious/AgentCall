# Review — ACP 定位重构：从 auto 打分到 SOP 模板门

Reviewer: Opus 4.8 · 日期: 2026-06-04
关联: `plan_review_v2.2_*`、`acp_python_vs_rust_OPUS.md`、`python_debt_audit_OPUS.md`、CHANGELOG `v0.8a/v0.8b`
状态: 方向认可，待拍板执行细节

---

## 0. 一句话结论

ACP 不应是自由形态 runtime，而应是**模板化、短生命周期、报告驱动、可拒绝的 SOP worker**。
`route` 不再判断"任务小不小"（一个开工前不可测的连续量），改为对**离散的 SOP 模板做硬校验**。
Codex 不需要估算规模，只需要**选模板**；选不出 → 不 ACP，直接 PTY 或 parent 自己做。

---

## 1. 为什么放弃 "auto 打分判断 size"

当前 `route_decision`（`crates/agentcall-daemon/src/routes.rs:319-371`）拿调用方填的 `estimated_minutes / estimated_files / estimated_loc / needs_continuity / risk` 做加减打分，pty_score > acp_score 就走 PTY。

两个根本问题：

1. **route 没有判断能力**。它只是把 Codex 的猜测做算术，再把结果伪装成决策。`estimated_minutes>=20 → +2` 是在**垃圾输入上做精密运算**（false precision）。
2. **"任务多大"开工前不可知**。探索代码库之前估 size 误差经常 >100%：看着像 typo 的活会变成 12 文件重构。把 `estimated_files=1` 当 ground truth，整个路由建在沙子上。

**核心判断**：用不可测量的连续量（size）当边界判据 = 边界设计无从落地。换成**离散、可硬校验的属性（能否塞进已知 SOP 模板）**，正好喂给 route 它唯一擅长的活——schema 校验与 deny。

---

## 2. 新定位

| Runtime | 定位 |
|---|---|
| **ACP** | 模板化、短生命周期、报告驱动、可拒绝的 SOP worker |
| **PTY** | 自由探索、长生命周期、可视化 handoff、复杂协作 worker |

判据从 "是否够小" 变为 **"能否表达为一份固定 SOP 契约"**——这是 Codex 真能回答的 yes/no，因为它问的是"这活说清楚了没 / 能不能套模板"，不是"这活多大"。

---

## 3. 命门：模板必须有"牙齿"，不能只是输入槽位

> 这是整套设计成立或垮掉的支点。

输入槽位（`target_files / allowed_paths / max_reads / max_writes / report_path / acceptance_criteria / timeout / profile`）**本身没有牙齿**。

现状：`select_permission_option`（`crates/agentcall-daemon/src/acp.rs:271`）对任何 `allow*` 选项**一律自动放行**。所以今天贴个 read-only 模板，agent 真发起 write，daemon 照样点同意——约束停在声明层，没下沉到执行层（和"allowed_paths 只写进 prompt 文本 `routes.rs:558`"同一个病）。

**结论**：模板必须**编译成一套 daemon 强制的能力/权限策略**：

- `read-and-report` → `fs.writeTextFile=false`（`acp.rs:52-54` 已是 false，好起点）+ permission 策略翻转为**遇写类工具一律 deny** + 写预算 0。
- `single-report-update` → 唯一放行写目标 = `report_path`，其余写请求 deny。

**机制**：`session/request_permission` 进来时，daemon 从 `toolCall` 解析目标路径，对当前模板的 `report_path / allowed_paths` 比对，匹配才放行、否则 deny。

**必做改动**：把 `select_permission_option` 从"无脑放行"改成"按当前模板策略放行/拒绝"。没有这层 per-template permission policy，模板就是换了身衣服的 prompt-only 约束。

---

## 4. 模板是四元组，不是输入表

显式定义：

```
template = 输入槽位  +  能力/权限策略(daemon 强制)  +  输出 report schema  +  validator
```

第三项顺手**关掉 v2.2 的一个悬空 ambiguity**："report 合法由谁判、按什么 schema 判" → **按当前模板的 schema 判**。`read-and-report` 的报告 schema ≠ `contract-check` 的。validator 校验输出对不对得上**这个模板**的契约，而非通用的"report 合法"。具体了、可判了。

---

## 5. 两个必须钉死的点（否则漏回老问题）

### 5.1 模板名由 Codex 显式给，route 绝不推断
若让 route 从 objective **推断**该用哪个模板，就把刚删掉的"判断"请了回来。

- `template` 是显式 enum 参数，Codex 自己选；
- route 只**校验槽位**，零推断；
- 给不出模板名 = `needs_contract` / PTY。

route 由此退化为**纯校验器**——它的舒适区。

### 5.2 contract-check 不能只收"是/否"
agent 回 `passed: true` 就完事 = criteria theater，低信任。模板输出 schema 应**强制带证据**（命中的文件/行、跑过的测试输出），让父层可抽查。contract-check 的整个价值就是"可信的 gate"，这条不能省。

---

## 6. route 硬校验规则（default-deny）

| 条件 | 动作 |
|---|---|
| 缺 `report_path` | 拒绝 ACP |
| `allowed_paths` 太宽 | 拒绝 ACP |
| `target_files` 为空且非 discovery 模板 | 拒绝 ACP |
| `max_writes > 1` 且非单文件修复模板 | 拒绝 ACP |
| acceptance criteria 不满足模板要求 | 拒绝 ACP |
| 需要持续讨论 / 计划调整 | 拒绝 ACP，建议 PTY |
| 匹配不到任何模板 | `needs_contract` / PTY |

`runtime=auto` 语义改写：
```
runtime=auto
  -> 能匹配 SOP 模板（且槽位校验通过）：ACP probe
  -> 不能匹配：PTY 或 needs_contract
```

---

## 7. 首批模板：先做 2 个，不做十几个

提案原列 3 个（read-and-report / contract-check / single-report-update）。Review 建议**收到 2 个**——因为 `contract-check` 基本是 `read-and-report` + 对 criteria 给裁决：

| 模板 | 写权限 | 说明 |
|---|---|---|
| **read-and-report** | 0（仅 report_path 例外见下） | 只读指定文件输出报告；`acceptance_criteria` 可选，给了就**必须返回 verdict + evidence**（吃掉 contract-check） |
| **single-report-update** | 仅 `report_path` | 唯一会写的模板；适合阶段验收、同步验证 |

> 注：read-and-report 的"零写"指零写**实现文件**；写 report_path 本身通过"唯一放行写目标 = report_path"的 permission 策略实现。

**single-file-fix 先不进默认 ACP**（提案判断正确）：一旦允许改实现文件，就必须有真正的 `allowed_paths` 强制 + claim + validator，否则误判成本太高。

利好：项目**已有 `file_claims.json` + claim 系统**（`.agentcall/state/file_claims.json`），将来 single-file-fix 落地时，"强制路径 + claim + validator"的底座已存在一半——延后是"等强制层打磨好再接"，不是欠债。

---

## 8. 对 v2.2 的连带后果（瘦两块、换一块实的）

- **删**：`route_decision` 整块打分算术（`routes.rs:319-371`），不是修。auto 退化为模板门。
- **基本免做**：`acp_profile` 系统（上一版最语焉不详的一块）。"减少噪声"被 **per-template 能力策略**吸收——每个模板天然锁死 child 的写权限/工具面。最多保留一个 `bare` 当排障开关。
- **换来一块更实的**：per-template permission 强制（§3 命门）+ per-template 输出 schema/validator（§4）。

净效果：定位清楚 + v2.2 瘦身。

---

## 9. 与 progress-first 的关系

模板门提供**静态闸**（能否表达为已知 SOP），上一轮讨论的运行时机制提供**动态证伪**：

- 模板 ACP 跑超 `max_runtime` / 报告 schema 不合法 / `context_sufficiency=insufficient` → "它其实不该 ACP" 的信号 → 升级 PTY。
- v2.2 的 `checkpoint_due`/progress 不再是"通用后台任务管理器"，而是**模板假设的运行时证伪器**。

两者组合，不冲突。

---

## 10. 待拍板

- [ ] 首批模板定 2 个（read-and-report + single-report-update）还是保留 3 个。
- [ ] permission 策略落点：在 `acp.rs` 的 `handle_agent_request`/`select_permission_option` 内按模板策略 deny，模板策略从 route 传入 `AcpInvocation`。
- [ ] 输出 report schema/validator 放 Rust 还是只读 Python 校验器。
- [ ] toolCall 目标路径解析的可靠性（不同 ACP adapter 的 `request_permission` 结构差异）。
- [ ] `needs_contract` 作为 route 的一等返回状态，board 如何投影。

---

## 11. 唯一的真分歧 / 强调

提案强调"**选模板**"（解决定位/谁来判）；本 Review 强调"**模板要有牙齿**"（daemon 按模板真的 deny 越权写）。

二者缺一不可：选模板若不配 daemon 强制的权限策略，`read-and-report` 的"零写"就只是 prompt 里一句客气话。**§3 是命门，优先级最高。**
