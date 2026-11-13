# 功能冻结 pin 清单 — v0.1.1（FA 长稳二进制锚点）

> 冻结日期：2026-11-13。本清单锚定 C0–C2 功能冻结后的 release 二进制，
> 作为 24 h 长稳与 FA 最终验收的被测对象。**冻结后任何代码变更使长稳数据失效，必须重跑。**
> 本清单只 pin 基线，不含发布用签名 manifest（发布 manifest 属 C3 前置，另行生成）。
>
> **v0.1.1 取代 v0.1.0**：v0.1.0 冻结后发现两处缺陷并修复（属代码变更，故重新冻结）：
> ① `--update-manifest` 默认值非空导致 Gitee 优先多源逻辑被旁路（实际只查 GitHub）；
> ② fetch 不跟随 HTTP 重定向，Gitee raw 302 到 raw.giteeusercontent.com 时失败。
> 修复后版本 bump 0.1.1，重新统一构建 + 冒烟 + pin。v0.1.0 tag 保留作历史，不再作长稳锚点。

## 基线

| 项 | 值 |
|---|---|
| 版本 | 0.1.1 |
| tag | `v0.1.1`（含本清单的提交上） |
| 渠道 | GitHub `Mcloud136/McloudClient` + Gitee `wxbns/mcloud-client`（同步） |

## 二进制 SHA-256（release，统一重建于 2026-11-13 19:33）

| 产物 | SHA-256 | 字节 |
|---|---|---|
| client-core.exe | `a989ecfdf02bf7758c7fa17f9a6b0cbc2d7f846eb162b054c1948155f24b1a59` | 8384000 |
| client-gui.exe | `4d86f9ee9afbe5a1d8f917f3d06286209d355491b0f6e01e68cfcd7de6859ef3` | 10314240 |
| sidecar.exe | `9c7ec4f6a40fcdd1acab511cfbabf7d9756eba9c2341e4439f4f61dca0b6267d` | 5484032 |
| McloudClient-Setup-0.1.1.exe | `aa85fca38caec33df4ef03cf10bab9f63cfb6ce3a07084bf6d18c5eef61da757` | 10039828 |

归档位置（仓库外，防 `cargo clean` 丢失）：`D:\wxmuma\_freeze\v0.1.1\`，与 `target/` 产物逐字节一致（已校验）。

## 构建环境（阈值绑定本机，不跨机外推）

| 项 | 值 |
|---|---|
| OS | Windows 11 专业工作站版（build 26200） |
| CPU | Intel Core i9-14900HX |
| GPU（UI RSS 阈值绑定） | NVIDIA GeForce RTX 4070 Laptop GPU，驱动 32.0.16.1062 |
| Rust | rustc 1.99.1 (58cf8f9c3 2026-10-13)，cargo 1.99.1 |
| 构建 | `cargo build --release`（本机）+ NSIS 3.12 `scripts/build-installer.sh` |

## 冻结前验证

- 统一重建：core/sidecar/gui + 安装包同基线（0.1.1）。
- 安装器冒烟：`_c0d_test/s16_installer_probe.py` **11/11**（真实安装→服务 RUNNING→升级 previous 字节一致→升级后 RUNNING→卸载全清）。
- 门禁：fmt / clippy `-D warnings` / test / deny 全绿。
- 多源更新实机：GUI 不传 `--update-manifest`，Gitee 优先成功（无回退），首发 0.1.1 提示"已是最新版本"；fetch 跟随 Gitee raw 302 重定向。
- 签名：GUI 内嵌 Ed25519 公钥，验签硬拒绝；实机三例（合法接受/未签名拒绝/错签名拒绝）通过。

## 失效与重跑规则

- 冻结后任何代码变更（含修复）→ 长稳数据作废，须重新冻结 + 重跑 24 h。
- FA 期间禁止合入新代码。
- 换机型 / 换 GPU 驱动 → UI RSS ≤150 MiB 阈值须重新校准，不沿用本清单。