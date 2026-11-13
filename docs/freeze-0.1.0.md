# 功能冻结 pin 清单 — v0.1.0（FA 长稳二进制锚点）

> 冻结日期：2026-11-13。本清单锚定 C0–C2 功能冻结后的 release 二进制，
> 作为 24 h 长稳与 FA 最终验收的被测对象。**冻结后任何代码变更使长稳数据失效，必须重跑。**
> 本清单只 pin 基线，不含发布用签名 manifest（发布 manifest 属 C3 前置，另行生成）。

## 基线

| 项 | 值 |
|---|---|
| 版本 | 0.1.0 |
| 基线 commit | `057d22463c5df163349928a55e28af924ca92530` |
| tag | `v0.1.0`（含本清单的提交上） |
| 渠道 | GitHub `Mcloud136/McloudClient` + Gitee `wxbns/mcloud-client`（同步） |

## 二进制 SHA-256（release，统一重建于 2026-11-13 18:12–18:13）

| 产物 | SHA-256 | 字节 |
|---|---|---|
| client-core.exe | `a2d727c4e4fc5d90251d3fa2d24a78d888d7a63870774d8d747227cd3fa2d3cc` | 8301056 |
| client-gui.exe | `f1f9282646a4f966a8b6d170d8352391219fe147fb4eb6e36ff39d920086eda7` | 10287104 |
| sidecar.exe | `18ead5a3dd159af544387e05307c0df6f4c7081a1f13184d662d97dcef426ca8` | 5442048 |
| McloudClient-Setup-0.1.0.exe | `a0485f537e971779025b1d8ceb1a9287d7d966a12dbd5ed2924dfd303153f9f0` | 9986768 |

归档位置（仓库外，防 `cargo clean` 丢失）：`D:\wxmuma\_freeze\v0.1.0\`，与 `target/` 产物逐字节一致（已校验）。

## 构建环境（阈值绑定本机，不跨机外推）

| 项 | 值 |
|---|---|
| OS | Windows 11 专业工作站版（build 26200） |
| CPU | Intel Core i9-14900HX |
| GPU（UI RSS 阈值绑定） | NVIDIA GeForce RTX 4070 Laptop GPU，驱动 32.0.16.1062 |
| Rust | rustc 1.99.1 (58cf8f9c3 2026-10-13)，cargo 1.99.1 |
| 构建 | `cargo build --release`（本机）+ NSIS 3.12 `scripts/build-installer.sh` |

## 冻结前验证

- 统一重建：core/sidecar/gui 同基线（此前 installer 为 #17 旧 GUI，不含 #18 验签，已重建修正）。
- 安装器冒烟：`_c0d_test/s16_installer_probe.py` **11/11**（真实安装→服务 RUNNING→升级 previous 字节一致→升级后 RUNNING→卸载全清）。
- 门禁：fmt / clippy `-D warnings` / test（146）/ deny 全绿；CI 双平台 + 构建机 Linux 全绿（057d224）。
- 签名：GUI 内嵌 Ed25519 公钥，验签硬拒绝；实机三例（合法接受/未签名拒绝/错签名拒绝）通过。

## 失效与重跑规则

- 冻结后任何代码变更（含修复）→ 长稳数据作废，须重新冻结 + 重跑 24 h。
- FA 期间禁止合入新代码。
- 换机型 / 换 GPU 驱动 → UI RSS ≤150 MiB 阈值须重新校准，不沿用本清单。