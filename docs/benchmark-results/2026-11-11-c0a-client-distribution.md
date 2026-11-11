# C0a 用户侧客户端类型 / 版本分布（只读采集）

- 采集时间：2026-11-11 20:30（GMT+8）
- 采集方式：面板机只读执行 `script/collect_client_ua.sh`，只输出聚合计数；**绝不输出原始日志行 / token / IP / 用户 ID**（订阅 URL 含 `token=` 明文，已全程规避）。
- 数据源：`<PANEL_ACCESS_LOG>`（宝塔按站点分离 access log，extended combined 格式，CF 代理下真实 IP 已被替换，不在采集范围）。
- 筛选条件：请求路径含 v2board 生产订阅路径 `/Delta/Force/BigRED`（`v2board.subscribe_path`，非源码默认 `/api/v1/client/subscribe`）。
- 样本窗口：**2026-11-03 13:25:03 +0800 → 2026-11-11 20:29:43 +0800**，共 **1275** 条订阅请求（约 8 天）。
- 可识别率：**99.4%**（仅 8 条无法归类）。

> 说明：`example.com.log`、`pay.example.com.log` 订阅命中为 0；`www.example.com.log.1` 在面板机不存在（16MB 滚动日志实为 `example.com.log.1`，属另一域名）。故唯一有效数据源为 `www.example.com.log` 当前未切割周期。

---

## 1. 分类聚合（按 UA / `?flag=`）

| 类别 | 计数 | 占比 |
|---|---:|---:|
| Clash_Mihomo（Clash 系 / mihomo / ClashMeta / clash-verge / FlClash / CFW 等） | 977 | 76.6% |
| other（Shadowrocket / v2rayN / V2rayU / NekoBox / ClashForAndroid 等） | 249 | 19.5% |
| Xray（v2rayN 等，UA 含 `v2ray`） | 39 | 3.1% |
| sing-box（含 `?flag=sing-box`） | 2 | 0.2% |
| unidentified | 8 | 0.6% |
| **total** | **1275** | 100% |

`?flag=sing-box` 显式命中仅 2 条，与 sing-box 类别计数一致。

## 2. 平台维度聚合（脱敏 inline awk，仅取 UA 字段）

| 平台推断 | 计数 | 占比 | 判定依据 |
|---|---:|---:|---|
| Android | 359 | 28.2% | `ClashMetaForAndroid` / `ClashForAndroid` / `NekoBox` / `Platform/android` |
| iOS | 264 | 20.7% | `Shadowrocket` / `ShadowClash` / `mihomo.party` / `Platform/ios` |
| 跨平台 / 未标 OS | 557 | 43.7% | `clash-verge`（不标系统）/ `ClashMeta`/`mihomo` 桌面 / `FlClash` 未标平台 / `Hako` |
| Windows 桌面（明确） | 66 | 5.2% | `ClashforWindows` / `v2rayN` / `Platform/windows` |
| macOS | 22 | 1.7% | `V2rayU` / `Platform/macos` |
| other | 7 | 0.5% | 未匹配 |

**Windows 单一平台占比上限 48.9%（含全部跨平台），下限 5.2%（仅明确 Windows-only 客户端）。** 移动端（iOS+Android）合计 48.9%。

## 3. 高频 UA 示例（Top 15，计数/客户端）

| 计数 | UA |
|---:|---|
| 178 | clash-verge/v2.5.10 |
| 149 | clash-verge/v2.5.8 |
| 119 | ClashMetaForAndroid/2.11.27.Meta |
| 65 | ClashMetaForAndroid/2.11.35.Meta |
| 54 | Shadowrocket/2615 ... iPhone16,1 |
| 52 | ClashMetaForAndroid/2.11.23.Meta |
| 46 | Shadowrocket/2615 ... iPhone16,1 |
| 45 | clash.meta/1.19.30 Hako/v1.0.8.87 ... iPhone14,2 |
| 36 | clash-verge/v2.5.2 |
| 34 | clash-verge/v2.4.7 |
| 30 | Shadowrocket/3488 ... iPhone19,7 |
| 24 | clash-verge/v2.4.4 |
| 24 | ClashForAndroid/2.5.12.premium |
| 23 | v2rayN/7.0 V2rayU/5.4.0 |
| 22 | clash-verge/v2.5.7 |

Top 50 完整输出见面板机执行记录；长尾为个位数计数的小众客户端。

---

## 4. 对 G1 立项门控的解读

1. **交接文档 §2.2「覆盖率被客户端卡住」前提不成立。** 实测 Clash 系占 76.6%，而 ClashMeta 系（mihomo）原生支持 VLESS Encryption（Xray-core 亦支持）。这意味着「自研客户端才能解锁 encryption 覆盖」的假设被削弱——绝大多数目标用户已用支持 encryption 的客户端。
2. **内核选型强烈偏向 Mihomo 系 / 自研，而非 sing-box。** sing-box 仅 0.2%（2 条），若选 sing-box 内核则覆盖 <1% 用户，ROI 极低。Mihomo 系 + Clash API + ClashMeta YAML 订阅已占主导，与策略文档「倾向 Mihomo」一致但证据更硬。
3. **「Windows 优先」前提需修正。** 移动端（iOS+Android）合计 48.9%，与桌面端相当；明确 Windows 桌面仅 5.2%。若自研只做 Windows，按 UA 口径最多覆盖 ≤48.9%（上限，含跨平台），下限 5.2%。这正是策略文档 §8 未决问题「平台顺序」的实证答案——但答案偏向「先做 Clash 生态兼容 / 订阅优化，而非急于自研单平台客户端」。
4. **自研客户端「换皮」风险被再次确认。** 现有 Clash 系已覆盖 76.6% 且支持 encryption；自研若无末段体验 / 分发 / 供应链的差异化，迁移理由弱。

> 注意：以上是基于「订阅请求 UA」的分布，代表客户端**类型**占比，不是「独立用户」数（同一用户多次订阅会被重复计数）。作为类型占比的代表性样本有效；若需精确用户数，需面板数据库按 token 去重（超出 C0a 只读采集范围，且涉及用户隐私，不建议为门控做此改动）。

## 5. 诚实声明

- 数据为**订阅请求 UA**，非去重用户；作为类型占比样本有效，不可直接当「用户数」。
- 平台归类中 **43.7% 为跨平台/未标 OS**（clash-verge 等不在 UA 标系统），故 Windows 占比只能给上下限。
- 样本窗口约 8 天，受近期用户行为影响；如需更长周期可纳入 gz 历史日志（当前 www 域名无 `.gz` 滚动，周期即 8 天）。
- 全程未触碰生产配置、面板数据库、订阅数据；脚本只读、聚合、脱敏，符合 `docs/client-design-plan.md` §6 与 `script/collect_client_ua.sh` 安全约束。
