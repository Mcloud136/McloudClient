# C1 续作实机基准：多节点测速与负载均衡（2026-11-12）

> **性质**：本机可复现基准，不是公网性能结论。对应 G1 评审 §8.3 第 2 / 4 条
> 「可控 + 可观测」替代策略与「测速 / 负载均衡列为一等公民」约束。
> 全部测试脚本与产物位于仓库外的 `_c0d_test/` 目录，**不入库**；配置中无真实凭据。

---

## 1. 结论

C1 续作（节点可观测 + 多节点测速 + 负载均衡可控）已实现并实机验证：
**32/32（内核 IPC）+ 12/12（GUI 面板）+ 18/18（契约回归）+ 20/20（GUI 回归）**，
质量门禁 fmt / clippy `-D warnings` / test 全绿（Windows 86 tests）。

核心可复现证据：

| 结论 | 实测 |
|---|---|
| 全节点测速确实外连到每个节点 | 3 个 mock 各 +1 accept，RTT 0.279 / 0.469 / 0.797 ms |
| 手动锁定改变真实选路 | 6 次 SOCKS5 请求 → `node-a:0 / node-b:6 / node-c:0` |
| `auto` 恢复自动策略 | 4 次请求 → `4 / 0 / 0`（退化为首个成员） |
| `load-balance` 轮询均匀 | 12 次请求 → `4 / 4 / 4` |
| 配置热重载不丢用户选择 | reload 后仍 `manual → node-c`，实机请求命中 `node-c` |
| 节点地址/UUID 不离开内核 | 20 帧新增方法 IPC 载荷逐帧扫描零命中 |

---

## 2. 被测二进制与门禁

| 二进制 | 大小 | SHA-256 |
|---|---|---|
| `target/release/client-core.exe` | 7,800,320 B | `03cda5ccc0f640af5a35dcd4d8311dabda7e7c426745b304493145bdcd36940b` |
| `target/release/client-gui.exe` | 9,817,088 B | `3a4b6cbfcc7affd8ca04a3585ac4deceec786e6bbbfc05758057c5dfade82346` |
| `target/release/sidecar.exe` | 5,114,880 B | `1519636ea6ea4be35169be1c6a031214f6d62d403b918e0426a1f2175ab3b76a` |

构建：`CARGO_INCREMENTAL=0 cargo build --release --workspace -j 24`（13.40 s）。

门禁（`rust-quality` 推荐顺序）：

| 步骤 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | RC=0 |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | RC=0 |
| 测试 | `cargo test --workspace` | **86 passed**（core 44 / sidecar 29 / gui 13），0 failed |

---

## 3. 基准设置（固定节点 / 固定规则集 / 固定并发）

- **固定节点**：3 个回环 mock（`node-a` 19121 / `node-b` 19122 / `node-c` 19123），
  只做 TCP accept 计数后立刻关闭，使 VLESS/WS 握手快速失败；每个 mock 的
  accept 增量即为「流量真的走到该节点」的独立证据。
- **固定规则集**：`DOMAIN-SUFFIX,lb.test,lb` → `load-balance` 组；
  `MATCH,manual` → `select` 组。两组均含 3 个节点。
- **固定并发**：顺序 N 次 SOCKS5 CONNECT（不做并发，消除调度噪声），
  分别取 N=6 / 4 / 12 / 1。
- **探测触发**：`probe_nodes` 由 IPC 显式调用，或 GUI 的 Ctrl+T；不自动执行。

> 与既有基准的口径一致：所有回环 workload 均需 `--noproxy ""` 才能证明流量经过内核；
> 本基准用 SOCKS5 CONNECT 驱动选路，并以 mock accept 计数作为独立旁证。

---

## 4. 实机结果汇总

| 脚本 | 覆盖 | 结果 |
|---|---|---|
| `s12_nodes_balance_probe.py` | `nodes` / `probe_nodes` / `select_node` 正常与异常路径、选路分布、reload 继承、脱敏、日志、既有方法回归 | **32/32** |
| `s12b_gui_nodes_probe.py` | GUI NODES & BALANCE 区块渲染、Ctrl+T 批量测速、日志导出、mock 命中 | **12/12** |
| `s10_ipc_contract_probe.py` | C1 v1 契约回归（status / metrics / node_probe / reload / 版本协商 / 计数增长） | **18/18** |
| `s11b_gui_probe.py` | C1 首里程碑 GUI 回归（截图 + WinRT OCR + CPU/内存 + 干净退出） | **20/20** |

### 4.1 节点可观测与测速（S12 §2 / §3）

- `nodes` 快照：2 组 / 3 节点，初始均为 `auto`，组按名排序、节点按名排序。
- `probe_nodes`（`timeout_ms=1000`）：3 项全部 `reachable=true`，
  RTT 分别 `node-a 0.797 ms` / `node-b 0.469 ms` / `node-c 0.279 ms`；
  3 个 mock 各收到 1 次连接。
- 快照随后反映最近一次 TCP 探测（`tcp_ok=true` + `tcp_rtt_ms`）。
- 参数边界：`timeout_ms=0` 与 `999999` 均返回 `invalid_params`；
  省略 `params`（`null`）时取默认超时并通过。
- 单节点不可达：停止该 mock 监听后，`node-c` 返回
  `{"reachable": false, "error_code": "timeout"}`，**不作为 IPC 错误**。

### 4.2 可控与负载均衡（S12 §4 / §5）

| 场景 | 请求数 | `node-a` | `node-b` | `node-c` | 判定 |
|---|---:|---:|---:|---:|---|
| 锁定 `manual → node-b` | 6 | 0 | 6 | 0 | 锁定生效 |
| 恢复 `auto` | 4 | 4 | 0 | 0 | 退化首个成员 |
| `load-balance` 轮询 | 12 | 4 | 4 | 4 | 均匀分布 |
| reload 后仍锁定 `node-c` | 1 | 0 | 0 | 1 | 选择继承生效 |

- 异常路径：`unknown_group` / `unknown_node` 错误码正确。
- 热重载：profile 增加一条规则（rules 2 → 3），收到 `applied` 事件；
  手动锁定 `node-c` 在新路由器上保留，选路仍命中 `node-c`。
- 回归：`metrics.connections_total = 24`、`status`、`node_probe` 回环白名单均正常。

### 4.3 脱敏与可导出日志（S12 §6）

- `nodes` / `probe_nodes` / `select_node` 的 20 帧 IPC 载荷逐帧扫描：
  `127.0.0.1:1912x` 与 dummy UUID **零命中**。
- 内核日志 9 行 `node tcp probe` + `node selection changed`，测速日志行不含地址与 UUID。
- GUI 日志：`node probe batch completed nodes=3 reachable=3 summary=3/3 reachable`。

### 4.4 GUI 面板（S12b，PrintWindow + WinRT OCR）

截图视觉核验（`s12_gui\nodes_shot2.png`）确认 `NODES & BALANCE` 区块渲染：

```
NODES & BALANCE
  [Probe all (Ctrl+T)]   3/3 reachable
  node-a   health -   tcp 1.30 ms
  node-b   health -   tcp 0.76 ms
  node-c   health -   tcp 0.44 ms
  lb [load-balance]   auto -> node-a
```

- Ctrl+T 命令链路：`gui.log` 记录 `node probe batch completed`，3 个 mock 各 +1。
- 资源（S11b 回归）：GUI RSS 76.6 MiB / private 71.5–72.3 MiB；
  可见空闲 CPU 0.000–0.104%，最小化 0.000%；内核 16.5 MiB / 8.4 MiB。
  **D1 内存地板结论不变**（~124 MiB，来自 GL/驱动），等待用户裁决 wgpu / Tauri 对比。

---

## 5. 测量缺陷与修正（重要）

**S11b 曾连续两次 19/20，判定为稳定回归，实为测试输入注入失败。**

- 现象：`panel 3 shows probe result` 缺 `ms`；截图 NODE PROBE 区块无结果行。
- 定位：在 GUI 加 `gui probe command issued` / `ipc probe command received` /
  `node probe completed` 三级日志后，发现**按键根本未送达窗口**。
- 根因：Windows 前台锁定使 `SetForegroundWindow` 静默失败，
  `GetForegroundWindow` 返回 `201310` 而目标为 `1380044`；`keybd_event` 打进了别的窗口。
- 修正：探针改用 `AttachThreadInput` + 重试 + `GetForegroundWindow` 核对；
  修后 `focused=True`，日志三段链条完整，回归恢复 **20/20**。
- 教训：**GUI 自动化断言必须先验证输入注入本身成功**，否则会把环境失败误判为代码回归。
  该修正只改仓库外的测试脚本，未改动产品代码；产品侧新增的三级日志保留作为可观测能力。

**OCR 分词边界**：WinRT OCR 对小字号 `3/3 reachable` 会整体漏读（单独调用甚至返回空串）。
S12b 因此把面板断言放宽为结构 token，并把结果值改为
「可导出日志 + mock 命中数」的机器可验证证据，截图仅作视觉核验。

---

## 6. 边界与未覆盖

1. 基准只测 **TCP 连接探测**，不含经代理的 VLESS/WS/TLS/mlkem 端到端测速；
   `health.rs` 的经代理 HTTP 健康探测是另一套语义，本轮未改动、也未在真实节点上复测。
2. mock 节点会立即关闭连接，因此不产生可用流量；选路结论以 accept 计数为准，
   不是转发性能结论。
3. `url-test` / `fallback` 的健康表驱动选路已有单测覆盖，但未在本次实机基准中
   用真实健康数据验证（需可用 VLESS 节点或桩服务器）。
4. 手动锁定目前作用于所有组类型；嵌套策略组的 `members` 只展示一级成员名。
5. `nodes` 的 `load-balance` 选中值是「下一次」预览，不是上一次实际选中；
   这是避免查询消耗轮询计数器的刻意设计。
6. 12 h 长稳仍属 FA，二进制冻结前不执行。

---

## 7. 变更记录

| 日期 | 版本 | 内容 |
|---|---|---|
| 2026-11-12 | v1 | C1 续作首个基准，32/32 + 12/12 + 18/18 + 20/20，门禁 86 tests 全绿 |
