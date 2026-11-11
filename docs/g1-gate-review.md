# G1 门控评审材料：C1 客户端正式立项前

> 版本：v1.2（2026-11-11 21:10）
> 用途：汇总 C1 立项前所有门控输入，供评审会决策
> **结论：No-Go（当前不建议立项）**。C0a 用户侧版本分布已于 2026-11-11 20:30 采集完成（Clash 系 76.6%、sing-box 0.2%、移动端 48.9%）；**C0b 重构的 Linux E2E 与 Xray 26.11.9 互通已于 2026-11-11 21:10 在构建机全过**。但 C0c 加密灰度（生产部署与回滚演练）、C0d PoC、许可评审与内核路线决定仍未收口，C1 启动前置未齐。
>
> **⚠️ 2026-11-12 状态更正（以此为准，详见 `docs/HANDOVER.md`）**：C0c 加密灰度**已上线生产**——2026-11-11 17:14 起 <NODE_HOST>（NodeID 10）启用 `mlkem768x25519plus`，面板 id 10–19 全量配置，本机探针 `PROBE_OK` 6 ms、经 Cloudflare 500 ms；客户端侧 `crates/client-core` 已实现整条数据面，并于 2026-11-12 01:50 通过构建机 105 tests 与 9 项热重载实机验证。因此下方「待部署 / 待执行」「本轮没有修改任何生产配置」等表述**已被现场推翻**。G1 仍为 No-Go，但剩余前置项已收敛为：C0d Windows 链路 PoC、许可评审结论、内核路线拍板。

---

## 1. 门控清单

| 项 | 要求 | 状态 | 证据 / 阻塞 |
|---|---|---|---|
| C0a 用户侧客户端版本分布 | 目标用户 Mihomo / sing-box / Xray / 其他占比、Windows 占比、最低支持版本 | **已完成（2026-11-11 20:30）** | 数据见 `docs/benchmark-results/2026-11-11-c0a-client-distribution.md`；面板机只读 SSH 已提供（root@<PANEL_IP>:2026），实测数据源 `/www/wwwlogs/<PANEL_ACCESS_LOG>` 订阅命中 **1275 条**（8 天窗口），可识别率 99.4% |
| C0b 提前重构 | 抽 `v2node-proto` crate | **第一步完成 + Linux E2E/Xray 互通已验证 / 第二步待做** | `docs/benchmark-results/2026-11-11-c0b-after.md`、`2026-11-11-c0b-linux-e2e.md`；`make quality` 56 passed/0 failed、E2E 三连全过、Xray 26.11.9 互通完整 |
| C0c 加密节点灰度 | 独立节点/端口/订阅 + 单用户，产出握手耗时、CPU、回滚演练 | **已完成（2026-11-11 17:14 上线，生产在用）** | <NODE_HOST> / NodeID 10 启用 `mlkem768x25519plus`，面板 id 10–19 全量配置；本机 `PROBE_OK` 6 ms、经 Cloudflare 500 ms。**未收口**：约 62% 握手失败来自未刷新订阅的老客户端，处置待定，见 `docs/HANDOVER.md` P0-2 |
| C0d Windows 链路 PoC | 全链路跑通 + 6 条通过线 | **未执行** | 依赖 C0c 的加密节点与内核二进制（需 pin 版本 + SHA-256）；规格见本文 §6 |
| C0e 性能基线 | 三层基准 + 四维指标 | **完成** | `docs/benchmark-results/`（L1/L2/L3/criterion/总报告） |
| Go 内核对照组 | 同机同规则集同负载跑 W1–W10 | **阻塞 → 部分就绪** | 构建机 `<BUILD_HOST_IP>` 现已可达（SSH root + 面板密钥），但 `go` 未安装；需先装 Go + 拉取 Mihomo/sing-box 二进制，约 2–3 人日 |
| 许可评审 | 正式结论 | **待评审** | 事实与待确认项见本文 §3；本文不是法律意见 |
| 内核路线决定 | Mihomo / sing-box / 自研 R2 | **待决定** | 对比见 `docs/client-design-plan.md` §3，决定时点为 G1 |
| 用户投诉分布 | 是否集中在客户端侧卡顿 / 重连 / DNS | **未采集** | 属用户侧运营数据，需用户提供 |

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

**注意**：进程边界只影响「是否构成衍生作品」，**不免除分发 GPL 二进制时的源码义务**。正式发布前需一次法务评审。

---

## 4. 内核路线决定（不替用户决定）

| 路线 | 优势 | 代价 |
|---|---|---|
| Mihomo fork + sidecar（R1） | 面板已输出 ClashMeta YAML 且 `encryption` 只下发给 Clash 系；Clash API 降低 GUI 成本；YAML 稳定 | 双工具链、SOCKS5 一跳、GPL 分发义务 |
| sing-box | 资源更省（约 32 MB 量级）、route 分层现代 | JSON 跨版本破坏性变更、Windows PPPoE 下 TUN 接管问题、订阅需 Sub-Store 转换 |
| Rust 自研内核（R2） | 单一工具链、无 SOCKS5 一跳、无本机 IPC、无 GPL 分发义务 | TUN / DNS / 规则 / 订阅全部自建；N3 工作量最大 |

**当前倾向（非决定）**：Mihomo 更契合现有面板资产。建议 G1 前用 `CoreAdapter` 抽象层在 PoC 中实测两套配置语义，再定路线；范围锁定 **N0–N2 先行，N3 长期目标**。

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
- **6 条通过线（任一不过即停）**：TCP+UDP 通；≥1 MiB 多记录传输覆盖 8192 字节分片；域名经 SOCKS5 不丢（ATYP=DOMAINNAME）；DNS 无泄漏；`kill sidecar` 后内核存活并自动重连；24 h 无内存增长。
- **前置**：C0c 产出的加密节点；内核二进制 pin 版本并记录 SHA-256。

---

## 7. Go 对照组规格

按 `docs/benchmark-spec.md` §4：同一物理机 / OS、固定 CPU 亲和、同一规则集、同一输入数据、同一并发模型；固定 `GOMAXPROCS`、记录 `GOGC`；Rust 侧 `--release` 且 `lto` / `codegen-units` 与发布配置一致；每组 ≥5 轮报中位数与 P99；记录 commit、编译器版本、flags、CPU、内存、时间戳与复现命令。执行 W1–W10 的同一套 workload。

---

## 8. Go / No-Go 建议

**No-Go —— 维持「客户端不立项」。** C0a 已收口（结论见下），但 C1 启动前仍需：

1. ~~C0a 用户侧版本分布~~ ✅ 已完成；
2. ~~C0b Linux E2E 与 Xray 互通~~ ✅ 已完成（2026-11-11，构建机 56 passed/0 failed、E2E 三连全过、Xray 26.11.9 互通）；第二步角色解耦仍待做；
3. C0c 加密灰度数据（**审批已通过、访问已具备**，但生产部署与回滚演练结果尚未产生；仍须面板侧新增测试节点+用户或用户提供订阅）；
4. C0d PoC 六条通过线全过；
5. 许可评审结论与内核路线决定。

**C0a 实证对立项判断的影响**：Clash 系占 76.6% 且原生支持 VLESS Encryption，交接文档 §2.2「覆盖率被客户端卡住」前提不成立；sing-box 仅 0.2%，内核选型强烈偏向 Mihomo 系 / 自研；移动端（iOS+Android）48.9% 与桌面端相当，明确 Windows 仅 5.2%，「Windows 优先」前提需修正。这些使「自研单平台客户端」的立项理由更弱，但用户已决策推进准备工作，C0a 让后续决策更 informed。

在此期间应继续推进 **C0b 第二步（角色解耦）**、Linux 全量 E2E、Go 对照组与 W1 偏差复核。这些投入不依赖立项决定，即使最终不做客户端也不白做。

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
| C0d PoC | C0c 的加密节点 + 内核二进制（pin 版本 + SHA-256） | 规格与 6 条通过线已定义（§6） |
| Linux E2E / Go 对照组 | `<BUILD_HOST_IP>` 构建机访问（已具备）；Go 工具链（待装） | Linux E2E 已完成；Go 对照组待装 Go 后执行 |
| 决策类 | 许可评审结论（法务）、内核路线拍板 | 已产出清单与对比，建议由用户确认 |

### 10.4 最省事的下一步

在面板机执行一条命令（`bash collect_client_ua.sh` 无参数即自动探测），把**聚合输出**贴回；同时在面板管理端确认是否允许新增一个独立加密节点并给出测试用户。这两项到位后，C0a 与 C0c 即可连续推进，G1 门控才有实质输入。
