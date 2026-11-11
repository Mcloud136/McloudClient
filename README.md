# McloudClient

[![CI](https://github.com/Mcloud136/McloudClient/actions/workflows/ci.yml/badge.svg)](https://github.com/Mcloud136/McloudClient/actions/workflows/ci.yml)

用户侧客户端（Rust），与节点服务端 [`v2node-rust`](https://github.com/Mcloud136/v2node-rust) 分离维护。

当前阶段：C0 收口。`client-core` 是完整数据面（SOCKS5 入站 → 规则分流 → VLESS/WS/TLS/mlkem 出站），
`client-sidecar` 是控制面（订阅 URL 拉取、快照推送、IPC、崩溃重连），内核可作为 Windows 服务运行
（自动启动 + 崩溃重启）。TUN 与 GUI 尚未启动，见 `docs/client-design-plan.md`。

## 与 v2node-rust 的关系

只复用 `v2node-proto`（VLESS 帧与 mlkem 握手的共享实现），不依赖服务端任何私有模块。

以 **git tag** 固定版本引用：

```toml
v2node-proto = { git = "https://github.com/Mcloud136/v2node-rust", tag = "v0.2.0" }
```

服务端发布新 tag 后，提升 `tag` 并执行 `cargo update -p v2node-proto` 即可。
本仓库可脱离同级目录独立构建（CI 在 ubuntu 与 windows 上各跑一遍验证）。

## 构建与验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

实机验证（需可达节点与 profile）：

```bash
cargo run -p client-core -- --profile <profile.json> --listen 127.0.0.1:1080
curl --socks5-hostname 127.0.0.1:1080 https://api.ipify.org
```

订阅热重载：

```bash
cargo run -p client-core -- --profile <profile.json> --reload-interval 2
# 改 profile 观察 "router reloaded"；写入非法 JSON 应保留旧配置
```

## 目录

| 路径 | 说明 |
|---|---|
| `crates/client-core/` | 数据面：SOCKS5、VLESS 出站、规则引擎、DNS、rule-set、健康检查、热重载、文件日志、Windows 服务入口 |
| `crates/client-sidecar/` | 控制面：IPC 帧协议与命名管道、事件推送、快照存储、订阅拉取、崩溃重连 |
| `scripts/install-service.sh` | 安装/卸载 Windows 服务（自动启动 + `sc failure` 崩溃重启 + 快照路径） |
| `scripts/` | `clash2profile.py`、GEO 数据更新、DNS/UDP 端到端测试、C0a 采集脚本 |
| `docs/` | 客户端设计规划、G1 门控、C0a 分布数据、C0d 实测报告、ADR |

## 许可

本项目自 2026-11-12 起采用 **MIT** 许可（见 `LICENSE`，SPDX: `MIT`）。G1 评审决议见 `docs/g1-gate-review.md` §8。

第三方依赖与数据的许可需单独遵守，不因本项目采用 MIT 而自动兼容：

| 来源 | 许可 | 分发要求 |
|---|---|---|
| `v2node-proto`（`v2node-rust` tag `v0.2.0`） | MPL-2.0 | 文件级弱 copyleft；保留许可文本与源码获取说明，修改过的 MPL 文件须继续以 MPL 开源 |
| GEOIP 主源 `misakaio/chnroutes2` | CC-BY-SA 2.0 | 署名 + 相同方式共享；或改用 APNIC 源规避（见 `scripts/geoip-update.py --sources apnic`） |
| `wintun.dll`（C1 后引入） | WireGuard LLC 预编译二进制许可 | 随包保留声明文本 |
| 其余 Rust crate | 各 crate 自身 LICENSE | 发布前跑一次依赖许可扫描 |

MIT 与 MPL-2.0 可以共存：MPL 只覆盖其自身文件，不要求整个组合作品开源；但分发二进制时仍须附 MPL-2.0 文本与对应源码获取方式。自研路线不打包 Mihomo / sing-box，因此 §3 历史清单中的 GPL 分发义务不适用。**本文是工程记录，不是法律意见**；正式发布前建议法务复核一次。

## 注意

- **不要提交任何订阅地址、token、节点 IP、SSH 路径或密钥**。本仓库预期公开。
- GEOIP 主源 `misakaio/chnroutes2` 为 **CC-BY-SA 2.0**：随客户端分发需署名并以相同方式共享。规避方式见 `scripts/geoip-update.py --sources apnic`。
- 后续 TUN（Windows wintun）与 GUI 尚未启动，见 `docs/client-design-plan.md`。
