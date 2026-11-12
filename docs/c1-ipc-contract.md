# C1 IPC 契约（v1，2026-11-12）

- **状态**：已实现并实机验证（`status` / `reload` / `metrics` / `node_probe`）；
  **消费端已落地**：`crates/client-gui`（egui 只读面板，2026-11-12，实机 20/20，
  见 `docs/c1-gui-tun-prestudy.md` §9）
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
| `metrics` | ✅ 本轮新增 | 连接与字节计数、规则/版本/uptime |
| `node_probe` | ✅ 本轮新增 | TCP 连接延迟探测（网络测试的最小可测形态） |
| `log_tail` | 规划 | 环形日志尾部读取，供 GUI 诊断页 |
| `diagnostics` | 规划 | 连接统计、DNS 泄漏自检、崩溃取证导出 |

`IMPLEMENTED_METHODS` / `PLANNED_METHODS` 定义在 `contract.rs`，二者不允许重叠（有单测守护）。

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

回环白名单用 `IpAddr::is_loopback` 判定 IP 字面量，另允许 `localhost`；因此 `127.0.0.1.evil.example` 这类形近域名会被拒绝。这是防止 IPC 被当作任意内网探测入口（SSRF）的临时边界；C1 后续里程碑将扩展为「配置中已登记节点」白名单，并加上多节点测速与负载均衡。

## 5. 错误码

| 码 | 触发 |
|---|---|
| `unknown_method` | 方法不在已实现集合 |
| `invalid_params` | 参数反序列化失败、`timeout_ms` 越界 |
| `forbidden_target` | `node_probe` 目标不是回环地址 |
| `fetch_failed` / `validate_failed` | `reload` 拉取 / 构建失败（既有） |
| `handler_error` | 处理器内部错误 |
| `serialize_failed` | 响应序列化异常 |

## 6. 安全边界

1. 订阅 URL 含 token，**任何 IPC 响应都不得回显**；`source_kind` 只给类别，日志侧也不写 URL。
2. `node_probe` 当前只放行回环，避免未授权的内网扫描；扩展到真实节点前需先过威胁评审。
3. 规划中的 `log_tail` 必须先脱敏（节点地址、UUID、加密串、订阅链接）再下发给 GUI。
4. GUI 若使用 WebView，`token` 不得离开 Rust 层；本契约的响应字段设计已按此约束裁剪。

## 7. 版本与兼容

- 帧 `v` 与 `Metrics` / `NodeProbeResult` 字段新增：向后兼容（新增字段），`v` 不变。
- 字段删除 / 改名 / 语义变化：`v` 递增，且按 FA 升级回滚矩阵要求保留一个版本的双读能力。
- 快照格式与 profile schema 的兼容规则见 `docs/client-design-plan.md` §5「FA 最终验收规格」。

## 8. 验收方式

- 单元测试：`contract.rs`（载荷往返、回环白名单含绕过用例）、`metrics.rs`（RAII 计数）、`relay.rs`（字节计数）。
- 实机回归：`D:\wxmuma\_c0d_test\s10_ipc_contract_probe.py`（未入库），覆盖正常路径、未知方法、参数校验、越权目标、连接/字节计数增长与响应脱敏。
- 既有回归：s5–s9 不受影响（帧协议与 `status` / `reload` 语义未变）；它们用外部域名与 `--socks5-hostname`，不受本机 `NO_PROXY` 影响。
- GUI 回归（2026-11-12）：`D:\wxmuma\_c0d_test\s11b_gui_probe.py` **20/20**（PrintWindow 截图 + WinRT OCR 断言面板内容、计数增长、探测结果、断连状态、干净退出）；被测 `client-gui.exe` 9.3 MiB，SHA-256 `82a58be69d979616f2949682496e89f6b10b4969083ba3c1d4ca00002e8038e1`（深色主题修复后二进制）。
- 本轮被测二进制：`target/release/client-core.exe`，7,683,584 B，SHA-256 `c10e9ad58bb7664125bf91dd6d2e7c71a1bef65e743fce305bd9a345e517b6e8`。
- **测量前提**：本机 `NO_PROXY=127.0.0.1,localhost,::1`，任何回环 workload 的 curl 都必须带 `--noproxy ""`，否则会绕过 SOCKS5 直连目标、计数不增长、数据失效。本轮 18/18 回归正是靠 `connections_total` / `bytes_down` 增长独立证明了流量确实经过内核。
