# G1 门控评审材料：C1 客户端正式立项前

> 版本：v1.3（2026-11-12 06:50）
> 用途：汇总 C1 立项前所有门控输入，记录评审决议
> **结论：Go（立项通过，附工程约束）**。2026-11-12 用户评审补齐三项决策输入：① 项目采用 **MIT 开源**；② 内核路线**保持自研 R2**（ADR 0001 追认）；③ 用户投诉集中在**客户端侧**——Clash Verge 中「测试不可用但节点实际可用」，换用 NekoBox 等客户端正常，且网络测试与多节点负载均衡功能异常。按 §5 判定规则「投诉集中在客户端侧」成立，C1 启动前置已齐；约束见 §8.3。
>
> **⚠️ 历史状态更正（现场实测）**：C0c 加密灰度已上线生产（<NODE_HOST> / NodeID 10，`mlkem768x25519plus`）；客户端双进程、订阅 URL、Windows 服务与崩溃自愈均已实机验证（s5 8/8、s6 7/7、s9 11/11），CI 双平台 success。本文 v1.2 及之前的「待部署 / 待执行」「No-Go」等表述**以本节 v1.3 决议为准**。**另据 2026-11-12 流程调整**：24 h 长稳移出 C0d，归入 FA 最终验收，C0d 通过线由 6 条改为 5 条，G1 不等待长稳数据。

---

## 1. 门控清单

| 项 | 要求 | 状态 | 证据 / 阻塞 |
|---|---|---|---|
| C0a 用户侧客户端版本分布 | 目标用户 Mihomo / sing-box / Xray / 其他占比、Windows 占比、最低支持版本 | **已完成（2026-11-11 20:30）** | 数据见 `docs/benchmark-results/2026-11-11-c0a-client-distribution.md`；面板机只读 SSH 已提供（root@<PANEL_IP>:2026），实测数据源 `/www/wwwlogs/<PANEL_ACCESS_LOG>` 订阅命中 **1275 条**（8 天窗口），可识别率 99.4% |
| C0b 提前重构 | 抽 `v2node-proto` crate | **第一步完成 + Linux E2E/Xray 互通已验证 / 第二步待做** | `docs/benchmark-results/2026-11-11-c0b-after.md`、`2026-11-11-c0b-linux-e2e.md`；`make quality` 56 passed/0 failed、E2E 三连全过、Xray 26.11.9 互通完整 |
| C0c 加密节点灰度 | 独立节点/端口/订阅 + 单用户，产出握手耗时、CPU、回滚演练 | **已完成（2026-11-11 17:14 上线，生产在用）** | <NODE_HOST> / NodeID 10 启用 `mlkem768x25519plus`，面板 id 10–19 全量配置；本机 `PROBE_OK` 6 ms、经 Cloudflare 500 ms。**未收口**：约 62% 握手失败来自未刷新订阅的老客户端，处置待定，见 `docs/HANDOVER.md` P0-2 |
| C0d Windows 链路 PoC | 全链路跑通 + 5 条通过线 | **✅ 5/5 全通过** | `client-core` 的 TCP/UDP/域名/1 MiB/DNS 通过；`kill sidecar` 自愈由 s5 8/8 与 s9 11/11 补齐；Mihomo v1.19.34 的 UDP/DNS 负面证据仍成立（内核选型依据）。24 h 已移出本阶段。见 `docs/benchmark-results/2026-11-12-c0d-windows-poc.md` |
| C0e 性能基线 | 三层基准 + 四维指标 | **完成** | `docs/benchmark-results/`（L1/L2/L3/criterion/总报告） |
| Go 内核对照组 | 同机同规则集同负载跑 W1–W10 | **阻塞 → 部分就绪** | 构建机 `<BUILD_HOST_IP>` 现已可达（SSH root + 面板密钥），但 `go` 未安装；需先装 Go + 拉取 Mihomo/sing-box 二进制，约 2–3 人日 |
| 许可评审 | 正式结论 | **✅ 已决议：MIT**（2026-11-12） | 用户评审结论；`LICENSE` 已落地；依赖许可（MPL-2.0 / CC-BY-SA 2.0）见 `README.md` 与 §3.1 |
| 内核路线决定 | Mihomo / sing-box / 自研 R2 | **✅ 已定并追认：自研 R2** | ADR 0001 已接受；2026-11-12 用户评审明确「路线依然保持自研」 |
| 用户投诉分布 | 是否集中在客户端侧卡顿 / 重连 / DNS | **✅ 已提供（定性）** | 用户评审：Clash Verge 测试不可用但节点可用、换 NekoBox 正常；网络测试与多节点负载均衡异常。详见 §8.1；量化分布待 C1 补采 |

---

## 2. C0b 第一步验收摘要

- 已建 workspace 与 `v2node-proto` crate，`#[path]` hack 清零，`[lints]` 升级为 `[workspace.lints]`，CI / Makefile 覆盖新 crate。
- 门禁：`fmt` / `clippy -D warnings` / `test`（64 唯一测试）/ `check --all-targets --all-features` / `bench --no-run` / `git diff --check` 全绿。
- **按规范 §5 重跑基准后不宣称「零回退」**：W1 165.63 → 183.00 ns（+10.5%，复测确认稳定），`#[inline]` 无效已撤销；工程影响可忽略但需评审豁免或 Linux 复测。详见验收记录。
- **硬前置已消除**：Linux E2E 三连（`e2e.sh` / `e2e_encryption.sh` / `interop_xray.sh`）已于 2026-11-11 在构建机全过，Xray 26.11.9 互通完整（见 `2026-11-11-c0b-linux-e2e.md`）。

---

## 3. 许可评审清单（G1 输入）

| 事项 | 已核实事实 | 待确认 |
|---|---|---|
| Mihomo | GPL-3.0，附加命名限制（下游项目名不得含 `mihomo`） | 产品命名与 UI 文案规避方案 |
| sing-box | GPL-3.0-or-later + 命名限制，GitHub 归一化为 `NOASSERTION` | 是否接受其额外条款 |
| Xray-core | **MPL-2.0**（文件级 copyleft），TUN / 规则生态弱 | 是否作为第三条备选路线 |
| wintun.dll | WireGuard LLC 预编译二进制许可，可随包分发 | 保留声明文本 |
| geo / rule-set 数据 | 独立项目、独立许可，**不随内核许可推定** | 逐个核对数据源 |
| 分发义务 | 打包分发 GPL 内核须附许可原文、源码获取方式；fork 改动须公开 | 用户已确认「内核改动可公开」，需落实到发布清单 |
| UI / sidecar 闭源 | 许可底线放松后不再反向决定架构 | 只影响分发文案与开源范围 |
| 整体开源选项 | 若整体开源，Go 后端嵌入内核（Pandora-Box 模式）亦合法 | 是否与产品定位冲突 |

**注意**：进程边界只影响「是否构成衍生作品」，**不免除分发 GPL 二进制时的源码义务**。正式发布前需一次法务评审。该清单针对「打包 Mihomo / sing-box」的场景。

### 3.1 2026-11-12 评审决议与许可落地

用户评审结论：**本项目采用 MIT 开源**，内核保持自研。已落地 `LICENSE`（MIT，SPDX `MIT`）与 `README.md` 许可章节。自研路线不打包 GPL 内核，故上表中的 GPL 分发义务不适用于本仓库；但下列依赖与数据的许可仍需单独遵守：

| 来源 | 实际许可 | 要求 |
|---|---|---|
| `v2node-proto`（`v2node-rust` tag `v0.2.0`） | **MPL-2.0**（已核对其 LICENSE） | 文件级弱 copyleft；可与 MIT 共存，但分发时须附 MPL-2.0 文本与源码获取说明 |
| GEOIP 主源 `misakaio/chnroutes2` | CC-BY-SA 2.0 | 署名 + 相同方式共享，或改用 APNIC 源 |
| `wintun.dll`（C1 后引入） | WireGuard LLC 预编译二进制许可 | 随包保留声明文本 |

发布前需跑一次依赖许可扫描（如 `cargo deny`）确认其余 crate。**本节是工程记录，不是法律意见**。

---

## 4. 内核路线决定（已定：自研 R2）

| 路线 | 优势 | 代价 |
|---|---|---|
| Mihomo fork + sidecar（R1） | 面板已输出 ClashMeta YAML 且 `encryption` 只下发给 Clash 系；Clash API 降低 GUI 成本；YAML 稳定 | 双工具链、SOCKS5 一跳、GPL 分发义务 |
| sing-box | 资源更省（约 32 MB 量级）、route 分层现代 | JSON 跨版本破坏性变更、Windows PPPoE 下 TUN 接管问题、订阅需 Sub-Store 转换 |
| Rust 自研内核（R2） | 单一工具链、无 SOCKS5 一跳、无本机 IPC、无 GPL 分发义务 | TUN / DNS / 规则 / 订阅全部自建；N3 工作量最大 |

**决议（2026-11-12）**：采用 **Rust 自研内核 R2**，见 ADR 0001（C0d 的 UDP/DNS 负面证据为决定性依据）；2026-11-12 用户评审明确「路线依然保持自研」。上表其余路线仅作历史对比保留。范围仍锁定 **N0–N2 先行，N3 长期目标**。

---

## 5. C0c 加密灰度方案（用户已批准，2026-11-11 已执行）

依据 `docs/vless-encryption-prod.md` §6，步骤固定为：**新增独立节点 / 端口 / 订阅，不动现有 40 条**。

1. 新建 `v2_server_v2node` 行：新 node id、新 `server_port`（如 10005）、新域名/订阅入口。
2. 启用 `mlkem768x25519plus`，确认 `/api/v2/server/config` 下发 `encryption` / `encryption_settings`。
3. 只给一个测试用户/分组开放新节点，生成独立 Clash 系订阅。
4. 采集：TCP+UDP 连通、流量上报/在线 IP/限速、握手耗时与 CPU（<NODE_HOST> 为 1 vCPU）、回滚演练。
5. 回滚：关闭新节点 encryption 或删除新节点/订阅，现有 40 条不受影响。

**批准与执行记录**：用户于 2026-11-11 20:20 明确批准「其他操作和变更」，`docs/vless-encryption-prod.md` §8 的前置条件已满足。但**执行仍未发生**：本机 SSH 只有 `<NODE_HOST>`（节点机 `<NODE_HOSTNAME>`）与 `ai-server`，没有面板管理端访问；新增 `v2_server_v2node` 行、独立订阅与单用户分组必须在面板侧完成。执行所需的最小输入见 §10。

---

## 6. C0d Windows 链路 PoC 规格

- **最小范围**：单节点单订阅（Clash YAML）、一个加密入站、**先系统代理**（绕开 wintun 与管理员权限）、sidecar 单实例、连接/断开/状态、本地环形日志、崩溃自愈。
- **不做**：规则编辑 UI、自有 `v2node://`、自动更新、代码签名、遥测、多节点混合、完整 fake-ip 策略、Android 复用层。
- **5 条通过线（任一不过即停）**：TCP+UDP 通；≥1 MiB 多记录传输覆盖 8192 字节分片；域名经 SOCKS5 不丢（ATYP=DOMAINNAME）；DNS 无泄漏；`kill sidecar` 后内核存活并自动重连。
- **24 h 长稳不属于本节**：2026-11-12 起移入 FA 最终验收，在二进制冻结后执行；G1 评审不等待该数据。理由与规格见 `docs/client-design-plan.md` §5「FA 最终验收规格」。
- **前置**：C0c 产出的加密节点；内核二进制 pin 版本并记录 SHA-256。

---

## 7. Go 对照组规格

按 `docs/benchmark-spec.md` §4：同一物理机 / OS、固定 CPU 亲和、同一规则集、同一输入数据、同一并发模型；固定 `GOMAXPROCS`、记录 `GOGC`；Rust 侧 `--release` 且 `lto` / `codegen-units` 与发布配置一致；每组 ≥5 轮报中位数与 P99；记录 commit、编译器版本、flags、CPU、内存、时间戳与复现命令。执行 W1–W10 的同一套 workload。

---

## 8. Go / No-Go 建议

**Go —— 2026-11-12 用户评审决议，C1 可以启动。** 判定依据与附带的工程约束如下。

### 8.1 三项决策输入（用户提供）

| # | 输入 | 决议 |
|---|---|---|
| 1 | 项目许可 | **MIT 开源**；`LICENSE` 已落地，依赖许可见 §3.1 |
| 2 | 内核路线 | **保持自研 R2**；ADR 0001 追认，§4 已锁定 |
| 3 | 用户投诉分布 | 主要集中在 **Clash Verge**：测试显示节点不可用，但节点实际可用；换用 NekoBox 等其他客户端正常；**网络测试与多节点负载均衡功能异常** |

### 8.2 立项信号判定

`client-design-plan.md` §5 的判定规则为「现成客户端无法覆盖加密能力，或投诉集中在客户端侧，或出现必须两端配合的新能力」。第 3 项直接命中 **「投诉集中在客户端侧」**：问题随客户端切换而消失（Clash Verge 异常 / NekoBox 正常），指向客户端实现而非节点侧；且「网络测试、多节点负载均衡」正是本机 `url-test` / `fallback` 健康检查与节点选择能力要覆盖的范围。C0d 5/5、C0a/C0b/C0c/C0e 均已收口，24 h 归 FA 不阻塞 G1，故判 **Go**。

### 8.3 Go 附带的工程约束（不阻塞启动，须在 C1 内关闭）

1. **投诉结论为定性描述**：无工单占比与时间窗。C1 需补一份量化分布（按客户端、问题类型、频次），作为 C2 内测的验收输入。
2. **先做问题归因**：「测试不可用但节点可用、换客户端正常」也可能源于订阅输出格式或延迟探测语义差异，未必需要重写内核。C1 首版必须先复现并定位根因，避免用自研去解一个配置生成问题。
3. **依赖许可复核**：`v2node-proto` 为 MPL-2.0、GEOIP 主源为 CC-BY-SA 2.0；发布前跑 `cargo deny` 并补齐声明（§3.1）。
4. **把投诉诉求转为功能优先级**：节点延迟探测（网络测试）与多节点负载均衡应列为 **C1/C2 一等公民功能**，因为它们是本次立项的直接动因。

### 8.4 历史 No-Go 清单（已关闭）

- ~~C0a 版本分布~~ ✅；~~C0b Linux E2E / Xray 互通~~ ✅（第二步角色解耦转并行项）；
- ~~C0c 加密灰度~~ ✅ 已上线生产（回滚演练转 FA 同跑项）；~~C0d 五条通过线~~ ✅ 5/5；~~24 h~~ 已移入 FA；
- ~~许可评审~~ ✅ MIT；~~内核路线追认~~ ✅ 自研 R2；~~投诉分布~~ ✅ 已提供；
- Go 对照组 / W1 复核、C0b 第二步按 §10 作为不依赖立项的并行投入继续推进，不作为 C1 启动前置。

**C0a 历史影响（保留）**：Clash 系占 76.6% 且原生支持 VLESS Encryption，交接文档 §2.2「覆盖率被客户端卡住」前提不成立；sing-box 仅 0.2%；移动端 48.9% 与桌面端相当，明确 Windows 仅 5.2%。这与本次决议**不矛盾**：C0a 说明「加密覆盖率」不是瓶颈，本次投诉数据说明「体验与功能异常」集中在客户端侧，两者分别否定一个旧前提、支撑一个新信号。

在此期间应继续推进 **C0b 第二步（角色解耦）**、Linux 全量 E2E、Go 对照组与 W1 偏差复核。这些投入不依赖立项决定，即使最终不做客户端也不白做。

### G1 时序与分支动作（2026-11-12 重定）

- **24 h 长稳不再是 G1 输入**：它是 FA 项，必须在 C0–C2 功能冻结后执行；提前跑会因二进制变更而失效。因此 G1 评审无需等待长稳，也不必用 Conditional Go 规避时序冲突。
- **Go**：启动 C1（UI 技术栈决策、IPC 扩展、安装器），FA 仍排在所有功能完成后。**2026-11-12 评审已选此分支，见 §8。**
- **Conditional Go**：仅补齐指定输入（许可 / 投诉分布 / Go 对照组）后复审，不启动 C1 编码。
- **No-Go**：停在 PoC，归档 `client-core` / `client-sidecar` 为可复用资产（proto 抽 crate、性能基线、双进程与回归套件回用节点侧），停止 UI / 安装器投入；FA 仅在后续决定发布时执行。

---

## 9. 本轮新增交付物

| 文件 | 内容 |
|---|---|
| `crates/v2node-proto/` | 抽出的协议 crate（Cargo.toml + lib.rs + encryption.rs） |
| `script/collect_client_ua.sh` | C0a 只读采集脚本，已验证 + 已执行（多文件解析修正、脱敏确认） |
| `docs/benchmark-results/2026-11-11-c0a-client-distribution.md` | C0a 采集结果、平台分布与对 G1 门控解读 |
| `docs/benchmark-results/2026-11-11-c0b-after.md` | C0b 第一步验收与基准 before/after |
| `docs/benchmark-results/2026-11-11-c0b-linux-e2e.md` | C0b 在构建机权威 Linux 的全量 E2E + Xray 26.11.9 互通验证 |
| `docs/g1-gate-review.md` | 本文件 |
| `Cargo.toml` / `Cargo.lock` / `Makefile` / `.github/workflows/rust.yml` | workspace 化与门禁范围扩展 |

**未改动**：生产节点配置、面板文件、订阅数据；`vless.rs` / `tls.rs` 的节点业务逻辑保持原样。

---

## 10. 用户批准记录、概念澄清与仍需输入（2026-11-11 20:20）

### 10.1 已批准

用户明确表示「其他的我批准你进行操作和变更」，涵盖 C0c 加密灰度生产变更、C0d PoC、Go 对照组、Linux E2E、许可评审推进与内核路线相关工作。**批准记录在此留档**，`docs/vless-encryption-prod.md` §8 的「未经确认不得修改生产节点」前置条件已满足。

**但批准 ≠ 已执行**（⚠️ 2026-11-12 更正：此句已过时——C0c 已实际变更生产配置与面板数据，见第 1 节 C0c 行）。决策类事项（许可评审结论、内核路线拍板）仍建议由用户 / 法务确认，AI 已备好对比与清单。

### 10.2 「面板机只读访问 + access log 路径」是什么

| 词 | 含义 | 最低成本形式 |
|---|---|---|
| 面板机 | 托管 `www.example.com` / `api.example.com` 的那台服务器，根目录 `<PANEL_WEB_ROOT>`。**不是** `<NODE_HOST>`（那是节点机 `<NODE_HOSTNAME>`） | 不需要给我写权限或数据库权限 |
| 只读访问 | 能读取该机 nginx access log 的权限 | **你在面板机执行 `script/collect_client_ua.sh` 后把聚合输出贴回即可，我完全不需要 SSH**；或提供一个只读 SSH 别名 |
| access log 路径 | nginx 为站点记录访问日志的绝对路径 | 已内置自动探测，无需你手工找 |

从 `v2board` 源码与部署记录查到、并于 2026-11-11 20:30 **在面板机只读实测验证**的具体值：

- 生产订阅路径：`/Delta/Force/BigRED`（`docs/dev-logs/phase1-dev-log.md:66`）；源码默认值才是 `/api/v1/client/subscribe`（`app/Utils/Helper.php:109`）。
- 面板根目录：`<PANEL_WEB_ROOT>`。
- access log 路径族：`<PANEL_ACCESS_LOG_DIR>/{www,example,pay}.log`（`docs/tasks/cf-custom-rule-merge-20261103.md:112`）。

**已只读实测**：`<NODE_HOST>`（<NODE_PUBLIC_IP>，hostname `<NODE_HOSTNAME>`）上 `/www/wwwlogs` 与 `/www/wwwroot` 均不存在，确认它是纯节点机。用户随后提供了面板机只读 SSH：`root@<PANEL_IP>`（端口 2026，密钥 `<SSH_KEY_PATH>`），已在面板机实测：hostname `<PANEL_HOSTNAME>`、`/www/wwwlogs/` 存在、`<PANEL_ACCESS_LOG>` 订阅命中 1275 条、`<PANEL_ACCESS_LOG>` / `<PANEL_ACCESS_LOG>` 订阅命中 0。本机 `~/.ssh/config` 原只有 `<NODE_HOST>` 与 `ai-server`，面板机以内联参数访问（建议补一条 `Host panel` 便于后续 C0c 操作）。

同时核查了 C0c 的起点（只读）：该机 `v2node-rust` 为 `active`，仅监听 `127.0.0.1:10001`（Send-Q 4096），配置文件中 `encryption` 字段出现 **0 次** —— 与「生产节点加密全空、灰度须新增独立端口」一致，`10005` 尚未被占用。

`script/collect_client_ua.sh` 已把上述值固化为默认值并加 `--help`、stdin 支持与自动探测；用模拟日志验证分类正确、脱敏生效（订阅 URL 中的 `token=` 零泄漏）。

### 10.3 执行仍缺的最小输入

| 项 | 具体需要什么 | 我能做什么 |
|---|---|---|
| C0a 采集 | ~~面板机只读 SSH 别名，或你在面板机跑脚本后贴回聚合输出~~ ✅ 已完成；面板机 `root@<PANEL_IP>:2026`（密钥 `<SSH_KEY_PATH>`） | 已在面板机只读执行，聚合结果见 C0a 归档文档 |
| C0c 灰度 | 面板管理端 / 数据库写权限（或用户提供测试订阅）；新 node id、`server_port`（如 10005）、单测试用户；变更与回滚窗口 | 已具备 <NODE_HOST> 节点 SSH + 面板机 SSH；已产出步骤与回滚方案；面板侧新增测试节点+用户可由我经 artisan/DB 执行（先只读核 schema），或用户后台手动创建 |
| C0d PoC | C0c 的加密节点 + 内核二进制（pin 版本 + SHA-256） | 规格与 5 条通过线已定义（§6）；24 h 归入 FA |
| Linux E2E / Go 对照组 | `<BUILD_HOST_IP>` 构建机访问（已具备）；Go 工具链（待装） | Linux E2E 已完成；Go 对照组待装 Go 后执行 |
| 决策类 | 许可评审结论（法务）、内核路线拍板 | 已产出清单与对比，建议由用户确认 |

### 10.4 最省事的下一步

在面板机执行一条命令（`bash collect_client_ua.sh` 无参数即自动探测），把**聚合输出**贴回；同时在面板管理端确认是否允许新增一个独立加密节点并给出测试用户。这两项到位后，C0a 与 C0c 即可连续推进，G1 门控才有实质输入。
