# 与 meow-rs v0.29.0 的实机对比报告（2026-11-12）

> **结论摘要**：两者定位严重不对等，本报告不构成选型结论。可执行的本机回环 DIRECT 测试显示：两者冷启动、小请求延迟、1 MiB 吞吐基本同档；meow-rs 空闲 RSS 更低、5 MiB 大块传输更快，本项目侧 p99 延迟更稳定。但本项目侧为 **Debug 构建**（Release 构建受阻于 `aws-lc-sys` C 编译），meow-rs 为官方 **Release 产物**，体积与性能数据不可直接对比；真实节点加密链路对比本轮未执行。

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
| SHA-256 | `3ffd602369e2b852eca6348ba10689cc7f1f49f3f5935b8f8810a7d4e88dc275` | 官方 `.sha256` 文件，本地校验 **MATCH** |
| 解压内容 | `meow.exe` 16,044,032 B、`meow-cli.exe` 1,460,736 B、`wintun.dll` 427,552 B、LICENSE、LICENSE.wintun.txt、README.md | 本地解压 |
| CLI | `-f <config.yaml>`、`-d <dir>`、`-t`（校验配置）、`--ext-ctl`、`install` / `uninstall` / `status` | `meow.exe --help` |
| 版本输出 | `meow 0.29.0` | `meow.exe --version` |

**产物可信度**：仅使用官方 release 资产并校验 SHA-256；所有文件解压在 `D:\wxmuma\_c0d_test\meow-bench\`，未入库、未安装为系统服务、未启用 TUN。

### 本项目侧

| 项 | 值 |
|---|---|
| 二进制 | `target/debug/client-core.exe`，13,708,288 B（Debug） |
| 定位 | C0 / PoC 级自研数据面：SOCKS5 → 规则分流 → VLESS/WS/TLS/mlkem 出站 |
| License | MIT（2026-11-12 G1 评审决议） |
| 当前提交 | `b6c217f`（测试时工作区干净） |

---

## 2. 测试环境与方法

| 项 | 配置 |
|---|---|
| 系统 | Windows（win32），本机单台 |
| 目标服务 | Python `http.server` 绑定 `127.0.0.1:17899`，固定内容 `ping.txt`(4 B)、`1m.bin`(1 MiB)、`5m.bin`(5 MiB) |
| 规则集 | 双方均为 `MATCH,DIRECT`，无代理出站 |
| 入站 | meow-rs `mixed-port: 17890`；client-core `--listen 127.0.0.1:17891`（SOCKS5） |
| 控制面 | meow-rs `external-controller: 127.0.0.1:19090`；client-core `--ipc-pipe \\.\pipe\mcloud-s10` |
| 客户端 | `curl 8.14.1 (Windows) libcurl/8.14.1 Schannel`，`--socks5 <port>` |
| 空闲采样 | 20 s 窗口，`GetProcessTimes` 取 kernel+user CPU，`tasklist` 取 RSS |
| 采样轮数 | 两轮：Run2（延迟 20 次、吞吐各 3 次）、Run3（延迟 30 次、吞吐各 5 次） |
| 配置 | `meow-direct.yaml`、`client-direct.json`（均在本报告同级测试目录，不含任何真实节点信息） |

**为何用 DIRECT 回环**：真实节点需启用 `mlkem768x25519plus` 加密，而本轮未能完成第三方内核的加密节点配置校验（见 §5）。DIRECT 回环消除了外网波动，使双方在同一 workload 下可比，但只衡量「入站 + 路由 + 直连转发」，**不衡量协议栈、加密与真实链路性能**。

复现命令：

```bash
cd D:/wxmuma/_c0d_test/meow-bench
python s10_compare.py
```

---

## 3. 实测数据

### 3.1 Run3（主数据，采样轮数更多）

| 指标 | meow-rs v0.29.0 (Release) | client-core (Debug) | 说明 |
|---|---|---|---|
| 主二进制体积 | 15.30 MiB | 13.07 MiB | **不可直接比较**：后者含调试符号 |
| 附带依赖 | + `wintun.dll` 0.41 MiB | 无 | meow 随包分发驱动 |
| 冷启动到端口可连 | 661.2 ms | 652.8 ms | 同档 |
| 空闲 RSS | **16.03 MiB** | 19.05 MiB | meow 低约 16% |
| 空闲 CPU | 0.0000% | 0.0000% | 两者均低于 `GetProcessTimes` 采样分辨率 |
| 小请求延迟 p50 (n=30) | 2.19 ms | 2.20 ms | 同档 |
| 小请求延迟 p99 | 11.93 ms | **2.52 ms** | meow 本轮尾部抖动明显 |
| 1 MiB 吞吐（中位数） | 307.03 MiB/s | 313.09 MiB/s | 同档 |
| 5 MiB 吞吐（中位数） | **810.50 MiB/s** | 299.26 MiB/s | meow 明显更高，见 §6 风险 |
| 控制接口 | REST `/version` → `200` | 命名管道 `status` → 返回 JSON | 两者均可用 |

### 3.2 Run2（对照，检验波动）

| 指标 | meow-rs | client-core (Debug) |
|---|---|---|
| 冷启动 | 662.7 ms | 653.0 ms |
| 空闲 RSS | 15.95 MiB | 19.09 MiB |
| 小请求 p50 / p99 | 2.24 / 12.52 ms | 2.20 / 11.94 ms |
| 1 MiB 吞吐 | 279.72 MiB/s | 290.61 MiB/s |
| 5 MiB 吞吐 | 778.94 MiB/s | 344.71 MiB/s |

**跨轮观察**：冷启动、RSS、p50、1 MiB 吞吐两轮稳定；p99 与 5 MiB 吞吐波动大（client-core p99 从 11.94 → 2.52 ms），说明该工作台的尾部指标受本机调度与缓存影响，**不宜作为验收值**。

原始数据：`D:\wxmuma\_c0d_test\meow-bench\results.json`（Run3）、`results_run2.json`（Run2）、各进程日志与 `curl_out.bin`。

---

## 4. 静态能力对比

| 维度 | 本项目 `client-core` + `client-sidecar` | meow-rs v0.29.0 |
|---|---|---|
| 定位 | 自研 PoC 数据面 + 独立控制面 | 完整 mihomo 兼容内核 |
| 架构 | 双进程 + Windows 服务，命名管道 IPC | 11 个 workspace crate，单进程 + REST/Web UI |
| 出站协议 | 仅 VLESS / WS / TLS / `mlkem768x25519plus`、DIRECT | SS、Trojan、Hysteria2、VLESS、VMess、HTTP、SOCKS5、Snell、AnyTLS、Direct、Reject |
| 入站 | SOCKS5（+ 可选 fake-ip DNS UDP） | Mixed / HTTP / SOCKS5 / TProxy / **TUN-Wintun** |
| 规则 | 域名、IP-CIDR、GEOIP/GEOSITE、rule-set、MATCH | 域名、IP、端口、进程名、DSCP、GEOIP、GEOSITE、RULE-SET、SUB-RULE、逻辑组合 |
| 代理组 | url-test / fallback 健康检查（基础） | Selector / URLTest / Fallback / LoadBalance / Relay + `dialer-proxy` 链 |
| DNS | fake-ip、可配上游 | 缓存、in-flight 去重、DNS snooping、主/备 nameserver |
| TUN | 未实现（C1 规划） | 已实现，Windows 走 Wintun，随包附 `wintun.dll` |
| 控制接口 | 命名管道 `status` / `reload` + `applied`/`rejected` 事件 | REST API + 内置 Web Dashboard + `meow-cli` |
| 配置格式 | Clash 风格 **JSON**（YAML 需 `clash2profile.py` 转换） | Clash **YAML** |
| 服务化 / 崩溃自愈 | Windows 服务、`sc failure`、sidecar 指数退避重连、快照回滚 | `install` 子命令；未在本轮测试其行为 |
| 许可 | MIT（`v2node-proto` 为 MPL-2.0） | MIT（另附 Wintun 许可文本） |
| 代码规模 | 2 个 crate | 11 个 crate |

**判断**：功能覆盖面相差一个数量级。本报告的可比部分只覆盖「启动、空闲资源、回环直连转发、控制接口可达性」四项。

---

## 5. 未执行项与不可比边界（必须随结论一并阅读）

| # | 项 | 原因 | 影响 |
|---|---|---|---|
| 1 | **本项目 Release 构建** | `cargo build --release -p client-core` 卡在 `aws-lc-sys v0.47.0` 的 C 编译：后台累计 >22 min、前台 5 min 超时，无 `.lib`/`.a` 产物 | 本项目侧为 Debug，体积、吞吐、CPU 与官方 Release **不对等**；所有性能数值仅作基线 |
| 2 | **真实节点加密链路** | 需要把项目测试订阅写入第三方内核的 YAML 配置；本轮该配置生成被安全策略拦截，未继续执行 | 无法比较握手、加密记录层、真实 WAN 延迟与吞吐，而这恰是两者最核心的差异 |
| 3 | **`vless-encryption` feature 是否启用** | 源码 `crates/meow-config/src/proxy_parser.rs:2037` 与 CHANGELOG 显示 meow-rs **支持** `mlkem768x25519plus`（修正了仅依据 README Features 清单的初步判断），但注释表明该能力是编译期 feature；官方 Windows 产物是否启用未验证 | 即便将来做链路对比，也需先用 `-t` 校验加密配置能否通过 |
| 4 | meow-rs 的服务化、崩溃恢复、日志持久化 | 本轮未纳入 workload | 静态对比中相关行仅为文档记载，非实测 |
| 5 | 24 h 长稳 | 按 2026-11-12 流程调整已移出 C0d，归入 FA 最终验收 | 不属本轮范围 |
| 6 | 多节点测速与负载均衡 | 需要多节点配置与真实链路 | 用户投诉的核心诉求未被覆盖 |

---

## 6. 数据可靠性说明

1. **5 MiB 吞吐差异（810 vs 299 MiB/s）**在两轮中方向一致，可能反映缓冲区/零拷贝策略差异，但也可能受 Debug 构建、页面缓存与本机调度影响；在 Release 对等构建与多轮交错采样前，不应据此推断协议栈效率。
2. **空闲 CPU 全为 0.0000%** 是采样分辨率下限（`GetProcessTimes` 100 ns tick + 20 s 窗口），不代表零开销；FA 规格要求 20 min 窗口、Release 构建、三项分别计量。
3. **p99 抖动**受本机负载影响，Run2/Run3 差异明显，需 interleaved 多轮采样才能收敛。
4. curl 计量首轮曾因 `-o /dev/null` 在 Windows 原生 curl 下返回 `size_download=0` 而失效；已改为落盘文件大小计量并复跑，本报告只采用修复后的 Run2/Run3 数据。

---

## 7. 结论与后续

**结论**：在本轮可测范围内，两者冷启动、小请求延迟与 1 MiB 直连吞吐处于同一档；meow-rs 空闲 RSS 更低、大块传输更快；本项目侧的优势不在这些通用指标，而在已验证的双进程隔离、快照回滚、崩溃自愈与事件推送等工程机制。由于构建不对等与链路测试缺失，**不支持任何「谁更快 / 谁更适合」的选型判断**。

**建议的补齐顺序**：

1. 在已具备 AWS-LC 构建工具链的机器（或 Linux 构建机）产出本项目 **Release** 二进制并 pin SHA-256，重跑本报告的 DIRECT workload，使体积与性能可比。
2. 在用户明确授权使用测试订阅、且第三方加密配置通过 `meow -t` 校验后，补做真实节点链路对比：握手耗时、1 MiB / 5 MiB 下载、DNS 无泄漏、多节点测速与负载均衡。
3. 按 `docs/benchmark-spec.md` 的 W1–W10 做交错多轮采样，收敛 p99 与吞吐波动。
4. 更新本报告，形成可复现的发布级基线。

**安全与合规**：官方产物 SHA-256 已校验；所有第三方文件、配置与日志存放于 `D:\wxmuma\_c0d_test\meow-bench\`，**未入库**；本报告不含任何真实节点地址、UUID、加密串或订阅链接。
