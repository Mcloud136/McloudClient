# C0d Windows 链路 PoC 实测（2026-11-12）

> 范围（历史口径）：当时 `docs/g1-gate-review.md` §6 定义的 6 条通过线中可自动化验证的部分；§6 现为 5 条。
> 环境：Windows（本机）、Rust 1.99.1、单节点单加密入站、系统代理（未使用 wintun）。
> 结论（当时实测）：**自研数据面 4/6 通过**（TCP+UDP、≥1 MiB、域名 ATYP、DNS 无泄漏），**Mihomo 2/6**（≥1 MiB、域名）；自愈与 12 h 在当时均未验证。**现状**：自愈已由 s5 8/8、s9 11/11 补齐，12 h 已移入 FA 最终验收。
>
> **⚠️ 2026-11-12 流程调整（当前状态以此为准）**：12 h 长稳已从 C0d 通过线移出，改列「FA 最终验收」（所有功能冻结后执行）；C0d 通过线由 6 条改为 5 条。`kill sidecar` 自愈已由 `s5_probe.py` 8/8 与 `s9_service_probe.py` 11/11 补齐，故当前 **C0d 5/5 全通过**。下方表格与结论保留当时的历史实测数据，不代表当前流程口径。

## 1. 被测对象与版本锁定

| 对象 | 版本 | SHA-256 |
|---|---|---|
| `client-core`（自研数据面） | 0.1.0（commit `af0bf9e`） | 由源码构建，见 CI |
| Mihomo 内核（Windows amd64 compatible） | **v1.19.34**（go1.26.11，with_gvisor） | zip `cd6738d4648cc1a5f48049a1343b697ab510889d648f045c5c780fc1ab443a79`；exe `5a0231811a80b27477ebdee15684b41946d01afbd2114534e9a4b256125ca147` |

节点：单加密入站 `<NODE_HOST>:443`，`mlkem768x25519plus.native.1rtt`，VLESS + WS + TLS。测试凭据仅存于仓库外临时目录，未入库。

## 2. 通过线结果

> §6 的第 1 条为「TCP+UDP 通」，此处拆成两行分别验证，故表中为 7 行；第 6、7 行即原第 5、6 条。

| # | 通过线 | client-core | Mihomo v1.19.34 |
|---|---|---|---|
| 1 | TCP 出站 | ✅ 出口 IP = 节点出口 | ✅ 出口 IP = 节点出口 |
| 2 | 域名经 SOCKS5（ATYP=DOMAINNAME） | ✅ 手写 SOCKS5 握手返回 `0x00` | ✅ 同 |
| 3 | ≥1 MiB 传输（跨 8192 字节分片） | ✅ 5,242,880 B 完整下载 | ✅ 同 |
| 4 | UDP | ✅ DNS 经 VLESS `command=2`，服务端日志可证 | ❌ 见 §3.1 |
| 5 | DNS 无泄漏 | ✅ 查询由代理侧发起 | ❌ 53 端口走 DIRECT |
| 6 | kill sidecar 自愈 | ⏸ 依赖 sidecar 架构，未覆盖 | ⏸ 同 |
| 7 | ~~12 h 无内存增长~~（已移出 C0d → FA） | 不适用 | 不适用 |

**服务端证据**（节点调试日志，测试时间窗内）：

```
VLESS request accepted user=1 command=2 address=8.8.8.8 port=53
```

该记录只在 `client-core` 测试期间出现，证明 UDP 载荷经 VLESS 隧道转发，而非本地直连。

## 3. 关键发现

### 3.1 Mihomo 的 UDP/DNS 路径不受规则控制

配置 `mixed-port` + `rules: [DST-PORT,53,proxy, NETWORK,udp,proxy, MATCH,proxy]`，日志仍为：

```
[UDP] 127.0.0.1:xxxxx --> 8.8.8.8:53 doesn't match any rule using DIRECT
```

即使存在 `MATCH` 兜底，UDP 到 53 端口仍走 DIRECT。关闭 `dns.enable` 后行为不变。这表明该版本内核对 DNS 端口与 fake-ip 目标存在硬编码路径，**普通 SOCKS5 配置无法保证 UDP DNS 经代理**。

影响：若以 Mihomo 作为内核，DNS 泄漏不能靠规则修正，需改用 TUN 模式或 `dns.proxy-server-nameserver` 等专门配置；这会改变 C0d 的实现路径与工作量评估。

### 3.2 fake-ip 会污染外部 UDP 目标

STUN 测试首次失败时，目标被本地 DNS 解析为 `198.18.0.115`（fake-ip 段），隧道无法转发。该现象说明：PoC 阶段必须确保测试目标使用真实 IP，或显式绕过 fake-ip。

### 3.3 自研数据面的 UDP 行为更可控

`client-core` 只有一条出站路径（规则 `MATCH,proxy` → VLESS），UDP 无法被内核隐式改写；服务端日志可直接证明 `command=2`。代价是需要自行实现 DNS、fake-ip 与健康检查（已在 0.1.0 具备）。

## 4. 资源占用

| 对象 | RSS |
|---|---|
| `client-core` | 约 20.5 MiB（10 分钟平台期） |
| Mihomo v1.19.34 | 约 35.9 MiB |

10 分钟长稳采样（15 s 间隔、8 次链路探测全部成功）显示：首次转发后出现一次性台阶（+5.9 MB / +35 句柄），随后进入平台期，8.5 分钟内仅上升约 0.8 MB，未见线性泄漏。详见 `2026-11-12-soak-10min.md`。**12 h 长稳已移入 FA 最终验收**，C0d 不据此判定无泄漏。

## 5. 未覆盖项与后续

| 项 | 原因 | 建议 |
|---|---|---|
| ~~kill sidecar 自愈~~ | ✅ 已收口 | 双进程架构见 ADR 0001；实机证据 s5 8/8、s9 11/11 |
| 12 h 内存 | **已移出 C0d** | 归入 FA 最终验收；二进制冻结后采集 RSS / 句柄 / 连接数 |
| UDP 逐字节校验 | 节点 UDP echo 未运行 | 恢复测试 echo 或改用 STUN 真实 IP |
| Mihomo UDP | 内核硬编码 DIRECT | 评估 TUN 模式或 `proxy-server-nameserver` 配置成本 |

## 6. 对 G1 的影响

C0d 当时的实测结论为「部分完成」（自研 4/6、Mihomo 2/6），Mihomo 的 UDP/DNS 负面证据仍是内核选型依据。**按 2026-11-12 流程调整后的当前状态**：C0d 5 条通过线 5/5 全通过（自愈已补齐），12 h 长稳归 FA 最终验收，不再是 G1 输入。G1 仍为 No-Go，剩余前置为许可评审、内核路线追认、投诉分布采集、Go 对照组。
