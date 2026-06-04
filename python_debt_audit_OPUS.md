# Python Debt Audit — 确保 Python 只作为胶水(Glue)

Auditor: Opus 4.8 · 日期: 2026-06-03
范围: `crates/agentcall-mcp`、`crates/agentcall-daemon`、`src/agentcall/**`、`scripts/*.py`
方法: 全量扫描 Rust→Python 调用点、Python 模块职责、state 写入者、默认/legacy 路径归属。

---

## 0. "胶水"的判定标准

Python 只有同时满足以下条件才算合格胶水:

1. **无状态**或**只读转换**(读输入 → 返回 JSON,不落 `.agentcall` 写)。
2. **有界**:被 Rust 调用时必须带超时。
3. **一次性**:跑完即退,不持有 live 控制循环。
4. **不是默认控制/写入路线**:绝不写 daemon 拥有的 state,绝不驱动 live 执行。

违反任意一条 = 不是胶水,是债。

---

## 1. 结论速览

| 维度 | 现状 | 判定 |
|---|---|---|
| Python 作为 **state 写入器** | `store.py` 全量写 `events.ndjson`/`state/`/`tasks/`,经多个 MCP 工具触发 | ❌ **双写违规** |
| Python 作为 **live 控制路径** | `delegate`→`workflow_simulate`→ACP `orchestrator/drivers`(阻塞 readline) | ❌ **无界控制路径** |
| Python shell-out **超时** | `run_agentcall_owned` 裸 `.output()`,**无超时**;daemon 客户端有 8s 超时(`main.rs:457-461`) | ❌ **超时不对称** |
| Python **重复 daemon 只读** | `events_tail`/`reports_list`/`transcripts_list`/`workflow_inspect` 读 daemon 已服务的 state | ⚠️ 浪费+无界+可能读到旧视图 |
| Python 合格胶水 | `report_schema`(静态 schema dump)、`models.py`(纯数据类) | ✅ 可留 |

核心判断:**Python 已不是 live writer 的说法不成立**——它通过 `store.py` 仍是多条默认 MCP 控制路径上的真实写入器;同时还是 `delegate` 背后无界的 live 控制路径。两者都拖体验、且违反已定方向。

---

## 2. MCP 工具逐项分类(证据)

分类依据:工具函数体调 `daemon_get/daemon_post`(Rust 路线)还是 `run_agentcall_owned/python_json`(Python 路线)。

### ✅ 已在 daemon(Rust,有界,正确)
`runtime_health` · `project_sessions` · `board` · `file_claims` · `session_summary` · `session` · `session_list` · `session_spawn` · `session_send` · `hook_ingest` · `concurrency_probe` · `codex_preflight`(经 `board()`) · `capabilities`(本地)

### ❌ Python 写入器(双写违规,且无超时)
| 工具 | python 子命令 | 写什么 | daemon 是否有对应 endpoint |
|---|---|---|---|
| `checkpoint_request` / `report` | `checkpoint request` | 标记 session 需 checkpoint → `state/`+`events.ndjson` | **无** |
| `context_packet_create` | `context create` | `write_call_artifacts` 写 `tasks/<id>/calls/...` | **无** |
| `transcript_index` | `transcript index` | 写 transcripts state | **无** |
| `delegate`/`delegate_acp`/`workflow_simulate` | `workflow simulate --driver acp` | orchestrator 经 store 写 tasks/reports/events | **无** |

> 这些 python 进程与 daemon 是两个独立进程,**各自写同一批 `.agentcall` 文件**。`events.ndjson` 的 append、`state/*.json` 的 read-modify-write 在无协调下会交错/损坏。这正是 v0.6.1 "close daemon single writer gap" 想关掉、且非目标明令禁止("不新增 Python/Rust 双写")的东西。

### ❌ Python live 控制路径(无界)
- `delegate` → `workflow_simulate` → `run_agentcall_owned`(`main.rs:570`)→ `python -m agentcall workflow simulate --driver acp` → `src/agentcall/v2/orchestrator.py` → `drivers.py`(ACP client **阻塞 `stdout.readline()`**)。
- MCP 等 Python,Python 等 ACP/Claude,**全链路无超时**。Codex 120s 工具超时返回后,`python.exe`/`agentcall-mcp.exe` 仍挂着,无人 kill。

### ⚠️ Python 重复 daemon 只读(应删或改 daemon_get)
| 工具 | daemon 已服务同一数据 |
|---|---|
| `events_tail` | `/api/board` → `recent_events` |
| `reports_list` | `/api/board` → `reports` |
| `transcripts_list` | `/api/board` → `transcripts` |
| `workflow_inspect` | task/report 可由 daemon `board(task_id)` 服务 |
| `route_task_tool` | 纯建议计算,无 state,但仍走 python+无界 |

### ✅ 合格胶水(可保留)
- `report_schema` → `python_json -c REPORT_SCHEMA_SNIPPET`:无状态、一次性 schema dump。可留(更优是改成 Rust 常量)。

---

## 3. Python 模块职责与归宿

| 模块 | 当前职责 | 归宿 |
|---|---|---|
| `store.py` | **写** `events.ndjson`/`state/`/`tasks/` | ❌ 默认路径上禁止写;改为 daemon 拥有写,store 仅 debug/离线 |
| `v2/orchestrator.py` + `v2/drivers.py` | ACP bounded workflow(阻塞 readline) | ❌ 默认禁用;仅显式 `legacy_delegate_acp` + 强制超时 |
| `sessions.py` / `session_worker.py` / `supervisor.py` / `runtime.py` | legacy Python PTY(v0.7 已降级 `legacy_detached`) | ⚠️ debug/manual,不得默认调起 |
| `models.py` | 纯数据类 | ✅ 胶水(若 schema/转换需要) |
| `cli.py` | 上述全部的入口复用 | 随上面收敛;default 子命令逐步指向 daemon |
| `scripts/agentcall-*-hook.py` | hook 入口(POST daemon) | ✅ 胶水,但需修 UTF-8 入口(见 report_review) |

---

## 4. 整改方案(按优先级)

### P0 — 止血:给 Python shell-out 加 bounded timeout
`run_agentcall_owned` / `run_agentcall` 不能再裸 `.output()`。std 无 timeout → 用 `spawn()` + 计时线程到点 `child.kill()`(或引 `wait-timeout`)。默认上限如 30s,超时 kill 并返回结构化错误。
> 这一步独立、无争议,先做,立刻消除"挂死不退"。

### P1 — 斩断 Python 写路径(关双写)
为以下动作在 daemon 增 POST endpoint,MCP 改 `daemon_post`,删 python 子命令调用:
- `checkpoint_request` → `POST /api/sessions/{name}/checkpoint`
- `context_packet_create` → `POST /api/context` 或并入 session spawn 的 input artifact
- `transcript_index` → `POST /api/transcripts/index`(daemon 读 JSONL→写 state;**Python 至多做只读 JSONL 解析返回 JSON,由 daemon 落盘**)

完成后 `store.py` 在默认路径上**不再被写调用**。

### P2 — 只读去重:Python read 工具改 daemon_get(或删)
`events_tail`/`reports_list`/`transcripts_list`/`workflow_inspect` 改为读 `/api/board`(加 `section`/`task_id` 参数),与 v0.7 "board 投影坍缩" 一致。`route_task_tool` 改为 Rust 本地计算(无状态)。

### P3 — delegate 降级(承接上一轮结论)
- `agentcall_delegate` 默认**不执行**:返回 route/context/建议。
- 真执行显式 `execute:true`+`driver`+`timeout_s`,默认走 **daemon PTY `session_spawn`**。
- ACP Python 路线 → 显式 `agentcall_legacy_delegate_acp`,从默认 `tools/list` 摘掉 + 强制 P0 超时。
- 别让 Python 占着 `delegate` 默认名。

---

## 5. 目标终局(Python = 纯胶水)

整改后 Python 仅剩三类,全部满足 §0:

1. **hook 入口脚本**(`scripts/agentcall-*-hook.py`):一次性,读 stdin→POST daemon,daemon 落盘。(需先修 UTF-8 buffer 读)
2. **只读转换器**:如 transcript JSONL 解析、report schema——读输入返回 JSON,**daemon 负责写**,Rust 调用时带超时。
3. **离线/debug 工具**(`store.py` 手动维护、legacy PTY worker):非默认路径,显式调用。

**不变量**:
- daemon 是 `.agentcall` state 的**唯一写入者**。
- Codex/MCP 默认控制与读取**只经 daemon**。
- 任何 Python 调用**必有超时**。
- Python 路线在 `tools/list` 默认不可见,只在显式 legacy/debug 下出现。

---

## 6. 验证清单

- [ ] 全 MCP 工具调用后,无 `python.exe` 残留超过 timeout 上限。
- [ ] 默认工具路径中 `store.py` 写函数零调用(可加断言/日志)。
- [ ] `events.ndjson`/`state/` 仅 daemon 进程写(按 PID/打开句柄核验)。
- [ ] `delegate` 默认返回建议、不起 python workflow。
- [ ] `cargo test -p agentcall-daemon -p agentcall-mcp` + `python -m pytest -q` 绿。
