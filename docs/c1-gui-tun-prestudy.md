# C1 GUI / TUN 技术预研（2026-11-12）

> **状态**：预研，不产生生产代码。G1 已判 Go（`g1-gate-review.md` v1.3），C1 可启动。
> **约束**：低内存占用优先；候选语言 Rust / Zig / 汇编；Windows 优先；与现有双进程架构兼容；MIT 分发兼容。
> **资料口径**：下文体积/内存数字来自 2026-11-12 联网检索的公开资料，**非本机实测**；C1 首个 Release 原型必须实测校准。

---

## 1. 硬约束

| 约束 | 来源 |
|---|---|
| UI 空闲 CPU ≤1.0%（可见）/ ≤0.2%（托盘）、合计 ≤1.5%，三项分别计量 | `client-design-plan.md` §5「FA 前置规格一」 |
| 常驻内存尽可能低（用户明确要求） | 2026-11-12 用户指示 |
| 订阅 `token` 不进入 UI/WebView 层，日志可导出且脱敏 | `g1-gate-review.md` §8.3、`README.md` 安全注意 |
| 内核持 TUN 网卡需管理员权限，UI 不提权 | Windows 平台要求 |
| DNS 无泄漏为 C0d 既有通过线，TUN 引入后必须继续成立 | `docs/benchmark-results/2026-11-12-c0d-windows-poc.md` |
| 不引入与 MIT 冲突的强 copyleft 依赖 | `LICENSE`、`README.md` 许可章节 |

---

## 2. GUI 技术路线对比

| 方案 | 语言 / 渲染 | 二进制体积 | 常驻内存（简单应用） | 许可 | Windows 成熟度 | 结论 |
|---|---|---|---|---|---|---|
| **egui / eframe** | Rust，即时模式 wgpu | 4–6 MB | **极低** | MIT / Apache-2.0 | 高；IME 部分支持 | **首选候选** |
| **iced** | Rust，Elm 架构 wgpu | 5–8 MB | 较低 | MIT / Apache-2.0 | 中高；屏幕阅读器 / IME 有缺陷 | 次选 |
| **fltk-rs** | Rust + FLTK(C++) | 约 1 MB | 极低 | MIT | 中；控件风格老旧 | 极致轻量备选 |
| **slint** | Rust，声明式自研渲染 | 5–9 MB | 极低 | **GPLv3 / 商用双许可** | 高 | **需商用授权，否则与 MIT 分发冲突** |
| **Tauri 2** | Rust + 系统 WebView2 | 3–10 MB | **30–50 MB** | MIT / Apache-2.0 | 高 | **不推荐**：内存违背约束 |
| **Dioxus** | Rust + WebView/wgpu | 4–12 MB | 30–50 MB | MIT / Apache-2.0 | 高 | 不推荐（同上） |
| **Zig + zigwin32 原生 Win32** | Zig，直接 Win32 API | 单 exe 约 1.1 MB（公开案例） | 最低（无运行时） | 取决于自身 | 可行；控件需自建 | 内存最优，但引入第二套工具链 |
| **Vercel native / zero-native** | Zig 原生渲染 / WebView 壳 | <6 MB / 2–5 MB | 原生版极低；zero-native 常驻 <20 MB | 实验性 | **macOS 优先，Windows 支持弱，未 production-ready** | 不作首版依赖 |
| **汇编构建 UI 主体** | 汇编 + Win32 | — | — | — | 无控件生态、ABI/clobber 易错、不可移植 | **不作为主体**，见 §6 |

检索来源（2026-11-12）：Rust 桌面 GUI 框架横评（CSDN / zeeklog / tsight）、`github.com/leebase/lfznotepad`（Zig + 原生 Win32 单 exe 1.1 MB）、Vercel Labs `native` / `zero-native` 公开介绍。

### 关键修正

上一轮曾建议 **Tauri 2**（理由是复用 Rust 后端）。按用户「低内存优先」的新约束，该建议不成立：Tauri 依赖系统 WebView2，公开资料显示简单应用常驻内存 30–50 MB，而 egui / iced / fltk / Zig 原生均属「极低」档。**预研结论改为纯 Rust 原生渲染优先，不用 WebView 方案。**

---

## 3. GUI 推荐路线（分阶段）

**首选：Rust + egui，与 `client-sidecar` 合并为同一进程。**

- 复用现有 `client-sidecar`（IPC 帧协议、快照、订阅拉取、退避重连），不引入第二套语言栈；
- 避开单连接 IPC 改造：sidecar 本来就是控制面，GUI 是它的前端，管道仍只有一个客户端；
- MIT / Apache-2.0 与本项目 MIT 兼容；
- 即时模式适合「状态面板 + 诊断」这类只读起步的界面，迭代快。

**次选（仅当实测内存仍超预算）：Zig + `zigwin32` 极薄前端**，通过 C ABI 链接 Rust 静态库。优点是内存最低、二进制最小；代价是第二套工具链、自建控件与构建链、维护成本上升。Vercel 的 Zig 方案目前实验性且 Windows 支持弱，只作观察项。

**分阶段**：只读状态面板（status / 规则 / 节点延迟）→ 订阅导入与规则切换 → 诊断与日志导出 → 托盘与最小化。每一步都跑一次内存/CPU 采样，作为 FA 预算的校准数据。

---

## 4. TUN 技术路线

### 4.1 事实标准与许可

**WinTun** 是 WireGuard 官方为 Windows 开发的 Layer 3 TUN 驱动，用户态通过 `wintun.dll` 读写 IP 包，类似 Linux `/dev/net/tun`。要点：

- 驱动源码 GPL-2.0，但**官方提供的预编译签名 DLL 采用更宽松的许可**（随 ZIP 附许可文本），是唯一受支持的分发方式；官网提供 0.14.1 与 SHA-256。
- 创建/打开网卡适配器**必须管理员权限**。
- 分发前须把该 DLL 许可与声明纳入 `README.md` 的第三方清单，并在发布前跑 `cargo deny` / 法务复核。

### 4.2 语言与绑定候选

| 方案 | 语言 | 特点 | 许可 |
|---|---|---|---|
| `wintun` crate 0.5.1 | Rust | 最常用 FFI 安全封装；需外部 `wintun.dll`；`load_from_path` 含 unsafe | MIT |
| `wintun-rs` | Rust | 纯 Rust 重实现用户态逻辑，**内嵌签名驱动、无外部 DLL**、100% safe API、RAII 清理僵尸适配器 | 见 crate 声明 |
| `tun-rs` / `tappers` / `tunio` | Rust | 跨平台抽象，Windows 底层仍是 WinTun | BSD-3-Clause / MIT / Apache-2.0 |
| Zig + `zigwin32` + WinTun C ABI | Zig | 直接调 C ABI，无 GC / 无运行时；需自建 RAII、SetupAPI 驱动安装与生命周期管理 | 取决于自身 |

**推荐**：`wintun` 或 `wintun-rs`（Rust），与内核同语言、同构建链；`wintun-rs` 免外部 DLL 对安装器最友好，但成熟度和驱动内嵌的许可形态需在 C1 前复核。**Zig 路径暂不引入**，除非 Rust 绑定的内存/性能实测不达标。

### 4.3 进程模型

```
UI（用户会话，egui，合并在 sidecar 进程内）
  └─ 命名管道（单连接）→ 内核服务 client-core（session 0，管理员/SYSTEM）
                            ├─ WinTun 网卡会话（提权部分只在内核服务）
                            ├─ 用户态协议栈（Windows 推荐 gVisor / mixed，system stack 风险高）
                            ├─ fake-ip DNS + 规则分流
                            └─ VLESS / WS / TLS / mlkem 出站
```

UI 不提权、不接触驱动；TUN 会话随内核服务启停，崩溃恢复沿用现有 `sc failure`。

### 4.4 DNS 无泄漏（TUN 引入后的头号风险）

Windows 会向**所有网卡并发**发起 DNS 查询（smart multi-homed name resolution），仅靠默认路由无法防漏。公开资料给出的做法：

1. `auto-route` + `auto-detect-interface`：自动配置系统路由，并给**代理出口 IP 加 /32 直连路由**，避免出站流量回环进 TUN。
2. `dns-hijack: tcp/udp://any:53` + Fake-IP，把 DNS 收进自有解析器。
3. **Strict Route**：用 WFP 过滤规则阻断除 TUN 网卡外所有网卡的 53 端口；禁用 IPv6 时同时阻断其他应用的 IPv6。Windows 10/11 可用，Win7/8 不可用。
4. 较弱的回退：组策略 / 注册表 `DisableSmartNameResolution`（只改变查询对象，不阻断）。
5. **浏览器 DoH / 适配器 DoH 不走 53**，Strict Route 拦不住；需在 UI 明确提示或提供策略处理。
6. QUIC/WebRTC、UWP 应用、Docker/WSL2 虚拟交换机都是易漏点，验收要单独覆盖。

**验收**：复用 C0d 的 DNS 无泄漏用例，新增「`curl.exe` 无代理环境变量 → 出口 IP 等于节点出口」「DNS 服务器归属」「WebRTC 不暴露本机公网 IP」「UWP 应用流量进入连接面板」四项，并在 FA 回归套件中固化。

---

## 5. 与现有架构的接口契约

GUI 接入前需补齐 IPC 能力（当前只有 `status` / `reload` / 事件）：

| 方法 | 用途 |
|---|---|
| `metrics` | 连接数、流量、节点延迟、CPU/RSS 采样 |
| `node_probe` | 节点延迟探测（网络测试），对应投诉诉求 |
| `balance_policy` | 多节点负载均衡策略查询/切换 |
| `log_tail` | 日志尾部读取与导出（脱敏后） |
| `diagnose` | DNS 泄漏自检、路由表与网卡状态 |
| `tun` | TUN 开关与状态（C1 后期） |

`token` 只留在 Rust 控制面：URL 拉取、快照写入、IPC 请求都在 sidecar 内完成，UI 只拿到结构化结果。

---

## 6. 汇编的定位（明确边界）

用户把汇编列为候选语言之一。工程判断是：**汇编不适合作为 GUI 或 TUN 的主体实现**，理由：

- 无窗口控件、无高级网络抽象，需自建整套 Win32 消息循环与驱动交互；
- 手写 `asm!` 需逐条审计 ABI 与 clobber 列表，错误即未定义行为，且跨 CPU 不可移植；
- 公开资料的一致结论是「先测再写」：LLVM 自动向量化对多数循环已足够，raw asm 只在编译器无法推断（gather/scatter、跨迭代依赖、非标准位宽）或需要特定指令（如 CRC32、ARM `crc32cx`）时才划算。

**沿用附录 C.3 门控**：仅当微基准证明某热点占 CPU 时间 >10% 且 safe intrinsic / 现有 crate 无空间时引入。顺序为 `core::arch` intrinsics 或 `crc32fast` 这类条件编译 SIMD → 极窄 `asm!`。硬性要求：提供标量回退、用 proptest 做正确性对比、用 criterion 记录 before/after、运行时 `is_x86_feature_detected!` 分发、每个 unsafe 块写明不变量。TUN 侧潜在热点是 IP 校验和与加密帧处理，**必须先有基准数据再决定是否引入**。

---

## 7. 风险与验证

| 风险 | 缓解 | 验证方式 |
|---|---|---|
| 公开内存数字非本机实测 | C1 首个 Release 原型实测 RSS / CPU | 20 min 窗口，按内核 / sidecar / UI 三项分别计量，写回 FA 预算 |
| slint 的 GPLv3 与 MIT 冲突 | 改选 egui / iced / fltk，或取得商用授权 | `cargo deny` 许可扫描 |
| WinTun 签名 DLL 分发许可 | 附官方 ZIP 内许可文本与 SHA-256 | 发布清单 + 法务复核 |
| Zig / Vercel 方案实验性、Windows 支持弱 | 不作首版依赖，仅观察 | C1 前复核成熟度 |
| 第二套工具链维护成本 | 仅在实测不达标时引入 | 决策点 D3 评审 |
| DNS / WebRTC / DoH 泄漏 | §4.4 六条 + 验收用例 | C0d 用例复用 + 新增四项探测 |
| 单连接 IPC 与 GUI 并存 | GUI 合并进 sidecar | s9 回归保持 11/11 |

---

## 8. 决策点与下一步

| 决策 | 选项 | 定案时机 |
|---|---|---|
| D1 GUI 框架 | egui / iced / fltk-rs | 最小只读原型实测 RSS 后 |
| D2 WinTun 绑定 | `wintun` / `wintun-rs` / `tun-rs` | 驱动内嵌许可与成熟度复核后 |
| D3 是否引入 Zig | 是 / 否 | 仅当 Rust 方案实测不达标 |
| D4 汇编热点 | 引入 / 不引入 | 微基准 >10% 门控触发后 |

**建议的 C1 首个里程碑**：`egui` 只读状态面板 + IPC `metrics` / `node_probe`，在 Release 构建下实测内存与 CPU，据此关闭 D1/D3；同期完成 WinTun 绑定的许可与成熟度复核（D2）。不写生产 TUN 代码，直到 D2 与 DNS 无泄漏验收方案通过评审。

**明确不做（C1）**：汇编主体实现、Electron / CEF / WebView 壳、移动端、自有 `v2node://` 订阅格式、多节点的跨设备同步。
