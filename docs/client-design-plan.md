# 自研客户端设计规划

> 版本：v0.9（2026-11-11 20:30）
> 依据：`v2node/node-rust-client-strategy.md`、`docs/client-handoff-thinking.md`、`docs/vless-encryption-prod.md`、`docs/m1-completion-plan.md`、`v2node-rust/src` 代码实证、Mihomo / sing-box 选型调研、`v2board` 订阅路由核查、`docs/benchmark-spec.md`
> 结论：**客户端不立项**（⚠️ 2026-11-12 更正：该结论已被实现推翻，见下方状态更正块）。本轮交付「提前重构 + 用户侧数据采集 + 加密灰度 + 链路 PoC + 性能基线 + 立项门控」。架构不锁死；**底层全 Rust 重构确立为长期技术方向**。性能验收口径已确认：**以代码执行效率为准，按延迟 / 内存 / 吞吐 / 多线程四维衡量**（附录 C），端到端公网测速不计入；手写汇编默认不引入。
> 已生效规范：`docs/benchmark-spec.md` v1.0 —— **所有后续跨语言测试一律按此标准执行**。
> 已批准并行工作：C0e（性能基线）与 C0b（提前重构）同时启动；范围锁定 **N0–N2 先行，N3 为长期目标**；**Go 内核对照组纳入**。
> C0e 进展：L1 / L2 / L3 三层基准与 criterion 正式工具链均已落地并归档；**Go 对照组因本机无 Go 工具链阻塞**，需构建机执行。
> C0b 进展：**第一步完成 + Linux E2E/Xray 互通已验证**（workspace + `v2node-proto` crate + `#[path]` 清零 + `[workspace.lints]` + CI/Makefile 覆盖），验收见 `docs/benchmark-results/2026-11-11-c0b-after.md`、`2026-11-11-c0b-linux-e2e.md`（构建机 `make quality` 56 passed/0 failed、E2E 三连全过、Xray 26.11.9 互通）；第二步角色解耦待办。
> G1 评审材料：`docs/g1-gate-review.md` —— **当前 No-Go**；C0a 已于 2026-11-11 20:30 在面板机只读执行完成（Clash 系 76.6% / sing-box 0.2% / 移动端 48.9%），维持客户端不立项。
>
> **⚠️ 2026-11-12 状态更正（以此为准，事实依据见 `docs/HANDOVER.md`）**
>
> 1. **C0c 已上线生产**：2026-11-11 17:14 起 <NODE_HOST>（NodeID 10）启用 `mlkem768x25519plus`，面板 id 10–19 全部配置；本机探针 `PROBE_OK` 6 ms、经 Cloudflare 500 ms。原文「C0c 待执行」不再成立。
> 2. **客户端已实现，不再「不立项」**：`crates/client-core` 已完成 SOCKS5 → VLESS/WS/TLS/mlkem 数据面，含 P0 规则引擎、GEOIP/GEOSITE、fake-ip DNS、rule-set（197,596 条）、`url-test`/`fallback`、订阅热重载；2026-11-12 01:50 起已通过构建机全量门禁（105 tests）与热重载实机验证（9 项）。
> 3. **G1 仍未 Go**，但缺失项已变化：不再是「未部署 / 未实现」，而是 C0d Windows 链路 PoC、许可评审结论、内核路线拍板。
> 4. 灰度未收口：仍有约 62% 握手失败来自未刷新订阅的老客户端，处置方式待用户决定（`docs/HANDOVER.md` P0-2）。

---

## 0. 本版变更与已确认决策

| # | 用户决策（2026-11-11） | 对规划的影响 |
|---|---|---|
| 1 | 客户端可不立即立项；依赖与模块较多，需提前重构 | C0 从「三件事」扩展为五阶段，新增模块化依赖清单（§4） |
| 2 | 内核改动可公开；不局限于 fork，可参考 Mihomo / sing-box 优点自研 | 许可不再否决进程内集成；「自研内核」由非目标升为路线 R2（§2） |
| 3 | 内核选型需先对比优劣，决定留到后续 | 新增对比章节（§3）；G1 前不定内核，PoC 用 `CoreAdapter` 屏蔽差异 |
| 4 | 授权采集用户侧数据 | 新增只读采集方案（§6）；已定位到零改造入口 |
| 5 | 希望不止自研 GUI，而是从底层用 Rust / 汇编重构内核与功能模块以提升性能与响应 | 新增附录 C；R2 升为长期技术方向；新增 C0e 性能基线；汇编设门控 |
| 6 | **以代码执行效率作为性能评价标准**：更低延迟、更低内存占用、更高吞吐、更强多线程能力 | 验收口径收敛为四维；重写 C.5 指标体系；修正 C.3 汇编门控逻辑 |

相对 v0.1 的五处实质修正：

1. **许可论证更正**：Mihomo 与 sing-box 同为 GPL-3.0 且各带命名限制条款；进程边界只影响「是否构成衍生作品」，不免除随安装包分发内核二进制时提供源码与许可原文的义务。Xray-core 为 MPL-2.0（文件级 copyleft），属另一类选项。
2. **「协议零改动」更正**：代码实证显示只有 `encryption.rs` 双向可复用；`vless.rs` / `tls.rs` 是服务端专用，抽 crate 必须先做角色解耦（§4）。
3. **门控输入更正**：C1 所需「目标用户客户端版本分布」无法由单用户加密灰度产出，补 C0a 独立采集任务。
4. **性能目标更正**：拒绝以「测速 / 首包延迟」作为底层重构的验收口径。节点侧实测已证明 Rust 重写拿到的是内存与并发收益，公网差异被 CF / 隧道 / WAN 主导；客户端侧结论同构。可测收益与汇编边界见附录 C。
5. **口径收敛与门控修正（v0.4）**：用户明确以「代码执行效率」为准，因此「延迟」拆为端到端延迟（不计入）与代码执行延迟（核心指标）；并修正 C.3 的门控条件——原「热点占端到端总耗时 >10%」在用户态占比 <1% 的链路下永不触发，属于逻辑错误，改为「微基准中占 CPU 时间 >10%」。

---

## 1. 目标与非目标

### 目标

1. 兑现 VLESS Encryption，从「节点支持」变成「用户实际用上」。
2. 控制末段体验：TUN / DNS / 分流 / 重连。
3. 自控分发、签名更新与供应链。
4. **本轮目标**：把协议层、控制面抽象、订阅解析做成可复用模块，且不依赖客户端是否立项。

### 非目标

1. 不承诺「测速翻倍」。公网经 CF 的差异由 CF / 隧道 / WAN 主导。
2. 首版不做全平台。
3. **本阶段不做内核实现决策**，但明确保留自研路线；不因「不立项」而锁死架构。
4. 不做 clean-room 协议栈重写。协议层复用 `v2node-proto`，不重新实现 VLESS Encryption。

---

## 2. 架构决策树（许可放松后重排）

### 2.1 许可事实基线

| 组件 | 许可 | 约束 |
|---|---|---|
| Mihomo | GPL-3.0 | 下游项目名不得含 `mihomo`；分发须附许可原文与源码获取方式 |
| sing-box | GPL-3.0-or-later + 额外命名条款 | GitHub 归一化为 `NOASSERTION`；不得暗示与上游关联 |
| Xray-core | MPL-2.0 | 文件级 copyleft，与闭源代码组合的空间更大，但 TUN / 规则生态弱 |
| wintun.dll | WireGuard LLC 预编译二进制许可 | 可随包分发，须保留声明 |
| geo / rule-set 数据 | 独立项目、独立许可 | 需单独核对 |

**已确认**：内核 fork 改动可公开。因此「打包分发 GPL 内核 + 公开 fork 改动 + 附源码获取方式」是可接受的分发形态；UI 是否闭源不再反向决定架构。

> 本文不是法律意见。正式发布前需一次许可评审，重点是双内核命名限制、geo 数据与商店条款。

### 2.2 四条路线

| 路线 | 形态 | 优势 | 代价 / 排除理由 |
|---|---|---|---|
| FFI 嵌入 | Go 内核 cgo 调 Rust staticlib | 法律障碍已解除 | **仍排除**：Rust panic 跨 FFI 边界会带崩内核；cgo + MSVC / musl 构建链脆弱；Go GC 与 Rust 所有权两套运行时；同进程排障困难 |
| **R1 fork + sidecar** | Go 内核管 TUN / DNS / 规则，出口指向本机 Rust sidecar（SOCKS5） | 验证最快；故障隔离；协议层独立发版；不侵入内核 | 多一跳、本机 IPC 鉴权、双工具链、GPL 分发义务 |
| **R2 Rust 自研内核** | 控制面 + TUN / DNS / 规则 / 协议全 Rust，单进程集成 `v2node-proto` | **单一工具链、无 SOCKS5 一跳、无本机 IPC、无 GPL 分发义务、内存更省**；可参考 Mihomo 规则语义与 sing-box 路由分层的优点 | TUN / DNS / 规则 / 订阅全部自建；wintun + 提权服务；Rust 生态的 clash-rs / leaf 均不成熟；工作量最大 |
| **R3 渐进（本轮采用）** | PoC 用 R1 验证链路，同步建 `CoreAdapter` 抽象层，保留切 R2 的空间 | 不锁死架构；避免「先按旧架构做完、再整体重写」 | 抽象层本身 1–2 人日成本 |

**补充**：若最终产品整体开源，「Go 后端嵌入 Mihomo 库 + 前端壳」（Pandora-Box 模式）也是合法选项，但会引入 Go 工具链，与单一 Rust / TS 栈冲突，不作为首选。

### 2.3 本轮架构原则

1. **协议层与内核解耦**：`v2node-proto` 不依赖内核存在形式，R1 下是 sidecar，R2 下是进程内模块。
2. **控制面抽象先行**：定义 `CoreAdapter`（start / stop / reload / subscribe-events / query-connections）与 `Profile` 模型，屏蔽两套配置语义差异。
3. **不提前引入私有 IPC 扩展**：SOCKS5 保持标准协议，避免削弱进程边界论证。

---

## 3. 内核路线对比（决定留到 G1 之后）

> 本节按用户要求提供对比，**不做决定**。数据口径：公开第三方测试，非受控环境，仅作量级参考，不作为验收依据。

| 维度 | Mihomo | sing-box | 对本项目的相关性 |
|---|---|---|---|
| 定位 | 规则分流隧道，Clash 系事实标准 | 通用代理平台，inbound / outbound / route 分层 | 两者皆为 Go 单二进制，适合「GUI 壳 + 内核子进程」 |
| 配置 | YAML，向后兼容好 | JSON，字段跨版本频繁废弃 / 改名 | 面板已输出 ClashMeta YAML → Mihomo 契合度更高 |
| 规则 | proxy-groups + rule-providers + SCRIPT，语义最丰富 | route.rules + SRS 编译规则集，更省内存 | 复杂分流 Mihomo 强；大规则集 sing-box 省 |
| 订阅生态 | 机场默认格式，`providers` / `includes` 原生 | 主流机场不直接提供，需 Sub-Store 转换层 | **决定性**：现有面板订阅可直接导入 Mihomo |
| 控制面 | Clash API（REST + SSE），现成面板可直接嵌 | 兼容 clash_api，另有 metrics | 自研 GUI 成本 Mihomo 明显更低 |
| Windows TUN | Wintun / gVisor；PPPoE 拨号场景正常 | gVisor 一等公民；有反馈 PPPoE 下 TUN / mixed 无法接管流量 | Windows 是目标平台 → sing-box 实质减分 |
| DNS / fake-ip | fake-ip 下 IP 规则仍生效 | fake-ip 下 IP 规则失效，需在 `dns.rules` 二次分流 | 影响 DNS 泄漏验收口径 |
| 资源 | 三万条规则待机约 68 MB | 约 32 MB；UDP / QUIC 路径 CPU 低 20–60% | 桌面端绝对差小；若走 R2 自研则此优势消失 |
| 维护风险 | 上游贡献集中；命名限制 | 主作者高度集中；配置破坏性变更；命名限制 | 两者都需 pin 版本 + SHA-256 校验 |

**对本项目的初步倾向（非决定）**：Mihomo 更契合现有资产——面板已生成 Clash 系订阅且 `encryption` 字段只下发给 Clash 系（`vless-encryption-prod.md` §2、§5），Clash API 可降低 GUI 成本，YAML 稳定性利于长期维护。sing-box 的资源效率优势在「桌面单用户 + 经 CF 的 VLESS/WS」链路上难以兑现。

**自研路线（R2）的定位**：真正收益不是「更快」，而是消除 Go 工具链、SOCKS5 一跳与 GPL 分发义务；真正成本在 TUN / DNS / 规则 / 订阅，而非协议层（协议层已有）。建议按分层推进，不一步到位：

| 层级 | 自研范围 | 依赖 |
|---|---|---|
| L0 | UI / 控制面 / 订阅解析 | 内核仍用 fork（当前 PoC） |
| L1 | 协议层（`v2node-proto`）与出站管理 | §4 重构完成 |
| L2 | TUN / DNS / 规则引擎 | wintun + 提权服务 + 规则数据许可 |
| L3 | 全自研内核 | 连续投入 ≥2 个月，且 PoC 证明 TUN / DNS 是主要痛点 |

**决策时点**：G1 门控评审。触发 R2 需同时满足 `CoreAdapter` 稳定、用户侧数据显示现成客户端无法覆盖、且有连续投入窗口；否则维持 R1。**但 R2 已被确立为长期技术方向**：其分层重构范围、性能收益地图、汇编边界与可测指标独立于立项决定，详见附录 C；其中 N1 / N2 与节点侧共用，可先行投入。

---

## 4. 提前重构：依赖与模块清单

### 4.1 模块拆解（代码实证结论）

| 模块 | 现状 | 目标形态 | 可复用性 | 粗估 |
|---|---|---|---|---|
| VLESS Encryption | `encryption.rs` 已含双向实现：`from_decryption/handshake`（L397/447）、`ClientEncryptionConfig::parse/client_handshake`（L605/649）、`PumpRole::{Server,Client}`（L786）；`vless_probe.rs:1` 用 `#[path]` 复用 | 抽 `v2node-proto` lib，消除 `#[path]` 技术债 | **高** | 1–2d |
| VLESS 帧 / 地址编解码 | 埋在 `vless.rs::serve_plain`，`VlessContext` 直接持有 `Arc<NodeState>` / `Arc<RuntimeConfig>` / `Arc<NodeInfo>`（L25–32） | 抽出不依赖节点状态的纯函数；限速 / 计量 / 用户校验留在 bin | 中 | 2–3d |
| 节点业务逻辑 | `state.rs` + `vless.rs` 转发路径耦合 | 定义 trait 边界，仅 bin 侧实现 | 不复用 | 1–2d |
| TLS 客户端 | `tls.rs` 是服务端终止 + 热重载（`apply_node` / `build_server_config`）；probe 另写一套 `ClientConfig` | `v2node-proto::tls` 统一客户端配置（SNI / 证书校验 / 自定义 verifier） | 中 | 1d |
| WS 传输 | `listener.rs` 服务端；probe 客户端 | 抽客户端 WS 连接构造 | 中 | 0.5d |
| 订阅解析 | 无 | 新建：Clash YAML → `Profile` 模型，与内核选型解耦 | 新建 | 2–3d |
| 控制面抽象 | 无 | 新建 `CoreAdapter` trait + 事件模型 | 新建 | 1–2d |
| 本地 IPC | 无 | 命名管道或 `127.0.0.1` + 每次启动 token；**走 R2 时此模块可省** | 新建 | 1d |
| 质量门禁 | `[lints]` 为 package 级（`Cargo.toml:49`），`make quality` / CI 只覆盖单 crate | 改 `[workspace.lints]`，CI `quality` 覆盖新 lib crate | 工程 | 0.5d |
| Windows 工具链 | 节点为 musl static-pie | MSVC + `rustls` 的 `aws_lc_rs`（需 CMake / NASM）单独验证 | 工程 | 1d |
| 缓冲池 / 零拷贝 | `AeadState::seal/open` 每次返回 `Vec<u8>`（L225/230），pump 按 `chunks(8192)` 分段并多次 `write_all`（L864–878） | safe Rust：`BytesMut` + 缓冲池 + in-place AEAD，消除逐段分配 | **节点侧同受益** | 2–3d |
| 规则匹配数据结构 | 无（现由内核承担） | 编译规则集 + 零分配匹配，为 N3 预备 | 新建 | 2–3d |
| 性能基线 / benchmark | **代码库零 benchmark**（本轮核查） | `criterion` + `tracing` 分段打点，纳入 CI 观测 | 工程 | 2d |

**合计约 16–23 人日**（一人 + AI 口径，粗估，不含 PoC 与灰度）。其中缓冲池 / 零拷贝 / 规则数据结构约 6–8 人日与节点侧共用，属于「即使客户端永不做也不白做」的先行投入，不占客户端立项预算。这是交接文档 §7.1「抽 crate 成本失控」信号的实测结果：`vless.rs` / `tls.rs` 是服务端专用，「同一份代码两端跑」三份里只有一份成立。

**实施顺序**：先做类型 / 角色解耦（帧编解码与计量分离），再建 crate 并 re-export，最后逐步移动；每步跑 `make quality`（`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`）+ 8 套 E2E + Xray v26.11.9 互通，不得引入新 clippy 警告。E2E 与 clippy 以 Linux 构建机（`<BUILD_HOST_IP>`）结果为准——本地 Windows 与 Linux 的 clippy 版本存在差异，已有误报先例。

### 4.2 外部依赖与前置

| 依赖 | 用途 | 前置动作 |
|---|---|---|
| 内核二进制（Mihomo / sing-box） | PoC 数据面 | pin 版本 + 记录 SHA-256；附 GPL 原文与源码获取方式 |
| wintun.dll | TUN（L2 及以后） | 保留 WireGuard LLC 许可声明；PoC 阶段用系统代理绕开 |
| 规则 / geo 数据 | 分流 | 单独核对许可，不随内核许可推定 |
| 代码签名证书（OV / EV） | 分发 | 采购周期长，C1 之后再启动 |
| 面板只读访问日志 | C0a 采集 | 见 §6 |

---

## 5. C0 阶段与门控

| 阶段 | 内容 | 出口 |
|---|---|---|
| **C0a** | 用户侧数据采集（§6） | ✅ 已完成：Clash 系 76.6% / sing-box 0.2% / 移动端 48.9%，详见 C0a 归档 |
| **C0b** | 提前重构（§4） | ✅ 已完成：`v2node-proto` 抽出、构建机全绿、Linux E2E 三连 + Xray 26.11.9 互通 |
| **C0c** | 加密灰度：独立节点 / 端口 / 订阅 + 单用户 | ✅ 已完成：2026-11-11 17:14 生产上线（NodeID 10）；老客户端握手失败待处置 |
| **C0d** | Windows 链路 PoC | ✅ **5/5 全通过**：TCP/UDP、≥1 MiB、域名、DNS 无泄漏、kill sidecar 自愈；12 h 长稳已移出本阶段，见「FA 最终验收」 |
| **C0e** | 性能基线：三层测量（微基准 / 组件基准 / 本机回环整机）+ `criterion` + CI 观测（附录 C.5） | ✅ 已完成：L1/L2/L3 + criterion 归档，四维基线可复现 |
| **G1 立项门控** | C0a–C0e 数据 + 许可评审 + 内核路线决定 | ✅ **2026-11-12 评审 Go**：MIT 许可、自研 R2 追认、投诉集中在客户端侧；详见 `g1-gate-review.md` §8 |
| C1 / C2 | 原型 / MVP | 仅 G1 通过后展开；**G1 已通过，可启动** |

**依赖关系**：C0c → C0d（PoC 需要可用的加密节点）；C0a / C0b / C0e 三者可并行；C0b → C0d（`v2node-proto` 是 sidecar 前提）；**C0e 先于任何性能优化动作**——无基线不启动优化。

**C0d 通过线（5 条，任一不过即停）**：TCP + UDP 通；≥1 MiB 多记录传输覆盖 8192 字节分片；域名经 SOCKS5 不丢失（ATYP=DOMAINNAME）；DNS 无泄漏；`kill sidecar` 后内核存活并自动重连。

> **2026-11-12 流程调整**：12 h 长稳从 C0d 移出，改为「FA 最终验收」项（见 §5 里程碑表下方规格）。理由：长稳数据要求二进制冻结，若在 PoC 阶段执行，后续 C1/C2 的二进制变化会使数据失效；同时避免它成为 G1 评审的时序阻塞。C0d 出口不再包含长稳，G1 评审无需等待 12 h 数据。

**PoC 最小范围**：单节点单订阅（Clash YAML）、一个加密入站、**先系统代理**（绕开 wintun 与管理员权限）、sidecar 单实例、连接 / 断开 / 状态、本地环形日志、崩溃自愈。不做规则编辑 UI、自有 `v2node://`、自动更新、代码签名、遥测、多节点混合、完整 fake-ip 策略、Android 复用层。

---

## 6. 用户侧数据采集方案（已授权，只读）

### 6.1 已定位的零改造入口

`v2board/routes/web.php:55` 把订阅路由到 `ClientController::subscribe`；`ClientController.php:31–59` 用 `?flag=` 或 `User-Agent` 匹配 `app/Protocols/*.php` 的 `$flag` 来决定返回哪种订阅格式。**因此 nginx access log 中的订阅请求天然携带客户端类型与版本**，无需改面板代码、无需改中间件、无需碰生产数据库。

| `Protocol::$flag` | 对应客户端 | 归类 |
|---|---|---|
| `meta` / `verge` / `nyanpasu` / `clash` | ClashMeta / Clash Verge / ClashNyanpasu / Clash | Clash 系（Mihomo 内核） |
| `sing` | sing-box（含 `sing-box 1.12+` 版本分流） | sing-box |
| `v2rayn` / `v2rayng` / `v2raytun` | v2rayN / v2rayNG / v2rayTun | Xray 系 |
| `shadowrocket` / `loon` / `stash` / `surge` | 移动端 / macOS 客户端 | 其他 |
| `quantumult%20x` / `surfboard` / `passwall` 等 | 其他 | 其他 |

UA 中常含版本（如 `ClashVerge/1.7.0`、`sing-box 1.12.0`），可直接用于 `vless-encryption-prod.md` §5 要求的「真实客户端版本握手测试」前置判断。

### 6.2 只读聚合口径

```bash
# 在面板机执行；SUB_PATH 取 config('v2board.subscribe_path') 的实际值，LOG 取站点 access log
SUB_PATH="/path/to/subscribe"
LOG="<PANEL_ACCESS_LOG>"

# 1) UA 分布（nginx combined 格式，第 6 个引号分隔字段为 User-Agent）
grep -F "$SUB_PATH" "$LOG" \
  | awk -F'"' '{print $6}' \
  | sort | uniq -c | sort -rn | head -50

# 2) 显式 ?flag= 分布（UA 可能是通用 curl / 浏览器，需单独归类）
grep -F "$SUB_PATH" "$LOG" \
  | grep -oE 'flag=[A-Za-z0-9._%-]+' \
  | sort | uniq -c | sort -rn
```

**输出物**：客户端类型分布、Clash 系 / sing-box / Xray 系占比、UA 可识别率、版本下限、Windows 客户端占比上限。

**口径约束**：只输出聚合计数；不采集、不回显 `token` / 用户 ID / IP / 原始日志行；时间窗口 7–30 天；`?flag=` 显式指定与 UA 隐式匹配分别统计，避免重复计数；`sing` 分支在控制器中用 `!strpos` 判断，UA 以 `sing` 开头时可能误分类，归类以命中 `Protocol::$flag` 的实际结果为准。

**安全项**：订阅请求 URL 携带 `token=` 查询参数，历史日志中已有明文残留先例（Node 3 journald 16:49 段）。因此聚合脚本只取 UA / flag 字段，**禁止输出整行、禁止把原始日志拷贝出面板机、禁止归档到仓库**；统计结果只保留聚合计数。

**执行前提（已于 2026-11-11 20:30 满足并执行）**：用户提供了面板机只读 SSH（`root@<PANEL_IP>:2026`，密钥 `<SSH_KEY_PATH>`），已在面板机只读执行 `script/collect_client_ua.sh`，数据源 `<PANEL_ACCESS_LOG>`（订阅命中 1275 条，8 天窗口），聚合结果见 `docs/benchmark-results/2026-11-11-c0a-client-distribution.md`。全程未修改生产中间件、数据库、订阅数据。若未来需更长周期，可纳入 gz 历史日志（当前 www 域名无 `.gz` 滚动）。

---

## 7. 订阅与配置契约

1. 兼容 Clash YAML：复用面板现有订阅，降低迁移成本；`Profile` 模型与之对齐。
2. 自有订阅 `v2node://`（携带 `encryption` / `encryption_settings` / 节点指纹）列为 **C1 之后**的可选项，需面板配合，不进首版。
3. 混合节点集：同一入站不能兼容加密与非加密客户端（`vless-encryption-prod.md` §1），sidecar 需按节点配置管理多出站实例；该逻辑在 `CoreAdapter` 之下，不绑定内核。
4. 面板字段与客户端解析需一致；面板补丁由用户统一处理，本规划只定义契约。
5. **账号集成（2026-11-12 需求变更）**：客户端改为邮箱+密码登录，订阅 URL 由
   `GET /user/getSubscribe` 返回并作为内存 source 注入既有快照流程；账号数据不进入
   内核服务、不进入 UI 层。完整设计与架构锁点见 `docs/c2-account-integration-design.md`
   与 `docs/adr/0004-account-integration-boundaries.md`。面板补丁会被 `update.sh` 的
   `git reset --hard` 覆盖，因此客户端**只依赖 v2board 标准用户端 API**。

---

## 8. 分发、更新与可观测

| 项 | 方案 |
|---|---|
| Windows 安装包 | MSI / EXE，代码签名（EV / OV），C1 后启动采购 |
| 自动更新 | 签名 manifest（Ed25519）+ 版本差分；内核、sidecar、UI 独立更新。**2026-11-13 已实现**：ring Ed25519，公钥编译期内嵌 GUI，私钥 DPAPI 加密存仓库外；双渠道（Gitee 国内优先 `gitee.com/wxbns/mcloud-client` + GitHub `Mcloud136/McloudClient`），GUI 账户区可选更新源（自动/仅 Gitee/仅 GitHub，持久化 `gui-prefs.json`）；验签失败硬拒绝，无降级路径。签名消息 `version\nurl\nsha256`，manifest 增 `signature` 字段（64 字节 hex）。 |
| 内核分发合规 | 随包附 GPL 原文、源码获取方式、fork 改动说明；产品命名与 UI 文案规避 `mihomo` 字样与 sing-box 关联暗示 |
| 崩溃与日志 | 本地环形日志 + 用户主动导出；默认不上传 |
| 遥测 | 默认关闭；如需开启只上报匿名计数（连接成败、错误码），不含域名与 IP |

---

## 9. 里程碑与门控

| 阶段 | 内容 | 进入下一阶段的条件 |
|---|---|---|
| C0 准备 | 数据采集、提前重构、加密灰度、链路 PoC、性能基线 | 见 §5 各阶段出口 |
| **G1 门控** | 用户侧数据 + 许可评审 + 内核路线决定 + PoC 结论 | 版本分布显示现成客户端无法覆盖加密能力，或投诉集中在客户端侧，或出现必须两端配合的新能力 |
| C1 原型 | Windows：内核 + sidecar / 进程内协议层 + 订阅导入 + **节点延迟探测（网络测试）与多节点负载均衡** | 单用户可用；DNS 无泄漏；崩溃可自愈；测速与负载均衡有本机可复现基准（现场归因不可执行，见 `g1-gate-review.md` §8.3）。**2026-11-12 需求变更后**，「单用户可用」升级为「可登录 + 主界面代理/订阅/商店/账户四区可用」，见 `c1-roadmap-2026-11-12.md` §0 |
| C2 MVP | **账号登录 / 账户管理 / 订阅信息 / 订阅购买**（`c2-account-integration-design.md`）+ 分流规则、自动更新、日志导出、连接诊断 | 10 人内测一周无 P0；**凭据边界断言通过**（不落盘 / 不入日志 / 不入 IPC 帧 / 不入截图产物） |
| **FA 最终验收** | 二进制冻结后的 12 h 长稳 + 全量回归 + 脱敏复检 | 所有功能完成后的最后一道技术闸门；未通过不得进入 C3 |
| C3 发布 | 签名分发、灰度通道、回滚预案 | 崩溃率与支持工单达标 |
| C4 协议裁剪 | 两端同步简化握手 / 0-RTT / 私有复用 | 收益可测且不破坏兼容 |

注：原「节点 CPU 长期 <30–40%、RSS 稳定」按现有实测（1 vCPU、空载 6–8 MB）几乎必然满足，**不作为有效立项信号**；立项门控以用户侧数据与 PoC 结论为准。

### FA 最终验收规格（2026-11-12 新增）

12 h 长稳不再属于 C0d，也不进入 G1 评审输入；它排在所有功能完成之后、C3 签名分发之前，是流程上最后执行的技术验收。

| 项 | 要求 |
|---|---|
| 执行时机 | C0–C2 功能冻结、二进制 pin 版本并记录 SHA-256 之后；C3 之前 |
| 失效条件 | 冻结后任何代码变更都会使数据失效，必须重跑 |
| 采集指标 | RSS、句柄数、连接数、FD 上限、链路探测成功率；采样间隔 ≤15 s，连续 12 h |
| 判定 | 无线性泄漏；平台期后斜率趋零；链路探测无持续失败 |
| 同时执行 | 全量回归（s5–s9 + CI 双平台）、脱敏复检、升级/回滚演练 |
| 预算重定义 | C1 引入 UI 后，空闲 CPU 预算按内核 / sidecar / UI 三项分别计量，不再沿用单一 0.5% |

FA 未通过不得进入 C3。`2026-11-12-soak-10min.md` 的 10 分钟采样仅为方法验证，不构成 FA 证据。

#### FA 前置规格一：C1 加入 UI 后的资源预算

原 ADR 0002 §5 的「空闲 CPU ≤ 0.5%（单核折算）」只覆盖内核 + sidecar；UI（client-gui）常驻后不能沿用单一阈值，必须按进程分别计量。以下为**建议预算，待 C1 首个 Release 构建实测校准**：

| 进程 | 空闲预算（单核折算） | 口径 |
|---|---|---|
| `client-core`（服务，session 0） | ≤ 0.2% | 无连接、无 reload，20 min 窗口 |
| `client-sidecar`（控制面） | ≤ 0.3% | 含 1 s 心跳；实测曾达 0.391%（Debug），Release 应更低 |
| UI（client-gui，原生 WinUI） | ≤ 1.0% 窗口可见 / ≤ 0.2% 最小化到托盘 | 无用户交互；最小化时不得保留动画与轮询 |
| 合计 | ≤ 1.5% | 三者之和，作为 FA 判定值 |

测量方法：`GetProcessTimes` 取 kernel+user CPU 时间差 / 墙钟窗口；Release 构建；记录 commit、SHA-256、采样间隔与机器配置。FA 报告必须分别列出三项，不得只报合计。

**内存预算（2026-11-12 D1 定案后补充）**：UI 常驻内存改用本机实测校准值，不沿用公开资料。
`egui 0.37 + glow + winit` 最小应用在本机（RTX 4070 硬件 GL 3.3）地板为 **124 MiB RSS / 113 MiB private**，
完整面板 76–135 MiB 波动来自 GL 驱动分配与工作集修剪。FA 判定值：**UI RSS ≤ 150 MiB**，
与内核（约 16.5 MiB）/ sidecar 分别计量；该阈值绑定被测 GPU 与驱动版本，换机型须重新校准，
不跨机器外推。裁决依据见 `docs/c1-gui-tun-prestudy.md` §11。

#### FA 前置规格二：升级与回滚兼容矩阵

UI 引入后会同时演进 IPC、快照与配置 schema，升级失败必须可回退。三条版本轴独立管理：

| 轴 | 当前版本 | 兼容规则 |
|---|---|---|
| IPC 帧协议 | v1（u32 长度前缀 JSON） | 新增方法用 feature 协商，未知方法返回 `unknown_method` 并降级；破坏性变更必须 major+1 且拒绝旧客户端 |
| 快照文件 | seq 编号 + `current.txt` + `live.json` | 新增字段必须可选或有默认值；旧内核读新快照忽略未知字段（serde 默认行为需在 FA 验证） |
| profile schema | JSON（rules / proxies） | UI 偏好等扩展放独立文件，不混入 `live.json`；保证数据面解析不受 UI 版本影响 |

| 升级/回滚场景 | 期望行为 |
|---|---|
| 内核 v1 ← 控制面 v2（新） | 允许；新 IPC 方法降级，快照仍生效 |
| 内核 v2 ← 控制面 v1（旧） | 安装器拒绝降级，提示一并升级 |
| 快照 schema v2 → 内核 v1 | 允许（忽略未知字段），FA 需实机验证 |
| 升级后 60 s 内健康检查失败 | 自动回滚：恢复上一版二进制 + 升级前的 `live.json` seq |
| 手动回滚 | 保留上一版二进制与最近 5 版快照，回滚后转发探测必须成功 |

升级前置：Ed25519 签名 manifest 校验、二进制 SHA-256 pin、升级前记录当前快照 seq 作为回滚点。FA 必须实际执行一次「升级 → 强制失败 → 回滚 → 转发正常」演练；未演练不得进入 C3。

**签名实现状态（2026-11-13）**：Ed25519 验签已落地（`client-sidecar::update_sign` 单一事实源，GUI `update.rs` 强制验签）。实机三例闭环：合法签名接受 / 未签名硬拒绝 / 错签名硬拒绝（`_gui_spike/run_sign.sh`）。密钥：本机生成，私钥 DPAPI 加密存 `D:\wxmuma\_secrets\mcloud-update\`（永不入库），公钥 `crates/client-gui/src/update-public-key.hex` 入库内嵌；离线备份 `private.pk8.offline-backup` 由用户保管。签名工具 `tools/manifest-sign`（keygen/sign），发布签名经 `_secrets/sign_manifest.py`（DPAPI 解密→签名→擦除临时明文）。**待办**：Gitee 仓库已建（`wxbns/mcloud-client`）但尚未推送代码与签名 manifest；正式发布前需把签名后的 `docs/update-manifest.json` 推到两个渠道。

---

## 10. 风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| 内核路线过早锁死 | 高 | R3 渐进 + `CoreAdapter` 抽象层；G1 前不做决定 |
| 抽 crate 成本失控 | 中高 | 先解耦再建 crate；每步全量回归；已按 10–15 人日重估 |
| GPL 命名限制与分发义务 | 高 | 改动公开（已确认）+ 附原文与源码获取方式 + 命名规避 + 发布前许可评审 |
| sing-box 配置破坏性变更 | 中 | pin 版本 + SHA-256 + 发布级 smoke test |
| Windows TUN / 提权 / 签名 | 高 | PoC 先用系统代理；wintun 与证书放到 C1 后 |
| SOCKS5 域名保真与 UDP | 中 | 列入 C0d 通过线；fake-ip 下 ATYP 需实测 |
| 性能数字被误用 | 中 | §3 的第三方数据仅作量级参考，验收须在目标机型自建 benchmark |
| 用户侧数据不可识别 | 中 | 先算可识别率；退路为小样本问卷，不改生产 |
| 面板补丁被 `update.sh` 覆盖 | 中 | 面板侧改动单独记录，不依赖 `git reset --hard` 后的文件状态 |
| 上游维护集中度 | 中 | 抽象层保留可替换内核；不把业务逻辑写进 fork |
| 性能目标被误设为公网测速提升 | 高 | 验收口径限定为用户态指标（附录 C.2）；CF / 隧道 / WAN 延迟不计入 |
| 手写汇编引入 `unsafe` 与平台差异 | 中高 | 默认不引入；门控条件 + 局部 `#[allow(unsafe_code)]` + 安全不变量文档 + 双平台 CI |
| 无基线导致优化不可证伪 | 中 | C0e 先行；每个优化必须附同环境 before/after，否则不合并 |

---

## 11. 待决策事项

1. ~~**内核选型**~~ ✅ **已定：自研 R2**（ADR 0001；2026-11-12 用户评审「路线依然保持自研」追认）。
   - 同轮决议：**项目采用 MIT 开源**（`LICENSE` 已落地），依赖许可清单见 `README.md`。
   - 同轮输入：**用户投诉集中在客户端侧**（Clash Verge 测试不可用但节点可用、换 NekoBox 正常；网络测试与多节点负载均衡异常），直接驱动 C1 功能优先级。详见 `g1-gate-review.md` §8。
2. ~~**UI 技术栈**~~ ✅ **已定案（2026-11-12 用户裁决「保持原有路线」）**：
   `egui 0.37 + glow + winit`，关闭 D1 与 D3；不做 wgpu 复测、不重启 Tauri 对比。
   依据与预算见 `docs/c1-gui-tun-prestudy.md` §11 / §9；后续计划见 `docs/c1-roadmap-2026-11-12.md`。
3. **UI / sidecar 是否闭源**：许可底线已放松，此项现在只影响分发文案与开源范围，不再反向决定架构。
4. ~~面板机访问方式与 access log 路径~~：✅ 已提供（`root@<PANEL_IP>:2026`），C0a 已执行完成。
5. **C0b 是否本轮启动**：建议启动；它对节点侧也有独立收益，且可随时回退。
6. ~~性能验收口径~~ **已确认**：以代码执行效率四维（延迟 / 内存 / 吞吐 / 多线程）为准，端到端公网指标不计入（附录 C.1、C.5）。
7. ~~汇编门控~~ **已确认**：默认不引入，仅当微基准中热点占 CPU 时间 >10% 且现有 crate 无空间时按门控引入（附录 C.3）。
8. ~~C0e 是否本轮启动~~ **已确认并已启动**：与 C0b 并行；无基线不启动任何优化。L1 harness 见 `examples/bench_proto.rs`，首批结果见 `docs/benchmark-results/2026-11-11-l1-baseline.md`。
9. ~~底层重构范围~~ **已确认**：N0–N2 先行，**N3（TUN / DNS / 规则自研）为长期目标**，本轮不锁定完整实现。
10. ~~对照组范围~~ **已确认纳入**：Go 内核（Mihomo / sing-box）同机同规则集同负载对照，约 2–3 人日，遵循 `docs/benchmark-spec.md` §4。
11. **账号集成设计（2026-11-12 新增，待确认）**：面板版本/分支（`auth_data` 语义）、
    站点是否固定预置、注册/找回密码是否内置、「记住我」默认策略、支付是否允许跳转浏览器、
    登出后是否保持代理可用、首版是否多账号、中文 UI 字体方案、真实账号联调授权。
    默认值与影响见 `docs/c2-account-integration-design.md` §1.3 / §13。

---

## 12. 与现有计划的关系

- 本规划不改变 M1 / M2 收尾项；`TODO.md` 不入库。
- C0b 会动节点源码结构，属 M2 前的重构，需单独评审并跑全量 E2E。
- 生产四节点已于 2026-11-11 18:47 统一升级到 `0.2.0`（`PushIdempotency=true`、`TrafficSpoolPath` 已启用），但 `encryption` 仍全空、加密未灰度；灰度须按 `vless-encryption-prod.md` §6 新增独立节点，不动现有 40 条。
- 客户端正式立项（C1）仍需 G1 门控数据，不建议在此之前投入 UI 与内核集成。

---

## 附录 A：调研与证据来源

- Mihomo README（GPL-3.0 与命名限制）：`github.com/MetaCubeX/mihomo/blob/Alpha/README.md`
- sing-box 许可与 2026 版本状态：VPNSmith《sing-box vs Xray (2026)》、FAUN.dev sing-box 许可说明
- 三者配置 / 协议 / 规则对照：Argsment《对比——选择内核》、代理百科《V2Ray、Xray、Clash、Sing-box……》
- Windows 实测差异（PPPoE / fake-ip / 配置稳定性）：V2EX 1149486《mihomo vs sing-box 使用体验》
- 资源与吞吐（第三方非受控测试）：机场网横评、NodeLoc 94665
- 面板订阅分流逻辑：`v2board/app/Http/Controllers/V1/Client/ClientController.php:29–63`、`routes/web.php:54–55`、`app/Protocols/*::$flag`
- 代码耦合实证：`v2node-rust/src/vless.rs:18–32`、`src/encryption.rs:397/447/605/649/786`、`src/tls.rs:27–28`、`src/bin/vless_probe.rs:1`

## 附录 B：变更记录

| 日期 | 版本 | 变更 |
|---|---|---|
| 2026-11-11 | v0.1 | 初版：C0/C1 门控、sidecar 架构、订阅契约、里程碑 |
| 2026-11-11 | v0.2 | 纳入用户四点决策；更正许可论证与「协议零改动」；新增内核路线对比、提前重构清单、只读采集方案；重排 C0 与 G1 门控 |
| 2026-11-11 | v0.3 | R2 升为长期技术方向；新增附录 C（性能收益地图、汇编边界、N0–N4 分层、可测指标）；新增 C0e 性能基线；重估重构工作量 |
| 2026-11-11 | v0.4 | 验收口径收敛为代码执行效率四维（延迟 / 内存 / 吞吐 / 多线程）；重写 C.5 为三层测量 + 四维指标体系；修正 C.3 汇编门控逻辑；新增对照组决策项 |
| 2026-11-11 | v0.5 | 四项决策确认：跨语言测试统一按 `docs/benchmark-spec.md`；C0e 与 C0b 并行启动；范围锁定 N0–N2、N3 长期目标；纳入 Go 对照组。落地 L1 harness 与首批基线 |
| 2026-11-11 | v0.6 | 推进 C0e L2 组件层：完整加密泵吞吐与 1/2/4/8 worker 扩展曲线实测；据 L2 数据修正 L1「零拷贝优先级」判断；登记 Go 对照组阻塞（本机无 Go 工具链） |
| 2026-11-11 | v0.7 | 正式开发前测试流程收敛：`criterion 0.8.2` 正式接入 + CI `bench-observe` job、L3 本机回环整机层落地、三维基线归档、全量门禁复核；结论见 `docs/benchmark-results/2026-11-11-predev-test-report.md` |
| 2026-11-11 | v0.8 | 推进 G1 前全部可完成工作：C0b 第一步抽 `v2node-proto` crate 并验收、C0a 只读采集脚本、G1 门控评审材料；结论为 **No-Go，维持不立项** |
| 2026-11-11 | v0.9 | C0a 在面板机只读执行完成：Clash 系 76.6% / sing-box 0.2% / 移动端 48.9% / 明确 Windows 5.2%；交接文档 §2.2「覆盖率被卡」前提不成立，内核选型偏向 Mihomo 系/自研，「Windows 优先」前提需修正；G1 评审 v1.1 同步更新 |
| 2026-11-12 | v1.0 | 12 h 长稳从 C0d 移出，新增 FA 最终验收阶段；C0d 通过线 6→5 且 5/5 全通过；G1 评审不再等待长稳数据 |
| 2026-11-12 | v1.1 | **G1 评审 Go**：MIT 许可、自研 R2 追认、投诉集中在客户端侧；C1 原型纳入节点延迟探测与多节点负载均衡，FA 补齐资源预算与升级回滚矩阵 |

---

## 附录 C：底层重构与性能目标（v0.4）

### C.1 目标界定与验收口径（已确认）

用户意图：不只是自研 GUI，而是从底层以 Rust / 汇编重构内核与功能模块，提升代码执行效率。本规划**采纳为长期技术方向**。

**已确认验收口径（2026-11-11）：以代码执行效率为唯一性能评价标准**，四个维度：

| 维度 | 含义 |
|---|---|
| 更低延迟 | 代码执行延迟：单次解析 / 封装 / 决策的 CPU 时间 |
| 更低内存占用 | 空载、稳态、峰值 RSS 与每连接增量、每 MiB 分配量 |
| 更高吞吐 | 本机环境下单位时间处理的字节数 / 连接数 / 决策数 / 记录数 |
| 更强多线程能力 | 并行扩展性、锁竞争、P99 随并发的变化、背压行为 |

**「延迟」必须两分**，否则口径会混：

| 类别 | 含义 | 是否计入验收 |
|---|---|---|
| 端到端延迟 | 用户操作到页面响应，含 CF / cloudflared / WAN / 系统调用 | **否**，仅作回归冒烟 |
| 代码执行延迟 | 单次函数 / 单次记录 / 单次决策的 CPU 时间，本机回环或内存流测量 | **是**，核心指标 |

同构证据（策略文档 §3.2、`m1-completion-plan.md` 附录 D）：节点侧 Rust 重写实测拿到空载内存 55–64 MB → 7.7–8.0 MB（约 1/7–1/8）、16 并发服务端 CPU 0.19–0.20s → 0.11–0.14s；而公网经 CF 的 5 MB 下载方差达 Rust 0.29–3.75 MB/s、Go 0.22–3.45 MB/s。**端到端测速不可作为代码效率证据；代码执行效率必须用隔离环境测量。** 这正是 C.5 三层测量方法存在的原因。

> 本口径已由用户确认适用于**所有后续编程语言之间的测试**，并独立固化为 `docs/benchmark-spec.md` v1.0（已生效）。跨语言对比须遵循该规范 §4 的同机对照组约束；首批 L1 结果见 `docs/benchmark-results/2026-11-11-l1-baseline.md`。

### C.2 性能收益地图

| 指标 | 当前主导因素 | Rust 重构能否改变 | 预期性质 |
|---|---|---|---|
| 公网首包延迟 / 下载速度（端到端） | CF 边缘、隧道、WAN、TLS 握手 RTT | **否** | 不计入验收，仅作冒烟 |
| 真实 DNS 查询 | 上游递归 + 网络 | 否 | 同上 |
| TUN 收包 / WinSock | wintun 驱动、内核态、IOCP | 否 | 系统调用边界，不计入验收 |
| **代码执行延迟** | 数据结构、分配、锁、算法 | **是** | **核心验收维度**，µs 级 |
| 冷启动到可连接 | 规则 / geo 解析、TUN 建立、内核启动 | **是** | 目标值待 C0e 基线确定，不预设 |
| 规则匹配 / 分流决策 | 规则集规模与数据结构 | **是** | 编译规则集 + 零分配匹配 |
| 常驻 / 峰值内存 | Go GC 堆、双进程 | **是** | 桌面端 68 MB 量级 → 10–20 MB 量级 |
| 代码吞吐 | 分配次数、锁竞争、单连接单任务模型 | **是** | **核心验收维度** |
| 并行扩展性 | 全局锁、任务调度 | **是** | **核心验收维度** |
| 数据面分配与拷贝 | `Vec<u8>` 逐段分配、8192 字节分片 | **是** | 缓冲池 + in-place AEAD，safe Rust 即可 |
| 断线重连耗时 | 重连逻辑与状态机 | 部分 | 语言影响小，主要取决于实现 |
| UI 交互帧耗时 | WebView（若选 Tauri） | 取决于 UI 选型 | 原生 Rust UI（slint / egui）可再降一档 |

**确定性收益是代码执行效率四维 + 供应链与分发形态；不是单用户测速。** 这与节点侧 M1 的结论一致，不因换成客户端而改变。

### C.3 汇编的边界（默认不引入）

三条硬事实：

1. **与质量门禁直接冲突**：`Cargo.toml:51` 为 `unsafe_code = "deny"`；本轮核查确认 `src/` 全库零 `unsafe`、零 `asm!`、零显式 SIMD。手写汇编（`core::arch::asm!`）必须开 `unsafe`，需单独评审、局部 `#[allow(unsafe_code)]`、安全不变量文档与双平台 CI。
2. **汇编收益已间接包含在依赖里**：`rustls` / `tokio-rustls` 使用 `aws_lc_rs`（BoringSSL 派生，自带 x86_64 / aarch64 平台汇编）；`aes` / `chacha20poly1305` 走 AES-NI / RustCrypto 后端；`blake3` 有 SIMD。`encryption.rs:22–60` 的手写标量 BLAKE3 derive-key 是为兼容 Xray 二进制 context，只在握手调用一次，不在数据面热路径，优化价值近零。
3. **真正的热路径是分配与拷贝，不是算术**：`AeadState::seal/open` 每次返回 `Vec<u8>`（L225 / L230），pump 按 `chunks(8192)` 分段并多次 `write_all`（L864–878）。这类优化用 safe Rust 的 `BytesMut`、缓冲池、in-place AEAD 与 SIMD crate（`safe_arch` / `wide`）即可拿到大部分收益。

**门控条件（v0.4 修正）**：仅当 C.5 的**微基准**证明某热点占 CPU 时间 >10%、且现有 crate 与编译器自动向量化已无空间时，才允许以「单点 + benchmark 佐证 + 双平台测试 + 单独评审 + 安全不变量文档」引入。原 v0.3 写的是「占端到端总耗时 >10%」——用户态在该链路占比 <1%，该门控永远不会触发，属于逻辑错误，故改为微基准口径。默认路线为**全 safe Rust**。

**明确不做**：重写 Windows TCP/IP 栈、wintun 驱动、系统调用层。这些是操作系统边界，不属于用户态重构范围。

### C.4 分层重构范围

| 层 | 范围 | 语言 | 与立项的关系 |
|---|---|---|---|
| N0 | UI / 控制面 / 订阅解析 | Rust（原生 UI 或 Tauri） | 依赖 G1 |
| N1 | 协议层 `v2node-proto` | Rust safe | **已有，待抽 crate；节点侧共用** |
| N2 | 传输 + 加密 + TLS | Rust safe，复用现有 crate | **已有；节点侧共用，可先行优化** |
| N3 | TUN / DNS / 规则引擎 | Rust safe，新建 | 工作量最大（wintun + 提权服务），依赖 G1 |
| N4 | 极窄汇编热点 | 汇编（默认不做） | 受 C.3 门控约束 |

N1 / N2 属于「即使客户端永不做也不白做」的投入，与 §4 的缓冲池 / 零拷贝 / 规则数据结构合并推进；N0 / N3 才占用客户端立项预算。这与「不立项」的判断不矛盾。

### C.5 四维指标体系与测量方法

#### A. 更低延迟（代码执行时间）

| 指标 | 定义 | 测量层 |
|---|---|---|
| VLESS 请求头解析 P50 / P99 | 单次 parse 的 CPU 时间 | 微基准 |
| 加密记录封装 / 解封 P50 / P99 | 单个 8192 字节记录 `seal` / `open` | 微基准 |
| 规则匹配单次决策 P50 / P99 | 3 万条规则集下单次 match | 微基准 |
| 本机回环 echo RTT P50 / P99 | `DuplexStream` 或 `127.0.0.1` + mock 节点，不含公网 | 组件 / 整机 |
| 分段打点 | TUN → 规则 → 出站 → 加密 → WS/TLS → syscall | 整机 |
| 冷启动到首连接可用 | 进程启动 → 首个连接就绪 | 整机 |

#### B. 更低内存占用

| 指标 | 定义 |
|---|---|
| 空载 / 稳态 / 峰值 RSS | 进程采样，区分启动后与长稳压测 |
| 每连接增量内存 | 1 / 40 / 200 / 1000 连接阶梯 |
| 每 MiB 分配次数与分配字节数 | 全局分配计数器 |
| 缓冲池命中率 / 复用率 | 组件基准 |
| 12 h 长稳压测内存增长 | 判定无泄漏；**归属 FA 最终验收，非 C0d 通过线** |

#### C. 更高吞吐（代码本身，不含公网）

| 指标 | 定义 |
|---|---|
| 单连接本机回显吞吐 | MB/s，`DuplexStream` |
| 加密泵吞吐 | MB/s，含 / 不含 AEAD 两组 |
| 新建连接数 | connections/s，本机回环 |
| 规则决策数 | decisions/s，微基准 |
| 加密记录数 | records/s，8192 字节记录 |

#### D. 多线程能力（并行扩展性）

| 指标 | 定义 |
|---|---|
| 扩展性曲线与加速比 | 1 / 2 / 4 / 8 worker 下的吞吐与加速比 |
| P99 随并发连接数变化 | 1 → 1000 连接阶梯 |
| 锁等待时间 / 争用次数 | `parking_lot::Mutex`、`DashMap` 分片、`ArcSwap` 读路径 |
| Tokio 任务数与 CPU 利用率 | 运行时打点 |
| 背压行为 | 超容量不崩、内存不无限增长、不丢连接 |

#### 三层测量方法（隔离网络，否则测到的是链路而非代码）

1. **微基准**：`criterion`，纯函数，无 IO、无网络。对应 A 类前三项、C 类后两项。
2. **组件基准**：`DuplexStream` / 内存流，走完整协议栈但不进网络。对应 A 类回环 RTT、B 类分配、C 类吞吐。
3. **整机基准**：本机回环 + mock 面板 / 节点，复用 `vless_probe` 与 `tests/e2e*.sh` 的本机探针方法。对应分段打点、并发扩展、长稳压测。

公网 CF 链路**只做回归冒烟，不作验收数据**。

#### 对照组与基线模板

- **对照组**：Go 内核（Mihomo / sing-box）在**同一机器、同一规则集、同一负载**下跑同一套 benchmark；否则「比 Go 高效」不成立。是否本轮纳入见 §11 第 10 项。
- **节点侧可复用基线**（策略文档附录 A、`m1-completion-plan.md` 附录 D）：16 并发 × 1 MiB 服务端 CPU 0.11–0.14s、耗时 481/651 ms；空载 RSS 7.7–8.0 MB、40 连接后 15–22 MB。节点是入站、客户端是出站，**绝对值不可直接比较**，只沿用口径与趋势。
- Rust 侧与节点侧共用同一套 benchmark 定义，便于比较单位成本。

#### 判定规则

1. 每类指标记录 baseline → 优化后 → 变化率；**目标数值在 C0e 基线出来后确定，不预设数字**。
2. `cargo bench` 纳入 CI **观测**（先积累约 2 周基线，不设硬阈值）；`tracing` 分段打点常驻。
3. **无基线不启动优化；每个优化必须附同环境 before/after，且四维不得出现未说明的回退**（避免用内存换吞吐却无人察觉）。
4. 吞吐或延迟优化若导致 P99 恶化 >5%，视为回退，不予合并。

### C.6 四项决策已确认（v0.5）

| 项 | 结论 |
|---|---|
| 跨语言测试标准 | **所有后续编程语言之间的测试**统一按 `docs/benchmark-spec.md` v1.0 执行 |
| 汇编门控 | 默认不引入，仅按 C.3 修正后的微基准门控引入 |
| C0e / C0b | **并行启动**；无基线不启动优化。C0e 已落地 L1 微基准与 **L2 组件层**（完整加密泵吞吐、worker 扩展曲线），结果分别见 `docs/benchmark-results/` 两份基线文件 |
| 重构范围 | **N0–N2 先行，N3 为长期目标**，本轮不锁定完整实现 |
| Go 对照组 | **纳入**，同机同规则集同负载，约 2–3 人日 |

**C0b 的执行边界**：抽 crate 属于 M2 前的节点源码重构，需单独评审并在 Linux 构建机（`<BUILD_HOST_IP>`）跑全量 E2E；本轮只完成不改动源码的准备工作（模块清单、接口契约、基准规范），不直接改 `src/`。
