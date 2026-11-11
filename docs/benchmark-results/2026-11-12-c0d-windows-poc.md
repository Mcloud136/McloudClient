# C0d Windows 链路 PoC 实测（2026-11-12）

> 范围：`docs/g1-gate-review.md` §6 定义的 6 条通过线中可自动化验证的部分。
> 环境：Windows（本机）、Rust 1.99.1、单节点单加密入站、系统代理（未使用 wintun）。
> 结论：**4/6 通过线完成**；UDP 能力在自研数据面成立、在 Mihomo 上受内核 DNS 路径限制；自愈与 24 h 内存未验证。

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
| 7 | 24 h 无内存增长 | ⏸ 仅短时采样（见 §4） | ⏸ 同 |

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

## 4. 资源占用（短时采样）

| 对象 | RSS |
|---|---|
| `client-core` | 21,304 KB（约 20.8 MB） |
| Mihomo v1.19.34 | 36,780 KB（约 35.9 MB） |

三次采样间隔 1 s，数值无增长。**24 h 长稳未执行**，不能据此声明无泄漏。

## 5. 未覆盖项与后续

| 项 | 原因 | 建议 |
|---|---|---|
| kill sidecar 自愈 | 当前为单进程数据面，无 sidecar/内核分离 | 先定架构（sidecar 是否独立进程），再实现 supervisor 与重启策略 |
| 24 h 内存 | 需长稳窗口 | 独立长稳任务，采集 RSS/句柄/连接数 |
| UDP 逐字节校验 | 节点 UDP echo 未运行 | 恢复测试 echo 或改用 STUN 真实 IP |
| Mihomo UDP | 内核硬编码 DIRECT | 评估 TUN 模式或 `proxy-server-nameserver` 配置成本 |

## 6. 对 G1 的影响

C0d 从「未执行」变为「部分完成」：Windows 上 TCP/域名/大传输/UDP（自研）已实测通过，但内核路线的 UDP/DNS 可行性出现负面证据。G1 仍为 No-Go，剩余前置项为许可评审、内核路线拍板、C0d 的自愈与长稳部分。
