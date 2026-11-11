# McloudClient

[![CI](https://github.com/Mcloud136/McloudClient/actions/workflows/ci.yml/badge.svg)](https://github.com/Mcloud136/McloudClient/actions/workflows/ci.yml)

用户侧客户端（Rust），与节点服务端 [`v2node-rust`](https://github.com/Mcloud136/v2node-rust) 分离维护。

当前阶段：`crates/client-core` 数据面 PoC —— SOCKS5 入站 → 规则分流 → VLESS/WS/TLS/mlkem 出站。

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
| `crates/client-core/` | 数据面：SOCKS5、VLESS 出站、规则引擎、DNS、rule-set、健康检查、热重载 |
| `scripts/` | `clash2profile.py`、GEO 数据更新、DNS/UDP 端到端测试、C0a 采集脚本 |
| `docs/` | 客户端设计规划、G1 门控、C0a 分布数据、C0d 实测报告、ADR |

## 注意

- **不要提交任何订阅地址、token、节点 IP、SSH 路径或密钥**。本仓库预期公开。
- GEOIP 主源 `misakaio/chnroutes2` 为 **CC-BY-SA 2.0**：随客户端分发需署名并以相同方式共享。规避方式见 `scripts/geoip-update.py --sources apnic`。
- 后续 TUN（Windows wintun）与 GUI 尚未启动，见 `docs/client-design-plan.md`。
