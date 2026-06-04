# ACP 实现对比报告 — Python vs Rust Native

Reviewer: Opus 4.8 · 日期: 2026-06-04
范围: `src/agentcall/v2/acp.py`、`src/agentcall/v2/drivers.py` ↔ `crates/agentcall-daemon/src/acp.rs`、`crates/agentcall-daemon/src/routes.rs`
关联: `plan_review_v0.8_OPUS.md`、`python_debt_audit_OPUS.md`、CHANGELOG `v0.8b`

---

## 0. TL;DR

ACP 在本项目里是**两层**：传输层（stdio JSON-RPC 收发）+ 驱动/报告契约层（prompt 拼装、报告解析、schema 校验、类型化报告、失败兜底）。

| 层 | Python | Rust | 差距 |
|---|---|---|---|
| 传输层 | `AcpStdioClient`（`acp.py`，172 行） | `acp.rs`（357 行） | **≈0，且 Rust 更稳** |
| 驱动/报告契约层 | `AcpClaudeDriver` + `HeadlessJsonClaudeDriver`（`drivers.py`，227 行） | **无** | **整层缺失** |

一句话：**传输层已对齐甚至反超；"ACP 驱动"的灵魂（报告解析 + schema 校验）整层仍留在 Python，daemon ACP route 目前只回传原始文本。另有一个 Windows `npx` 命令解析的现实缺口。**

---

## 1. 传输层对比：几乎等价，Rust 反超

`AcpStdioClient` ↔ `acp.rs::run_acp_invocation` + `JsonRpcClient`

| 能力 | Python | Rust | 判定 |
|---|---|---|---|
| initialize / session/new / set_mode / prompt | ✅ | ✅ | 一致 |
| request id 从 0 起 | ✅ `acp.py:40` | ✅ `acp.rs:126` | 一致（parity 前提） |
| permission 自动放行（allow* 优先，否则首项） | ✅ `acp.py:129-138` | ✅ `acp.rs:263-286` | 逐字一致 |
| 未知 client 方法 → `-32601` | ✅ | ✅ | 一致 |
| session/update 累积 + agent_message_chunk 取文本 | ✅ `acp.py:20-29` | ✅ `acp.rs:288`（兼容扁平格式，更防御） | Rust 略宽容 |
| **超时真正生效** | ❌ 形同虚设 | ✅ 真生效 | **关键差异** |
| stderr 读取 | ❌ 仅 stdout 关闭后才读 `acp.py:162` | ✅ 独立线程全程并发读 `acp.rs:43` | **Rust 更安全** |
| 进程中途死亡检测 | ❌ 靠 stdout EOF | ✅ 每轮 `child.try_wait()` `acp.rs:151` | Rust 更主动 |
| clientInfo.version | `"2.0.0"` | `"0.8.1"` | 都硬编码，测试归一化掩盖 |
| session/new 的 cwd | `cwd.resolve()` 绝对化 `acp.py:81` | `to_string_lossy()` 原样 `acp.rs:64` | 微差，测试归一化掩盖 |

### 两个关键点

1. **Python 的 `timeout_seconds` 是装饰品**。`call()` 里 `_read()` 是裸 `readline()`（`acp.py:159`），无任何超时；构造函数收的 900s 只在 `__exit__` 终止进程时间接起作用。ACP server 一旦挂起不回，Python 客户端**无限阻塞**，要靠外层 caller kill——正是 `python_debt_audit_OPUS.md` P0 点名的"裸 `.output()` 挂死"。Rust 在 `call()` 每轮检查 deadline、`recv_timeout(50ms)`、到点 `kill_child`（`acp.rs:147-149`），**真正把超时握在自己手里**。这是本次移植最实质的进步。

2. **stderr 并发读**。Python 仅在 stdout 关闭后才读 stderr；若 server 在 stdout 阻塞期间往 stderr 狂写，stderr 管道缓冲填满会反卡死子进程。Rust 起独立线程全程 drain，规避此经典死锁。CHANGELOG "stdout 和 stderr 并发读取" 已兑现。

**传输层差距 ≈ 0，且 Rust 在健壮性上反超。** parity 测试（`tests/test_v08b_acp_parity.py`）比对的正是这一层。

---

## 2. 驱动/报告契约层对比：Rust 完全没有

Python `AcpClaudeDriver.invoke()`（`drivers.py:52-109`）在传输之上的一整套，Rust `start_acp_route`（`routes.rs:416`）一项都没有：

| 驱动层能力 | Python | Rust route | 影响 |
|---|---|---|---|
| prompt 嵌入 `REPORT_JSON_SCHEMA` + schema 文本 | ✅ `drivers.py:53-58` | ❌ 仅 markdown 文字要求（`routes.rs:570`） | agent 拿不到机器可读契约 |
| 从 agent 输出**抽取 JSON 报告** | ✅ `extract_json_object`（去 fence + 正则兜底） | ❌ 只回传 raw `text`/`updates` | **报告无人解析** |
| **报告 schema 校验** | ✅ `validate_report_dict` | ❌ | 无效报告拦不住 |
| 注入 task_id/call_id/agent、补 `context_sufficiency` | ✅ | ❌ | |
| 失败路径返回**结构化 ChildReport**（传输失败/无 JSON/校验失败各一条） | ✅ 三分支 | ❌ 只塞 `error` 字符串进 `result` | 失败语义降级 |
| 类型化 `ChildReport` 产物 | ✅ | ❌ 返回 `json!({...})` | |
| **命令解析 `resolve_command`**（Windows 补 `.cmd`/`.exe`、`shutil.which`） | ✅ `drivers.py:213-226` | ❌ 直接 `Command::new` `acp.rs:24` | **真 bug，见 §3** |
| `HeadlessJsonClaudeDriver` 兜底（`claude -p --output-format json`） | ✅ `drivers.py:112` | ❌ | 无降级路线 |

**daemon ACP route 现状**：产出的是"原始文本 + updates + stop_reason"（`routes.rs:441-449`），**不是"已校验报告"**。`test_v2_parent_rejects_*` 那批验收逻辑（拒绝超生命周期、拒绝不存在的文件、拒绝上下文不足）只在 Python orchestrator 里跑，**daemon 路线上这一环是断的**。

---

## 3. 会立刻踩到的 Windows Bug：`npx` 命令解析

Python `resolve_command`（`drivers.py:213-226`）专门处理"`npx` 在 Windows 上是 `npx.cmd`"——依次试 `npx` / `npx.cmd` / `npx.exe`。

Rust 是 `Command::new(&invocation.command[0])`（`acp.rs:24`），**不做扩展名解析**。Rust 标准库 `Command` 在 Windows 上只补 `.exe`、**不试 `.cmd`/`.bat`**。结果：README 官方示例

```json
"adapter_command": ["npx", "-y", "@agentclientprotocol/claude-agent-acp"]
```

**在 Windows 上大概率 spawn 失败**（找不到 `npx`）。Python 路线能跑、Rust 路线跑不起来。本项目是 Windows 11 主场、README 自己用的就是 `npx`，优先级不低。临时绕法：显式传 `npx.cmd` 或绝对路径——但等于把 Python 已解决的问题甩回调用方。

---

## 4. 测试与验证状态

- `cargo test --workspace`：22 passed（含 `acp::tests` 2 条、`routes::tests` 4 条）。
- `tests/test_v08b_acp_parity.py`：**本地未真正执行**——`.agentcall_pytest` basetemp 报 `WinError 5 拒绝访问`，16 条 pytest 全 ERROR（含 parity）。即 parity 这次是"绿在设想里"，本地未验证。建议把 `[tool.pytest.ini_options] addopts` 的 `--basetemp` 移出 repo（如 `%TEMP%`）。

---

## 5. 差距评估与建议

| 层 | 差距 | 性质 |
|---|---|---|
| 传输层 | ≈0，Rust 更稳 | 移植完成且更优 |
| 驱动/报告契约层 | 整层缺失 | 取决于设计意图（见下） |

- 若按 `plan_review_v0.8_OPUS.md` 切分，**v0.8b 范围本就只是传输层**，报告校验属 orchestrator、留 Python 或后续——则 Rust 做的范围正确，**但需明确：daemon ACP route 现在产出"原始文本"而非"已校验报告"，报告契约在 daemon 路线上暂时断开**。
- 若预期"Rust ACP = 完整替代 Python ACP 驱动"，则差距很大，**还差一整层**。

### 建议（按优先级）

1. **[P0] 修 Windows `npx`**：在 `acp.rs` 加 `resolve_command` 等价物（试 `.cmd`/`.exe`，或文档强制要求 `adapter_command` 写全名/绝对路径）。否则 README 示例在主场跑不动。
2. **[P1] 明确报告契约归属**：要么把"抽 JSON + schema 校验"挪进 Rust（daemon route 之后挂一道校验），要么在文档/CHANGELOG 写明"daemon ACP route 只回传原始文本，报告校验仍由 Python orchestrator 承担"，避免误以为已对齐。
3. **[P1] 让 parity 真能跑**：移 basetemp 出 repo，闭合 v0.8b 验证环。
4. **[P2] 清理小项**：`acp.rs` clientInfo.version 改 `env!("CARGO_PKG_VERSION")`；timeout 双重 clamp（`acp.rs:22` 的 900 vs `routes.rs:424` 的 300）统一；`spawn_stdout_reader`（`acp.rs:229-230`）去掉双重解析。
