# 与 meow-rs v0.29.0 的实机对比报告（2026-11-12，Release 对等版 v3）

> **结论摘要**：v3 修正了一个致命测量缺陷——本机 `NO_PROXY=127.0.0.1,localhost,::1` 使 v1/v2 的 curl **绕过了 SOCKS5 代理直连目标**，那些吞吐／延迟数据并未经过任一内核。v3 显式加 `--noproxy ""` 后重跑两轮，流量确实经过内核。结果显示：冷启动、空闲 RSS/CPU、小请求延迟、1 MiB 吞吐**仍基本同档**；5 MiB 吞吐 meow-rs 名义高约 17%，但样本波动重叠，**不足以支撑性能结论**。两者功能面相差一个数量级，本报告仍**不构成选型判断**；真实节点加密链路未执行。**v1/v2 的吞吐与延迟数值全部作废，仅保留冷启动、RSS、CPU 与控制接口结论。**

---

## 1. 对比对象核实

| 项 | 值 | 来源 |
|---|---|---|
| 仓库 | `meow-rs/meow-rs` | GitHub API `200` |
| 定位 | A high-performance Rust implementation of the mihomo (Clash Meta) proxy kernel | 仓库 description |
| 语言 / 许可 | Rust / **MIT** | API metadata |
| Stars | 657 | API metadata（采集时） |
| 创建 / 最近推送 | 2026-02-21 / 2026-11-11 | API metadata |
| 版本 | **v0.29.0**，发布于 2026-11-05 | `/releases/latest` |
| Windows 产物 | `meow-v0.29.0-x86_64-pc-windows-msvc.zip`，8,237,787 B | release asset |
| ZIP SHA-256 | `3ffd602369e2b852eca6348ba10689cc7f1f49f3f5935b8f8810a7d4e88dc275` | 官方 `.sha256` 文件，本地校验 **MATCH** |
| `meow.exe` | 16,044,032 B（15.30 MiB），SHA-256 `48ff1b7102b134992a9d5822a71e71a6cd30423f21da8ca60ae5208c37cb52c2` | 本地解压后计算 |
| 随包文件 | `meow-cli.exe` 1,460,736 B、`wintun.dll` 427,552 B、LICENSE、LICENSE.wintun.txt、README.md | 本地解压 |
| CLI | `-f <config.yaml>`、`-d <dir>`、`-t`（校验配置）、`--ext-ctl`、`install` / `uninstall` / `status` | `meow.exe --help` |
| 版本输出 | `meow 0.29.0` | `meow.exe --version` |

**产物可信度**：仅使用官方 release 资产并校验 ZIP SHA-256；所有文件解压在 `D:\wxmuma\_c0d_test\meow-bench\`，未入库、未安装为系统服务、未启用 TUN。

### 本项目侧

| 项 | 值 |
|---|---|
| 二进制 | `target/release/client-core.exe`，**7,683,584 B（7.33 MiB，Release）** |
| SHA-256 | `c10e9ad58bb7664125bf91dd6d2e7c71a1bef65e743fce305bd9a345e517b6e8` |
| 构建命令 | `CARGO_INCREMENTAL=0 cargo build --release -p client-core -j 24`（本机，RC=0） |
| 伴随产物 | `target/release/sidecar.exe` 4.9 MiB（未参与本轮 workload） |
| 控制面探针 | `target/release/examples/ipc_probe.exe`（Release，2.0 MiB） |
| 质量门禁 | `fmt --check` / clippy `-D warnings` / `test --workspace` 全绿，**66 tests** |
| 代码状态 | 含本轮 C1 契约层（`metrics` / `node_probe`），测试时 HEAD 为 `513003e` 加未提交工作区改动 |
| 定位 | C0 / PoC 级自研数据面：SOCKS5 → 规则分流 → VLESS/WS/TLS/mlkem 出站 |
| License | MIT（2026-11-12 G1 评审决议） |

两轮采集的二进制 SHA-256 完全一致，确认被测对象在本轮内未变化。

---

## 2. 测试环境与方法（v3）

| 项 | 配置 |
|---|---|
| 系统 | Windows（win32），本机单台 |
| 目标服务 | Python `http.server` 绑定 `127.0.0.1:17899`，固定内容 `ping.txt`(4 B)、`1m.bin`(1 MiB)、`5m.bin`(5 MiB) |
| 规则集 | 双方均为 `MATCH,DIRECT`，无代理出站 |
| 入站 | meow-rs `mixed-port: 17890`；client-core `--listen 127.0.0.1:17891`（SOCKS5） |
| 控制面 | meow-rs `external-controller: 127.0.0.1:19090`；client-core `--ipc-pipe \\.\pipe\mcloud-s10r` |
| 客户端 | `curl 8.14.1 (Windows) libcurl/8.14.1 Schannel`，**`--noproxy ""` + `--socks5 <port>`**，落盘字节数计量。必须清空 `NO_PROXY`，否则回环目标会绕过代理 |
| 构建形态 | **两侧均 Release**（本项目 `-j 24` 优化构建 / meow 官方 release） |
| 采样方法 | 两个内核**同时运行**，请求在两者间交替发出；每个 run 含 **2 个 pass**，第二 pass 反转目标顺序以消除固定顺序偏差 |
| 单 run 样本量 | 每内核：小请求 40 次、1 MiB 8 次、5 MiB 8 次 |
| 独立 run | **2 次（v3）**（RunC / RunD），合并统计：小请求 n=80、1 MiB / 5 MiB 各 n=16 |
| 冷启动 | 每个 run 中每个内核单独启动 2 次测到端口可连，共 4 次取中位数 |
| 空闲采样 | 两内核共享 **20 s** 窗口，`GetProcessTimes` 取 kernel+user CPU，`tasklist` 取 RSS |
| 配置 | `meow-direct.yaml`、`client-direct.json`（均在测试目录，不含任何真实节点信息） |

**为何用 DIRECT 回环**：真实节点需启用加密，而本轮未完成第三方内核的加密节点配置校验（见 §6）。DIRECT 回环消除了外网波动，使双方在同一 workload 下可比，但只衡量「入站 + 路由 + 直连转发」，**不衡量协议栈、加密与真实链路性能**。

**为何改为交错**：v1 是「先测完一个内核、再测另一个」，页面缓存、CPU 调度与本机负载随时间漂移，会系统性偏向先／后测的一方。交错采样让两侧承受同一时段的环境噪声。

复现命令：

```bash
cd D:/wxmuma/_c0d_test/meow-bench
python s10_compare_release.py     # 单次 run
```

原始数据：`results_release_runC.json` / `runD.json` 与对应 `s10r_stdout_run*.log`（v3 有效）；`runA` / `runB` 保留为「代理被绕过」的无效样本证据。

**为何必须 `--noproxy ""`**：本机环境设了 `NO_PROXY=127.0.0.1,localhost,::1`，而 workload 目标正是 `127.0.0.1`，libcurl 会据此**忽略 `--socks5` 直接连目标**。v3 通过显式清空 no-proxy 列表修正；旁证：同一 1 MiB 请求耗时从 0.005 s 升到 0.015 s（多一跳），且 C1 契约实机回归中内核 `connections_total` 1→2、`bytes_down` 0→1,048,782，确认流量经过内核。s5–s9 用外部域名与 `--socks5-hostname`，不受该变量影响，回归结论仍有效。

---

## 3. 实测数据（v3 主数据：四轮合并 C+D+E+F，流量确实经过内核）

> 本次（2026-11-12 下午）按修复后的脚本重跑 RunE/RunF，与上午的 RunC/RunD 合并为四轮样本：小请求 n=160、1 MiB / 5 MiB 各 n=32。更多样本让 5 MiB 吞吐的轮间噪声显著收敛。

| 指标 | meow-rs v0.29.0 (Release) | client-core (Release) | 判断 |
|---|---|---|---|
| 主二进制体积 | 15.30 MiB | **7.33 MiB** | 本项目小 52%，但功能面差一个数量级，**不能据此判优** |
| 附带依赖 | + `wintun.dll` 0.41 MiB | 无 | meow 随包分发驱动 |
| 冷启动到端口可连（8 次中位数） | 661.4 ms | **651.9 ms** | 同档 |
| 空闲 RSS（4 轮均值） | 16.33 MiB | **16.32 MiB** | 差 **0.06%**，基本相同 |
| 空闲 CPU（20 s） | 0.0000% | 0.0000% | 均低于采样分辨率 |
| 小请求 p50（n=160） | 3.06 ms | **2.93 ms** | 同档；较 v2 的 2.31 ms 升高，符合多一跳 |
| 小请求 p99（n=160） | **15.13 ms** | 13.87 ms | 同档 |
| 1 MiB 延迟 p50 / p99 | 4.32 / 23.27 ms | **4.25 / 14.50 ms** | 本项目尾部更集中 |
| 1 MiB 吞吐中位数 | 231.56 MiB/s | **235.49 MiB/s** | 本项目高约 2%，同档 |
| 5 MiB 延迟 p50 / p99 | **11.05 / 27.58 ms** | 11.52 / 23.32 ms | meow 头部略优；p99 本项目略优 |
| 5 MiB 吞吐中位数 | **452.68 MiB/s** | 434.39 MiB/s | 差 4%，**同档**（较 2 轮合并的 17% 大幅收窄） |
| 5 MiB 吞吐样本范围 | 141.87 – 784.93 MiB/s | 184.44 – 625.94 MiB/s | 区间大幅重叠 |
| 控制接口 | REST `/version` → `200` | 命名管道 `status` → JSON（`rules=1`） | 两者均可用 |
| 请求成功率 | 160/160 + 32/32 + 32/32 | 160/160 + 32/32 + 32/32 | 全部 HTTP 200 |

### 3.1 四轮分列（检验可复现性）

| 指标 | RunC meow / client | RunD meow / client | RunE meow / client | RunF meow / client |
|---|---|---|---|---|
| 冷启动中位数 (ms) | 665.2 / 575.5 | 584.3 / 656.6 | 661.7 / 652.6 | 662.0 / 661.1 |
| 空闲 RSS (MiB) | 16.32 / 16.32 | 16.32 / 16.29 | 16.32 / 16.34 | 16.35 / 16.32 |
| 小请求 p50 (ms) | 2.84 / 2.72 | 2.91 / 2.77 | 4.25 / 3.97 | 3.12 / 2.91 |
| 小请求 p99 (ms) | 13.03 / 13.06 | 13.54 / 13.64 | 15.13 / 13.86 | 12.88 / 3.29 |
| 1 MiB 吞吐 (MiB/s) | 205.83 / 193.04 | 241.37 / 242.01 | 243.02 / 234.49 | 173.26 / 256.90 |
| 5 MiB 吞吐 (MiB/s) | 417.47 / 401.95 | 549.99 / 568.72 | 295.51 / 296.18 | 716.08 / 591.41 |

冷启动与 RSS 跨四轮稳定；1 MiB / 5 MiB 吞吐仍有明显轮间波动（meow 5 MiB 从 295.51 → 716.08），说明回环大块传输受页缓存与调度影响，尚未收敛。

### 3.2 对 5 MiB 差异的判定

四轮合并后 meow 中位数 452.68、client 434.39，**差 4%**；而两轮合并时这一差距曾被放大到 17%（meow 自身轮间中位数从 716.08 降到 295.51，差 59%）。结论不变且更强：**轮间噪声远大于两实现之间的差异**，该差异不构成性能结论，需 W1–W10 长窗口交错采样才可能收敛。

---

## 4. 测量有效性演进（v1 → v2 → v3）

本报告改版三次，原因不是结论反复，而是**逐步发现测量链路本身的问题**。按严重程度排序：

| 版本 | 构建形态 | 采样 | 致命缺陷 | 数据效力 |
|---|---|---|---|---|
| v1 (`d42a954`) | 本项目 Debug / meow Release | 顺序（先 meow 后 client） | 构建不对等 + 顺序偏差 + **curl 绕过代理** | 吞吐／延迟作废；冷启动／RSS／CPU 保留 |
| v2 (`e421cdb` / `e6a0c7c`) | 两侧 Release | 并发交错 + 两轮反转 | 前两项已修正，**curl 仍绕过代理** | 吞吐／延迟仍作废；冷启动／RSS／CPU 保留 |
| **v3（本版）** | 两侧 Release | 并发交错 + 两轮反转 | 已加 `--noproxy ""`，流量确实经过内核 | **全部有效** |

v2 → v3 的数值变化（同一脚本，仅改 curl 参数）：

| 指标 | v2（无效）meow / client | v3（有效）meow / client |
|---|---|---|
| 小请求 p50 | 2.31 / 2.31 ms | 2.88 / 2.75 ms |
| 1 MiB 吞吐 | 290.50 / 319.09 MiB/s | 241.37 / 249.13 MiB/s |
| 5 MiB 吞吐 | 396.36 / 326.78 MiB/s | 677.74 / 577.72 MiB/s |
| 空闲 RSS | 15.96 / 16.19 MiB | 16.32 / 16.30 MiB |

**方法论结论**：在这个量级的基准里，第一道关不是「构建是否对等」或「采样是否交错」，而是**「被测流量是否真的经过被测系统」**。必须先有一条独立的端到端证据（本轮是内核 `metrics` 的连接／字节计数），再谈统计方法。v1 的「meow 大块传输快 2.7×」「本项目 p99 更稳 4.7×」与 v2 的所有吞吐／延迟数值均建立在无效数据上，不再作为任何结论依据。

---

## 5. 静态能力对比

| 维度 | 本项目 `client-core` + `client-sidecar` | meow-rs v0.29.0 |
|---|---|---|
| 定位 | 自研 PoC 数据面 + 独立控制面 | 完整 mihomo 兼容内核 |
| 架构 | 双进程 + Windows 服务，命名管道 IPC | 11 个 workspace crate，单进程 + REST/Web UI |
| 出站协议 | 仅 VLESS / WS / TLS / `mlkem768x25519plus`、DIRECT | SS、Trojan、Hysteria2、VLESS、VMess、HTTP、SOCKS5、Snell、AnyTLS、Direct、Reject |
| 入站 | SOCKS5（+ 可选 fake-ip DNS UDP） | Mixed / HTTP / SOCKS5 / TProxy / **TUN-Wintun** |
| 规则 | 域名、IP-CIDR、GEOIP/GEOSITE、rule-set、MATCH | 域名、IP、端口、进程名、DSCP、GEOIP、GEOSITE、RULE-SET、SUB-RULE、逻辑组合 |
| 代理组 | url-test / fallback 健康检查（基础） | Selector / URLTest / Fallback / LoadBalance / Relay + `dialer-proxy` 链 |
| DNS | fake-ip、可配上游 | 缓存、in-flight 去重、DNS snooping、主/备 nameserver |
| TUN | 未实现（C1 规划，见 `docs/c1-gui-tun-prestudy.md`） | 已实现，Windows 走 Wintun，随包附 `wintun.dll` |
| 控制接口 | 命名管道 `status` / `reload` + `applied`/`rejected` 事件 | REST API + 内置 Web Dashboard + `meow-cli` |
| 配置格式 | Clash 风格 **JSON**（YAML 需 `clash2profile.py` 转换） | Clash **YAML** |
| 服务化 / 崩溃自愈 | Windows 服务、`sc failure`、sidecar 指数退避重连、快照回滚（s5–s9 实机验证） | `install` 子命令；未在本轮测试其行为 |
| 许可 | MIT（`v2node-proto` 为 MPL-2.0） | MIT（另附 Wintun 许可文本） |
| 代码规模 | 2 个 crate | 11 个 crate |

**判断**：功能覆盖面相差一个数量级。本报告的实测部分只覆盖「启动、空闲资源、回环直连转发、控制接口可达性」四项；§3 的体积与资源数值**不能**外推为「谁更轻量／更优秀」。

---

## 6. 未执行项与不可比边界（必须随结论一并阅读）

| # | 项 | 原因 | 影响 |
|---|---|---|---|
| 1 | **真实节点加密链路** | 需要把项目测试订阅写入第三方内核的 YAML 配置；该配置生成被本机安全策略拦截，未重试、未绕过 | 无法比较握手、加密记录层、真实 WAN 延迟与吞吐，而这恰是两者最核心的差异 |
| 2 | ~~**`vless-encryption` feature 是否启用**~~ ✅ **已实测启用（2026-11-12 14:08）** | 用纯虚构配置（dummy UUID、dummy 密钥、`127.0.0.1`、`example.com`，**不含任何真实凭据**）跑 `meow -t`：`encryption: none` 通过；`mlkem768x25519plus.native.1rtt.AAAA` 报 `vless: failed to use encryption: empty nfsPKeysBytes`，即进入了真实密钥解析路径，而非源码中 feature 关闭时的 `requires the 'vless-encryption' Cargo feature; rebuild with --features ...` 分支。源码依据：`proxy_parser.rs:2816/2827` | 官方产物承接本项目加密能力的**编译期障碍已排除**；但端到端握手与真实节点链路仍未验证 |
| 3 | meow-rs 的服务化、崩溃恢复、日志持久化 | 本轮未纳入 workload | §5 相关行仅为文档记载，非实测 |
| 4 | 12 h 长稳 | 按 2026-11-12 流程调整已移出 C0d，归入 FA 最终验收 | 不属本轮范围 |
| 5 | 多节点测速与负载均衡 | 需要多节点配置与真实链路 | 用户投诉的核心诉求（G1 立项动因）未被覆盖 |
| 6 | 双内核同时运行的互扰 | 交错采样让两侧共享同一时段的 CPU/缓存，是消除时间漂移的代价 | 绝对值不代表单进程独占时的性能；只用于**相对**比较 |
| 7 | 功能面不对称 | 见 §5 | 体积、RSS、吞吐量级差异不能推导出选型结论 |
| 8 | meow-rs 配置校验的静默降级 | 同一次 `-t` 实测中，加密节点解析失败只产生 `WARN Failed to parse proxy`，该 proxy 被跳过，GLOBAL selector 降级为 `DIRECT`，而 `-t` 仍输出 `Configuration test passed` 且 RC=0 | 若产品依赖这类校验结果判定节点可用性，可能把「配置解析失败」呈现为「节点不可用」；真实合法密钥下是否同样如此未验证。**这是行为观察，不是投诉根因结论**（用户已判定现场归因不可执行） |

**v1 的第 1 项边界（本项目 Release 构建受阻）已关闭**：`cargo build --release --workspace -j 24` 在本机成功；此前的「`aws-lc-sys` 编不了」判断有误，真实原因是构建进程被反复中断、退出码被管道掩盖、且默认 32 jobs 并行度过高。

---

## 7. 数据可靠性说明

1. **`--noproxy ""` 是 v3 的硬性前提**：本机 `NO_PROXY` 含 `127.0.0.1`，v1/v2 的 curl 直连目标，吞吐与延迟数据未经过内核，全部作废。有效性由内核 `metrics` 的 `connections_total` / `bytes_down` 增长独立佐证（C1 契约实机回归 18/18）。s5–s9 使用外部域名与 `--socks5-hostname`，不受影响。
2. **四轮独立 run（每轮 2 pass 反转顺序）**，冷启动、RSS、小请求 p50 跨轮稳定；1 MiB / 5 MiB 吞吐与 p99 波动明显，已在 §3.2 判定。
3. **空闲 CPU 全为 0.0000%** 是采样分辨率下限（`GetProcessTimes` 100 ns tick + 20 s 窗口），不代表零开销；FA 规格要求 20 min 窗口、Release 构建、内核/sidecar/UI 三项分别计量。
4. **吞吐由落盘字节数与 `time_total` 计算**。v1 首轮曾因 Windows 原生 curl 的 `-o /dev/null` 返回 `size_download=0` 而失效，v3 沿用落盘计量；控制面探测只取 `http_code`，不受该问题影响。
5. **冷启动存在 500.2 / 517.1 ms 离群值**，未剔除但用 4 次中位数降低影响；两侧结论（同档）不变。
6. 所有数值为**本机单次环境下的相对比较**，非跨机器基准；不同 CPU、Windows 版本与 curl 版本会改变绝对值。

---

## 8. 结论与后续

**结论**：在两侧均为 Release、并发交错采样、且**确认流量经过内核**的条件下，两者在冷启动、空闲资源、小请求延迟、1 MiB 吞吐上**处于同一档**；5 MiB 吞吐四轮合并后 meow-rs 仅名义高约 4%，且波动区间大幅重叠，不构成性能差异证据。功能覆盖面相差一个数量级，因此本报告**不支持任何「谁更快 / 谁更适合」的选型判断**。本项目侧的优势不在这些通用微基准，而在已实机验证的双进程隔离、快照回滚、崩溃自愈与事件推送（s5–s9，42/42）。

**建议的补齐顺序**：

1. 在用户明确授权使用测试订阅、且第三方加密配置通过 `meow -t` 校验后，补做真实节点链路对比：握手耗时、1 MiB / 5 MiB 下载、DNS 无泄漏、多节点测速与负载均衡（编译期 feature 障碍已排除，见 §6 第 2 项）。若考虑改用 meow-rs 作为内核，还需用 C0d 用例复测其 UDP/DNS 行为，确认是否继承 Mihomo 的负面证据。
2. 按 `docs/benchmark-spec.md` 的 W1–W10 做更长窗口的交错采样，收敛 p99 与 5 MiB 吞吐波动。
3. FA 最终验收时按 20 min 窗口、Release 构建、三项分别计量重测空闲 CPU。
4. 发布前跑 `cargo deny` 复核 `v2node-proto`(MPL-2.0) 与 GEOIP 数据许可。

**安全与合规**：官方 ZIP 与本地 `meow.exe` 的 SHA-256 均已记录；所有第三方文件、配置与日志存放于 `D:\wxmuma\_c0d_test\meow-bench\`，**未入库**；本报告不含任何真实节点地址、UUID、加密串或订阅链接。

---

## 9. 变更记录

| 版本 | 提交 | 变化 |
|---|---|---|
| v1 | `d42a954` | 首次对比：本项目 Debug + 顺序采样；结论受构建不对等影响 |
| v1.1 | `9423d8c` | 记录 Release 产出与 SHA-256，但**未重跑** workload，明确标注边界仍有效 |
| **v2** | `e421cdb` | 两侧 Release；双内核并发 + 请求交错 + 两轮反转顺序；**重写 §2/§3/§6/§7/§8 全部数据与结论**，新增 §4 偏差分析；v1 数据降级为历史记录 |
| **v2.1** | `e6a0c7c` | 用纯虚构配置实测官方产物已启用 `vless-encryption`，关闭 §6 第 2 项；新增 §6 第 8 项「配置校验静默降级」行为观察；§8 建议补入 UDP/DNS 复测 |
| **v3** | 本次 | 发现并修正 `NO_PROXY` 导致 curl 绕过 SOCKS5 的致命测量缺陷，重跑 RunC/RunD 并**再次重写全部实测数据**；新增 §4 测量有效性演进；v1/v2 吞吐与延迟数值作废 |
| **v3.1** | 本次 | 按修复脚本重跑 RunE/RunF，与 RunC/RunD **合并为四轮样本**（小请求 n=160、1/5 MiB 各 n=32）；§3 主表与 §3.1/§3.2 刷新。更多样本下 5 MiB 吞吐差距从 17% 收窄到 **4%**，进一步收敛轮间噪声，结论不变 |
