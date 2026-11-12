# 与 meow-rs v0.29.0 的实机对比报告（2026-11-12，Release 对等版 v2）

> **结论摘要**：本版两侧均为 **Release 优化构建**，并改为「双内核同时运行 + 请求交错 + 两轮反转顺序」采样，替换了 v1 的 Debug／顺序采样数据。结果显示：冷启动、空闲 RSS/CPU、小请求延迟、1 MiB 吞吐**基本同档**；v1 中「meow-rs 大块传输快 2.7 倍」「本项目 p99 更稳 4.7 倍」两条论断**均不成立**，主要由 Debug 构建与顺序采样偏差造成。5 MiB 吞吐名义上 meow-rs 高约 21%，但样本内波动 182–904 MiB/s，**不足以支撑性能结论**。两者功能面相差一个数量级，本报告仍**不构成选型判断**；真实节点加密链路未执行。

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
| 二进制 | `target/release/client-core.exe`，**7,665,152 B（7.31 MiB，Release）** |
| SHA-256 | `84e49c536f991fd51a7d68dab7b1661121ab10430aa78253cf4fe56e78e354d4` |
| 构建命令 | `CARGO_INCREMENTAL=0 cargo build --release --workspace -j 24`（本机，RC=0） |
| 伴随产物 | `target/release/sidecar.exe` 4.9 MiB，SHA-256 `4cd528fe724fda442bee0ea2699bccbeb67a51830de4cb427f6dc097c302ae78`（未参与本轮 workload） |
| 控制面探针 | `target/release/examples/ipc_probe.exe`（Release，2.0 MiB），本轮也已换成 Release |
| 质量门禁 | `fmt --check` / clippy `-D warnings` / `test --workspace` 全绿，58 tests |
| 定位 | C0 / PoC 级自研数据面：SOCKS5 → 规则分流 → VLESS/WS/TLS/mlkem 出站 |
| License | MIT（2026-11-12 G1 评审决议） |
| 当前提交 | `9423d8c`（测试时工作区干净） |

两轮采集的二进制 SHA-256 完全一致，确认被测对象在本轮内未变化。

---

## 2. 测试环境与方法（v2）

| 项 | 配置 |
|---|---|
| 系统 | Windows（win32），本机单台 |
| 目标服务 | Python `http.server` 绑定 `127.0.0.1:17899`，固定内容 `ping.txt`(4 B)、`1m.bin`(1 MiB)、`5m.bin`(5 MiB) |
| 规则集 | 双方均为 `MATCH,DIRECT`，无代理出站 |
| 入站 | meow-rs `mixed-port: 17890`；client-core `--listen 127.0.0.1:17891`（SOCKS5） |
| 控制面 | meow-rs `external-controller: 127.0.0.1:19090`；client-core `--ipc-pipe \\.\pipe\mcloud-s10r` |
| 客户端 | `curl 8.14.1 (Windows) libcurl/8.14.1 Schannel`，`--socks5 <port>`，落盘字节数计量 |
| 构建形态 | **两侧均 Release**（本项目 `-j 24` 优化构建 / meow 官方 release） |
| 采样方法 | 两个内核**同时运行**，请求在两者间交替发出；每个 run 含 **2 个 pass**，第二 pass 反转目标顺序以消除固定顺序偏差 |
| 单 run 样本量 | 每内核：小请求 40 次、1 MiB 8 次、5 MiB 8 次 |
| 独立 run | **2 次**（RunA / RunB），合并统计：小请求 n=80、1 MiB / 5 MiB 各 n=16 |
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

原始数据：`results_release_runA.json`、`results_release_runB.json`、`s10r_stdout_runA.log`、`s10r_stdout_runB.log`（含全部逐次请求记录）。

---

## 3. 实测数据（v2 主数据：两轮合并）

| 指标 | meow-rs v0.29.0 (Release) | client-core (Release) | 判断 |
|---|---|---|---|
| 主二进制体积 | 15.30 MiB | **7.31 MiB** | 本项目小 52%，但功能面差一个数量级，**不能据此判优** |
| 附带依赖 | + `wintun.dll` 0.41 MiB | 无 | meow 随包分发驱动 |
| 冷启动到端口可连（4 次中位数） | 654.1 ms | 651.5 ms | **同档**（meow 有一次 500.4 ms 离群，其余 651–657 ms） |
| 空闲 RSS（两轮均值） | **15.96 MiB** | 16.19 MiB | 差 **1.4%**，基本同档 |
| 空闲 CPU（20 s） | 0.0000% | 0.0000% | 均低于 `GetProcessTimes` 采样分辨率 |
| 小请求 p50（n=80） | 2.31 ms | 2.31 ms | **完全相同** |
| 小请求 p99（n=80） | **12.53 ms** | 12.70 ms | 同档 |
| 1 MiB 延迟 p50 / p99 | 3.44 / 12.42 ms | **3.13 / 3.53 ms** | 本项目尾部更集中 |
| 1 MiB 吞吐中位数 | 290.50 MiB/s | **319.09 MiB/s** | 本项目高约 10%，处于噪声量级 |
| 5 MiB 延迟 p50 / p99 | **12.67 / 18.05 ms** | 15.32 / 27.36 ms | meow 略优 |
| 5 MiB 吞吐中位数 | **396.36 MiB/s** | 326.78 MiB/s | meow 高约 21%，但见 §3.2 |
| 5 MiB 吞吐样本范围 | 193.92 – 867.15 MiB/s | 181.80 – 903.67 MiB/s | **波动范围完全重叠** |
| 控制接口 | REST `/version` → `200` | 命名管道 `status` → JSON（`rules=1`） | 两者均可用 |
| 请求成功率 | 80/80 + 16/16 + 16/16 | 80/80 + 16/16 + 16/16 | 全部 HTTP 200 |

### 3.1 两轮分列（检验可复现性）

| 指标 | RunA meow / client | RunB meow / client |
|---|---|---|
| 冷启动中位数 (ms) | 578.6 / 651.9 | 654.3 / 651.0 |
| 空闲 RSS (MiB) | 15.94 / 16.16 | 15.97 / 16.21 |
| 小请求 p50 (ms) | 2.33 / 2.39 | 2.29 / 2.27 |
| 小请求 p99 (ms) | 12.37 / 12.70 | 12.40 / 12.35 |
| 1 MiB 吞吐 (MiB/s) | 291.49 / 330.53 | 290.50 / 313.73 |
| 5 MiB 吞吐 (MiB/s) | 365.60 / 313.43 | **603.51 / 373.58** |

冷启动、RSS、小请求延迟、1 MiB 吞吐在两轮间高度稳定；**5 MiB 吞吐两轮差异显著**（meow 365.60 → 603.51），是本机页面缓存与调度噪声的主要来源。

### 3.2 对 5 MiB 差异的判定

合并中位数 meow 高约 21%，但：① 两轮 meow 自身中位数相差 65%；② 单次样本范围 182–904 MiB/s，两者区间完全重叠；③ 5 MiB 文件会被操作系统缓存，回环传输基本测的是内存拷贝与调度。因此**该差异不构成性能结论**，需要 W1–W10 长窗口交错采样才可能收敛。

---

## 4. 与 v1（Debug／顺序采样）的偏差分析

v1 报告（`d42a954`）的数据全部来自 Debug 构建 + 先 meow 后 client 的顺序采样。对照如下：

| 指标 | v1（Debug／顺序） | v2（Release／交错） | 结论 |
|---|---|---|---|
| 空闲 RSS | meow 16.03 / client 19.05 MiB（低 **16%**） | 15.96 / 16.19 MiB（低 **1.4%**） | 旧差异主要是 **Debug 运行时与调试符号开销** |
| 5 MiB 吞吐 | meow 810.50 / client 299.26（**2.7×**） | 396.36 / 326.78（**1.2×**） | 旧差异主要来自 **顺序采样 + 缓存／调度偏差** |
| 小请求 p99 | meow 11.93 / client 2.52（client 优 **4.7×**） | 12.53 / 12.70（同档） | 旧「本项目尾部更稳」**不成立** |
| 主二进制体积 | meow 15.30 / client 13.07 MiB（Debug） | 15.30 / **7.31** MiB | Debug 符号使体积对比失真 |
| 冷启动 | 661.2 / 652.8 ms | 654.1 / 651.5 ms | 两版一致，该指标对构建形态不敏感 |

**方法论结论**：在这个量级的回环微基准里，「构建形态 + 采样顺序」造成的偏差，**大于被测实现本身的差异**。v1 的两条性能论断都被 v2 推翻，因此 v1 数据仅保留为历史记录，不再作为任何结论依据。

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
| 2 | **`vless-encryption` feature 是否启用** | 源码 `crates/meow-config/src/proxy_parser.rs:2037` 与 CHANGELOG 显示 meow-rs **支持** `mlkem768x25519plus`（修正了仅依据 README Features 清单的初步判断），但该能力是编译期 feature；官方 Windows 产物是否启用未验证 | 即便将来做链路对比，也需先用 `-t` 校验加密配置能否通过 |
| 3 | meow-rs 的服务化、崩溃恢复、日志持久化 | 本轮未纳入 workload | §5 相关行仅为文档记载，非实测 |
| 4 | 24 h 长稳 | 按 2026-11-12 流程调整已移出 C0d，归入 FA 最终验收 | 不属本轮范围 |
| 5 | 多节点测速与负载均衡 | 需要多节点配置与真实链路 | 用户投诉的核心诉求（G1 立项动因）未被覆盖 |
| 6 | 双内核同时运行的互扰 | 交错采样让两侧共享同一时段的 CPU/缓存，是消除时间漂移的代价 | 绝对值不代表单进程独占时的性能；只用于**相对**比较 |
| 7 | 功能面不对称 | 见 §5 | 体积、RSS、吞吐量级差异不能推导出选型结论 |

**v1 的第 1 项边界（本项目 Release 构建受阻）已关闭**：`cargo build --release --workspace -j 24` 在本机成功；此前的「`aws-lc-sys` 编不了」判断有误，真实原因是构建进程被反复中断、退出码被管道掩盖、且默认 32 jobs 并行度过高。

---

## 7. 数据可靠性说明

1. **两轮独立 run + 每轮 2 pass 反转顺序**，冷启动、RSS、小请求 p50、1 MiB 吞吐跨轮稳定；p99 与 5 MiB 吞吐波动明显，已在 §3.2 单独判定。
2. **空闲 CPU 全为 0.0000%** 是采样分辨率下限（`GetProcessTimes` 100 ns tick + 20 s 窗口），不代表零开销；FA 规格要求 20 min 窗口、Release 构建、内核/sidecar/UI 三项分别计量。
3. **吞吐由落盘字节数与 `time_total` 计算**。v1 首轮曾因 Windows 原生 curl 的 `-o /dev/null` 返回 `size_download=0` 而失效，v2 沿用落盘计量；控制面探测只取 `http_code`，不受该问题影响。
4. **冷启动的 500.4 ms 离群值**未剔除，但已用 4 次中位数降低其影响；若改为均值，meow 为 616.5 ms、client 为 651.4 ms，结论（同档）不变。
5. 所有数值为**本机单次环境下的相对比较**，非跨机器基准；不同 CPU、Windows 版本与 curl 版本会改变绝对值。

---

## 8. 结论与后续

**结论**：在双方均为 Release 优化构建、且采用交错采样的对等条件下，两者在冷启动、空闲资源、小请求延迟、1 MiB 吞吐上**处于同一档**；5 MiB 吞吐 meow-rs 名义上高约 21%，但波动范围完全重叠，不构成性能差异证据。功能覆盖面相差一个数量级，因此本报告**不支持任何「谁更快 / 谁更适合」的选型判断**。本项目侧的优势不在这些通用微基准，而在已实机验证的双进程隔离、快照回滚、崩溃自愈与事件推送等工程机制（s5–s9，42/42）。

**建议的补齐顺序**：

1. 在用户明确授权使用测试订阅、且第三方加密配置通过 `meow -t` 校验后，补做真实节点链路对比：握手耗时、1 MiB / 5 MiB 下载、DNS 无泄漏、多节点测速与负载均衡。
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
| **v2（本版）** | 本次 | 两侧 Release；双内核并发 + 请求交错 + 两轮反转顺序；**重写 §2/§3/§6/§7/§8 全部数据与结论**，新增 §4 偏差分析；v1 数据降级为历史记录 |
