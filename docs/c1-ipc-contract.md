# C1 IPC 契约（v1.3，2026-11-13）

- **状态**：已实现并实机验证（`status` / `reload` / `metrics` / `node_probe` /
  `nodes` / `probe_nodes` / `select_node` + **v1.2 新增 `log_tail` / `diagnostics`**
  + **v1.3 新增 `get_rules`**）；
  **消费端已落地**：`crates/client-gui`（原生 WinUI 登录 + 四区 + NODES & BALANCE +
  日志导出；egui 面板与旧 s11b/s12b 探针已随 ADR 0005/0006 作废）
- **代码单一事实源**：`crates/client-sidecar/src/contract.rs`（方法名、载荷、错误码）
- **相关**：ADR 0002（IPC / 配置 / 日志归属）、ADR 0003（维持自研 R2）、`docs/c1-gui-tun-prestudy.md` §5、`docs/client-design-plan.md` §9

## 1. 进程模型（架构锁点）

**GUI 与 `client-sidecar` 合并为同一进程**，内核仍是独立服务进程。理由：

- 当前命名管道是**单连接模型**（一个服务端连接），GUI 若独立进程会与 sidecar 争抢连接；
- 合并后 GUI 直接以函数调用 / `tokio::mpsc` 读取控制面状态，无需把单连接改成广播多连接；
- s5–s9 的全部回归用例可原样复用，IPC 协议不变。

若未来改为独立 GUI 进程，必须先引入多连接或事件广播，并重新跑 s9 回归 —— 那是架构级变更，需单独评审。

## 2. 传输层（沿用 ADR 0002）

| 项 | 值 |
|---|---|
| 帧 | `u32` 大端长度前缀 + UTF-8 JSON |
| 协议版本 | `v = 1`（`protocol::VERSION`），不兼容变更时递增 |
| 单帧上限 | 1 MiB（`MAX_FRAME`） |
| 通道 | Windows 命名管道，SDDL 限 `SY` + `IU` + 当前用户 |
| 消息种类 | `request` / `response` / `event` |
| 事件 | `reload` 的 `applied` / `rejected`，事件先于响应入队 |

进程 RSS / CPU 由控制面**本地采样**，不经 IPC；内核只上报自己的内部状态。

## 3. 方法清单

| 方法 | 状态 | 用途 |
|---|---|---|
| `status` | ✅ 既有 | 规则数、配置版本、uptime，`detail=true` 时附 fake-ip 条目与脱敏 source |
| `reload` | ✅ 既有 | 重新拉取并原子替换路由器，附事件推送 |
| `metrics` | ✅ 已实现 | 连接与字节计数、规则/版本/uptime |
| `node_probe` | ✅ 已实现 | 单个回环目标的 TCP 连接延迟探测 |
| `nodes` | ✅ v1.1 新增 | 策略组与节点可观测快照（组/模式/选中/健康/TCP RTT） |
| `probe_nodes` | ✅ v1.1 新增 | 对所有已登记节点并发 TCP 测速（多节点网络测试） |
| `select_node` | ✅ v1.1 新增 | 手动锁定策略组成员 / 恢复自动（负载均衡可控） |
| `log_tail` | ✅ v1.2 新增 | 内核环形日志尾部读取，**逐行脱敏后**下发（§4.6） |
| `diagnostics` | ✅ v1.2 新增 | 连接统计 + DNS 路由自检 + 崩溃取证字段（§4.7） |
| `get_rules` | ✅ v1.3 新增 | 当前生效的原始规则行（按匹配顺序）+ 配置版本（§4.8） |

`IMPLEMENTED_METHODS` / `PLANNED_METHODS` 定义在 `contract.rs`，二者不允许重叠（有单测守护）。
预研 §5 提到的 `balance_policy` 不由独立方法承担：`select_node` 负责锁定/解锁，
`nodes` 负责暴露当前策略与选中结果，避免「读」「写」两套语义分裂。

## 4. 载荷定义

### `metrics`（请求参数为空）

| 字段 | 类型 | 说明 |
|---|---|---|
| `uptime_s` | u64 | 内核启动至今秒数 |
| `rules` | usize | 当前路由器规则数 |
| `version` | u64 | 配置版本（`reload` 成功时递增） |
| `fakeip_entries` | usize | fake-ip 池条目数 |
| `connections_total` | u64 | 累计接受的 SOCKS5 连接数 |
| `connections_active` | u64 | 当前活跃连接数（RAII guard 保证提前返回也正确） |
| `bytes_up` / `bytes_down` | u64 | **观测值**：`tokio::io::copy` 完成时累加；`select!` 收尾会取消另一方向，尾部字节可能少计，**不用于流量计费** |
| `source_kind` | string | 仅 `file` / `url` 类别，**绝不回显订阅地址或 token** |

### `node_probe`

请求：

| 字段 | 类型 | 约束 |
|---|---|---|
| `address` | string | 严格 `host:port`；当前**仅允许回环目标** |
| `timeout_ms` | u64 | 默认 2000，范围 `1..=10000` |

响应：

| 字段 | 类型 | 说明 |
|---|---|---|
| `address` | string | 原样回显目标 |
| `reachable` | bool | TCP 连接是否成功 |
| `rtt_ms` | f64? | 成功时的连接耗时（毫秒） |
| `error_code` | string? | `connect_failed` / `timeout`，失败时填写 |

**探测失败是正常结果，不作为 IPC 错误返回**。参数非法或目标越界才返回错误码。

`node_probe` 的回环白名单用 `IpAddr::is_loopback` 判定 IP 字面量，另允许 `localhost`；因此 `127.0.0.1.evil.example` 这类形近域名会被拒绝。该边界**保持不变**；多节点测速由独立的 `probe_nodes` 承担，见 §4.3。

### 4.3 `nodes`（请求参数可省略）

| 字段 | 类型 | 说明 |
|---|---|---|
| `groups[].name` / `kind` | string | 组名与类型（`select` / `url-test` / `fallback` / `load-balance`） |
| `groups[].mode` | enum | `auto` / `manual`；`manual` 表示被 `select_node` 锁定 |
| `groups[].selected` | string | 当前生效成员；`load-balance` 的 `auto` 模式是「下一次」预览值 |
| `groups[].members` | string[] | 组成员名，保持 profile 顺序 |
| `nodes[].name` | string | **仅节点名**，无地址/UUID/凭据 |
| `nodes[].health_ms` | u32? | 经代理的 HTTP 健康探测耗时（未知/失败为 `null`） |
| `nodes[].tcp_rtt_ms` / `tcp_ok` / `tcp_error` | 观测值 | 最近一次 `probe_nodes` 的 TCP 连接结果 |

`load-balance` 的预览值用 `AtomicUsize::load` 读取，**不消耗**轮询计数器，因此查询快照不会干扰选路。

### 4.4 `probe_nodes`

请求：`{ "timeout_ms": 1..=10000 }`，省略 `params` 或传 `null` 时取默认 2000。

响应：`results[]`，每项含 `name` / `reachable` / `rtt_ms` / `error_code`（`connect_failed` / `timeout`）。

- 目标来自 profile 中**已登记节点**，并发发起 TCP 连接探测；失败是正常结果，不是 IPC 错误。
- 结果缓存在内核供 `nodes` 查询，**不写入健康表**：`url-test` 仍由 `health.rs` 的经代理 HTTP 探测驱动，两者语义不同，不得混用。
- 每次探测写一条结构化日志 `node tcp probe node=<名称> reachable=… rtt_ms=…`，可导出且不含地址。

### 4.5 `select_node`

请求：`{ "group": "...", "node": "..." | "auto" | null }`。

| 规则 | 说明 |
|---|---|
| 锁定 | 手动选择优先于 `url-test` / `fallback` / `load-balance` 等任何自动策略 |
| 解锁 | `node = "auto"` / `null` 恢复自动 |
| 热重载 | 锁定迁移到新路由器；新配置中已不存在的成员自动丢弃 |
| 校验 | 组不存在 → `unknown_group`；成员不在组内 → `unknown_node` |

响应：`{ "group", "mode", "selected" }`，并写一条 `node selection changed` 日志。

### 4.6 `log_tail`

请求：`{ "lines"?: usize }`。省略或 `null` → 默认 **200**；合法域 `1..=1000`；
越界 → `invalid_params`（文案 `lines must be 1..=1000`）；非数字 → `invalid_params`（serde 文案）。

响应：

| 字段 | 类型 | 说明 |
|---|---|---|
| `requested` | usize | 实际生效的请求行数（校验后值） |
| `returned` | usize | 实读行数（≤ `requested`；日志不足或有历史轮转文件时为小值） |
| `lines` | string[] | **逐行脱敏**后的日志文本；脱敏规则见下表 |

脱敏规则（实现：`crates/client-core/src/redact.rs`，手写字节扫描、不引 regex 依赖；
按特异性从高到低**顺序**应用）：

| 序 | 模式 | 占位符 |
|---|---|---|
| 1 | 节点地址字面量（完整 `host:port` 与去括号裸 host 两形态；长前缀优先，防残尾） | `[REDACTED]` |
| 2 | `http(s)://` 起至空白/引号止整段（订阅链接含 token） | `[REDACTED-URL]` |
| 3 | 8-4-4-4-12 UUID（前后十六进制边界判定） | `[REDACTED-UUID]` |
| 4 | ≥32 位 base64/hex 连续串（密钥、加密串、哈希） | `[REDACTED-KEY]` |

- **顺序理由**：URL 先整段吃掉，避免订阅链接内部的 UUID / token 碎片在后续模式里漏网；
  UUID 结构化次之；长 token 兜底。
- 口径：**宁可过度脱敏**——诊断可读性让位于凭据边界（§6 第 3 条）。
- 边界：脱敏只替换**值**，不保证清除字段标签（`uuid=` 之类残留合法）；
  `redact()` 在 UTF-8 解码失败时返回原行，故此类行**不得**下发（实现须按丢弃处理）。
- 字面量来源：`redact::from_router` 取 `Router::probe_targets()`（节点名 + 地址，不含凭据字段）。
- 读取实现：从文件尾最多回看 256 KiB，**不整文件加载**；窗口首行可能被截断则丢弃。
  当前文件与历史 `core.N.log` 的跨轮转边界、以及内核持有写句柄时的并发追加（Windows
  共享写模式，std `append` 默认共享），实机已验。
- **已知缺口**（登记，非缺陷裁决）：无 scheme 裸域名、百分号编码片段、通用 IPv6 模式
  不做全局匹配；已配置节点的 IPv6 由规则 1 字面量覆盖。补齐前，诊断文本不得当作
  「绝对无残留」的证据对外分发。

### 4.7 `diagnostics`

请求：无参数（`null` 可）。

响应 = **`metrics` 全部字段**（`uptime_s` / `rules` / `version` / `fakeip_entries` /
`connections_total` / `connections_active` / `bytes_up` / `bytes_down` / `source_kind`）+ 三个对象：

| 字段 | 类型 | 说明 |
|---|---|---|
| `dns.server_enabled` | bool | 内核 DNS 服务是否在跑（`--dns-listen`） |
| `dns.resolver_route` | enum | `proxy` / `direct` / `reject` / `unknown` |
| `dns.fakeip_entries` | usize | 同 `metrics.fakeip_entries`，就近可读 |
| `crash.panic_count` | usize | 日志尾部窗口内 `thread panicked` 行数 |
| `crash.last_panic` | string? | 最后一条 panic（**脱敏后**）；无则 `null` |
| `log.file` | string? | 当前日志文件路径；未启用日志为 `null` |
| `log.size_bytes` | u64? | 当前日志文件大小；不可读为 `null` |

- `resolver_route` = `router.route(Target::Ip(8.8.8.8))` 的**结构性自检**：只判定「知名解析器
  IP 会走哪条规则」，**不发起任何外联、不是活体泄漏测试**。8.8.8.8 是公开知名地址，不属秘密。
- `dns=direct` 提示 DNS 流量可能绕过代理，属**需要人工研判的线索**，不构成泄漏结论。
- 「崩溃取证导出」由 `log_tail` 的全量脱敏导出承担，不另做独立 dump（C2 MVP 定案）。

### 4.8 `get_rules`（v1.3）

请求：无参数（`null` 可）。

| 字段 | 类型 | 说明 |
|---|---|---|
| `rules` | string[] | 原始规则行（配置顺序，**未展开 RULE-SET**），供 GUI 规则编辑回显 |
| `version` | u64 | 配置版本（与 `status`/`metrics` 同口径，reload 成功递增） |

规则行只含域名/网段/分类与策略名，不含节点地址与凭据（§6 脱敏边界）。

**规则编辑架构（C2，GUI 侧）**：

- 三类文件分离（守 `client-design-plan.md` §FA 前置规格二「UI 偏好独立文件」）：
  - `subscription-base.json`：**订阅原始 profile**（同步拉取的未合并字节），是合并的唯一订阅基底；
  - `user-rules.json`：用户自定义规则（`{"rules":[...]}`），UI 偏好；
  - `live.json`/编号快照：合并产物（用户规则前置 + 订阅规则），内核读取。
- 应用流程：GUI 取**纯订阅基底**（`subscription-base.json`；旧安装无此文件时从当前快照
  确定性剥离已保存用户规则前缀回退）→ 用户规则**前置**合并 → 用内核同一套
  `Router::from_profile` 校验 → 通过才写 `user-rules.json` + 合并快照 → IPC `reload` 生效；
  校验失败不写任何文件。**严禁以已合并快照为基底**，否则二次编辑会累积旧用户规则（已修复并有回归测试）。
- 编辑语义：文本框即用户规则全集——二次编辑**替换**旧用户规则，清空文本**清除**全部用户规则。
- 订阅同步（`sync_once`）持久化订阅基底并自动重并入用户规则，订阅更新不丢编辑。
- GUI 启动回显已保存用户规则（仅文本框为空时预填）；应用后状态经 reload applied 事件
  从「已应用（等待生效）」收敛到「已生效（内核已重载）」终态。
- **已知校验限制**（与服务形态对齐的有意取舍）：校验用空 geo 数据，故 `GEOIP/GEOSITE` 规则
  会被拒绝（服务 binPath 不带 `--geoip/--geosite`）；若内核以 CLI 带 geo 数据运行，GUI 校验
  口径会偏严。`RULE-SET` 相对路径在 GUI 进程 cwd 下展开，可能与内核 cwd 不一致——首版仅支持
  绝对路径 rule-provider。两者列为后续增强，不影响 DOMAIN/IP-CIDR/MATCH 等常用规则。
- **快照兼容矩阵**（FA 需实机复核，单测已覆盖）：
  - 旧内核读新快照：合并产物本身是合法 profile，正常构建；`user-rules.json`/`subscription-base.json` 被忽略；
  - 新 GUI 读旧快照：无 `user-rules.json` 回退纯订阅规则；无 `subscription-base.json` 从快照剥离用户规则前缀回退；
  - 两文件含未知字段：serde 忽略（新增字段可选/有默认值）。
- 实机证据：`_c0d_test/s14_rules_probe.py` 9/9（get_rules/reload 继承/用户规则前置/脱敏）；
  `_gui_spike/run_rules.sh` + `verify_rules.ps1` **9 步 PASS**（登录→同步→非法拒绝无副作用→
  合法合并 6 规则→内核 reload 硬断言→**二次编辑替换不累积→重启回显→清空清除内核回 5**）。

## 5. 错误码

| 码 | 触发 |
|---|---|
| `unknown_method` | 方法不在已实现集合 |
| `invalid_params` | 参数反序列化失败、`timeout_ms` 越界、`log_tail` 的 `lines` 越界（`1..=1000`）或非数字 |
| `forbidden_target` | `node_probe` 目标不是回环地址 |
| `fetch_failed` / `validate_failed` | `reload` 拉取 / 构建失败（既有） |
| `handler_error` | 处理器内部错误 |
| `serialize_failed` | 响应序列化异常 |
| `unknown_group` | `select_node` 的策略组不存在 |
| `unknown_node` | `select_node` 的节点不在该组成员列表中 |

## 6. 安全边界

1. 订阅 URL 含 token，**任何 IPC 响应都不得回显**；`source_kind` 只给类别，日志侧也不写 URL。
2. `node_probe` 当前只放行回环，避免未授权的内网扫描；扩展到真实节点前需先过威胁评审。
3. `log_tail` 与 `diagnostics` 的全部文本输出必须先脱敏（节点地址、UUID、加密串、订阅链接）
   再下发给 GUI；实现在 `crates/client-core/src/redact.rs`，规则表与已知缺口见 §4.6。
4. GUI 若使用 WebView，`token` 不得离开 Rust 层；本契约的响应字段设计已按此约束裁剪。
5. `probe_nodes` 是**显式调用触发的主动外连**，目标限定为配置中已登记的节点，不自动执行、不接受任意地址；响应与日志均只出现节点名。`node_probe` 的回环白名单不受影响。
6. `nodes` / `probe_nodes` / `select_node` 的全部响应字段经单测与实机逐帧扫描守护：节点地址、端口、UUID、加密串、订阅链接一律不回显。

## 7. 版本与兼容

- 帧 `v` 与 `Metrics` / `NodeProbeResult` 字段新增：向后兼容（新增字段），`v` 不变。
- 字段删除 / 改名 / 语义变化：`v` 递增，且按 FA 升级回滚矩阵要求保留一个版本的双读能力。
- 快照格式与 profile schema 的兼容规则见 `docs/client-design-plan.md` §5「FA 最终验收规格」。
- **v1.2 变更类型判定**：`log_tail` / `diagnostics` 属**纯新增方法与新增字段**，帧 `v` 保持 1
  （§7 第 1 条）。旧消费端遇到未知方法返回 `unknown_method` 的行为不变，不需要双读窗口。

## 8. 验收方式

- 单元测试：`contract.rs`（载荷往返、回环白名单含绕过用例）、`metrics.rs`（RAII 计数）、`relay.rs`（字节计数）。
- 实机回归：`D:\wxmuma\_c0d_test\s10_ipc_contract_probe.py`（未入库），覆盖正常路径、未知方法、参数校验、越权目标、连接/字节计数增长与响应脱敏。
- 既有回归：s5–s9 不受影响（帧协议与 `status` / `reload` 语义未变）；它们用外部域名与 `--socks5-hostname`，不受本机 `NO_PROXY` 影响。
- v1.2 实机回归（2026-11-13，脚本在仓库外 `_c0d_test/` 与 `_gui_spike/`）：
  `s13_log_diag_probe.py` **16/16**（默认/限行/三类参数校验、四类占位符齐全、追加秘密行后
  下发无原值、`diagnostics` 字段完备、DNS 自检 proxy/direct 双向、`unknown_method` 守护、
  全帧秘密扫描）；`run_export.sh` + `verify_export.ps1` GUI 实机 **PASS**
  （登录 mock → 代理区 `内核已连接` → 点「导出日志」→ `已导出 N 行（已脱敏）` →
  文件落 `%USERPROFILE%\Documents\McloudClient-logs-*.txt` → 面板秘密零命中）。
  回归时 **必须先 `cargo build --release`**：探针启动 `target/release/client-core.exe`，
  `cargo test` 不更新 plain bin（同 `run_*.sh` 旧坑）。
- v1.1 实机回归（2026-11-12，脚本在仓库外 `_c0d_test/`）：`s12_nodes_balance_probe.py`
  **32/32**，`s12b_gui_nodes_probe.py` **12/12**；既有 `s10_ipc_contract_probe.py`
  **18/18**、`s11b_gui_probe.py` **20/20**。全量数据与边界见
  `docs/benchmark-results/2026-11-12-c1-nodes-balance.md`。
- GUI 回归（2026-11-12）：`D:\wxmuma\_c0d_test\s11b_gui_probe.py` **20/20**（PrintWindow 截图 + WinRT OCR 断言面板内容、计数增长、探测结果、断连状态、干净退出）；被测 `client-gui.exe`（egui 时代二进制，现已被 ADR 0005/0006 取代，仅作历史记录）。
- v1.2 被测二进制（2026-11-13 08:17 Release，工作区含未提交的 #14 改动）：
  `client-core.exe` 7,988,224 B，SHA-256 `ad42a1610a897bf0…`；
  `client-gui.exe` 9,617,408 B，SHA-256 `b33a9e4ce9088758…`。
- **测量前提**：本机 `NO_PROXY=127.0.0.1,localhost,::1`，任何回环 workload 的 curl 都必须带 `--noproxy ""`，否则会绕过 SOCKS5 直连目标、计数不增长、数据失效。本轮 18/18 回归正是靠 `connections_total` / `bytes_down` 增长独立证明了流量确实经过内核。
