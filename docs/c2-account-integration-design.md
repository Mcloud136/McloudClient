# C2 设计方案：v2board 账号体系与订阅电商集成（2026-11-12 草案）

> **状态**：设计草案，待用户确认；**不产生生产代码**。
> **触发**：用户临时变更需求 —— 客户端集成 v2board 的账号订阅数据、账号管理、订阅购买，
> 采用「输入账号密码登录 → 进入主界面」的形态（类比 QQ），主界面提供代理功能、
> 订阅信息、订阅购买、账户管理四类功能。
> **不撤销既有决议**：G1 Go、ADR 0001（自研 R2）、ADR 0003（维持自研）、D1（egui + glow）
> 继续有效；本方案**改变 C1/C2 的范围与顺序**，并新增架构锁点（见 ADR 0004）。

---

## 1. 需求重述与范围

### 1.1 目标形态

```
启动 → [已登录且凭据有效] → 主界面
     → [未登录 / 凭据失效] → 登录页（邮箱 + 密码）→ 主界面

主界面
  ├─ 代理       连接开关、系统代理、节点列表、测速、负载均衡、规则（既有能力）
  ├─ 订阅信息   套餐、已用/总流量、到期时间、重置日、重置订阅链接
  ├─ 订阅购买   套餐商店、周期选择、优惠券、支付方式、支付二维码 / 跳转、订单状态
  └─ 账户管理   账户资料（脱敏）、修改密码、通知设置、余额/佣金、登出
```

### 1.2 首版做 / 不做

| 做 | 不做 |
|---|---|
| 单站点、单账号、邮箱+密码登录 | 多账号 / 多站点切换 |
| 账户/订阅/商店/订单的只读展示与标准操作 | 面板管理端 API、节点管理、工单、邀请、佣金提现 |
| 下单 + 支付二维码（原生渲染）/ 跳转系统浏览器 | 客户端内嵌支付页、代收卡信息、WebView |
| 登录态驱动订阅源，自动同步与重载 | 改动面板服务端、依赖面板自定义补丁 |
| 保留「手工导入订阅链接」作为兼容 / 离线路径 | 自有 `v2node://` 订阅格式（仍属 C1 之后） |
| 凭据安全存储、脱敏、日志与截图防护 | 遥测、用户行为上报 |

### 1.3 默认假设（待用户确认，见 §11）

| 编号 | 假设 |
|---|---|
| A1 | 面板基址与订阅路径**可配置、构建时预置**，用户可在「高级设置」修改；首版不自动发现 |
| A2 | 首版单账号、单站点 |
| A3 | 登录方式为邮箱 + 密码；注册 / 找回密码**跳转系统浏览器**，不在客户端内实现（但保留接口扩展位） |
| A4 | 「记住我」默认**关闭**；开启时只持久化 `auth_data`，绝不存密码 |
| A5 | 支付二维码由 egui 原生渲染；需要网页支付时跳转系统浏览器（不破坏 D1） |
| A6 | 按**最坏情况**设计：`auth_data` 视为等价于密码的长期凭据（依据见 §2.1） |
| A7 | 登出后已缓存的订阅继续可用至过期，避免「退出登录即断网」 |
| A8 | 中文 UI 仍需字体方案（沿用 roadmap P1.1 未决项） |

---

## 2. 事实基线（2026-11-12 核实）

### 2.1 认证机制（决定性约束）

v2board 用户端 API 前缀 `/api/v1`，所有需鉴权的 GET 请求携带 `Authorization` 头（或 `auth_data` 参数），值为登录返回的 `auth_data`。

| 分支 | `POST /passport/auth/login` 返回 | 安全含义 |
|---|---|---|
| v2board 1.6.0 系 | `{ token, auth_data }`，文档明确 **`auth_data = base64(email:password)`** | **等同长期密码**：无过期、无会话撤销，泄露即账号泄露，只能靠改密 / 重置使其失效 |
| wyx2685 等分支 | JWT，经 `AuthService::decryptAuthData()` 解出；提供 `/user/checkLogin`、`/user/getActiveSession`、`/user/removeActiveSession` | 可撤销会话，但仍是 bearer 凭据 |

**结论**：客户端不能把它当普通 token 处理。设计按 1.6.0 的最坏情况：加密存储、用户隔离、可一键清除、永不出现在 UI / 日志 / IPC / 崩溃转储。
建议在面板侧推动改用可撤销会话（JWT + 会话管理），但客户端不依赖此改进。

### 2.2 接口清单（只列首版依赖的**标准用户端**接口）

| 功能 | 方法 | 路径 | 响应要点 |
|---|---|---|---|
| 站点配置 | GET | `/passport/comm/config` | `tos_url`、`app_url`、`is_email_verify`、`is_invite_force`、`is_recaptcha`、`recaptcha_site_key` |
| 登录 | POST | `/passport/auth/login` | `email` + `password` → `{ token, auth_data }` |
| 登录校验 | GET | `/passport/auth/check` | `is_login` |
| 登出 | GET | `/user/logout` | `true` |
| 账户信息 | GET | `/user/info` | `email`、`transfer_enable`、`expired_at`、`balance`、`commission_balance`、`plan_id`、`banned`、`avatar_url` |
| 订阅信息 | GET | `/user/getSubscribe` | `plan_id`、`token`、**`subscribe_url`**、`u`、`d`、`transfer_enable`、`expired_at`、`reset_day` |
| 重置订阅链接 | GET | `/user/resetSecurity` | 新 `subscribe_url`（旧链接立即失效） |
| 待办统计 | GET | `/user/getStat` | `[待付订单, 工单, 待确认邀请]` |
| 修改密码 | POST | `/user/changePassword` | `old_password` + `new_password` → `true` |
| 通知设置 | POST | `/user/update` | `remind_expire` / `remind_traffic` |
| 套餐商店 | GET | `/user/plan/fetch` | `id`、`name`、`content`、各周期价格、`transfer_enable`、`show`、`renew` |
| 支付方式 | GET | `/user/order/getPaymentMethod` | `id`、`name`、`payment` |
| 优惠券校验 | POST | `/user/coupon/check` | 折扣信息 |
| 创建订单 | POST | `/user/order/save` | `plan_id` + `cycle` → `trade_no` |
| 结算订单 | POST | `/user/order/checkout` | `trade_no` + `method` → **`{ type: 0 二维码 | 1 URL, data: 付款地址 }`** |
| 订单状态 | GET | `/user/order/check?trade_no=` | `status`：0 待支付 / 1 开通中 / 2 已取消 / 3 已完成 / 4 已折抵 |
| 订单列表 / 详情 / 取消 | GET/POST | `/user/order/fetch`、`/details`、`/cancel` | 订单视图 |

**金额口径**：v2board 的 `balance` / 各 `*_price` / `total_amount` 均为**整数分**。DTO 层保持整数，UI 层用 `¥` 格式化，禁止浮点中间量。

**流量口径**：`u` / `d` / `transfer_enable` 为字节整数；`expired_at` 为 Unix 秒，UI 本地化展示。

### 2.3 本地环境事实

- 生产订阅路径为**自定义值**（非源码默认 `/api/v1/client/subscribe`），因此订阅 URL 必须由面板 API 返回，不能客户端拼接。
- 面板存在自定义补丁，且 `update.sh` 的 `git reset --hard` 会覆盖补丁 → **客户端不得依赖任何非标准接口或补丁字段**。
- 现有 `client_sidecar::fetch` 是最小 HTTP/1.1 客户端：只支持 `GET` + `200` + `Content-Length`/chunked；
  已具备 `rustls` + `tokio-rustls` + `rustls-native-certs`，**TLS 能力可复用**。

---

## 3. 架构锁点（详见 `docs/adr/0004-account-integration-boundaries.md`）

| 编号 | 锁点 | 理由 |
|---|---|---|
| L1 | **凭据不出控制面**：密码 / `auth_data` / 订阅 URL 只在 `client-sidecar` 的 `panel` 与 `credential` 模块内以 `Secret<T>` 持有；GUI 只拿脱敏 DTO | `auth_data ≈ 密码`；UI 层一旦接触，截图、OCR、日志、崩溃转储都会成为泄露面 |
| L2 | **内核服务完全不碰账号**：`client-core`（session 0）只接收订阅内容变更 → 快照 → reload | 账号态与数据面解耦；服务进程无用户凭据，减少提权进程的风险面 |
| L3 | **不引入 WebView**：二维码用纯 Rust `qrcode` 生成位图在 egui 渲染；URL 用系统浏览器 | D1 刚定案 `egui + glow`；WebView 会推翻内存画像并破坏「token 不进 WebView」约束 |
| L4 | **不依赖面板补丁**：只用标准用户端 API，字段全 `Option`/默认值，未知字段忽略 | 面板升级 `git reset --hard` 会抹掉补丁 |
| L5 | **复用自有 HTTP 栈**：扩展 `client-sidecar` 的 HTTP 模块支持 `POST` / JSON / 自定义头 / 状态码，不引入 `reqwest` | 已有 TLS 栈；避免大型依赖树抬高二进制与内存，且少一批 `cargo deny` 许可项 |
| L6 | **单连接 IPC 不变**：账号态走 GUI 与 sidecar 同进程的 `mpsc`，不给内核新增 IPC 方法 | ADR 0002 / `c1-ipc-contract.md` §1 架构锁点，避免广播多连接改造 |

> **诚实边界**：L1 是**模块与类型边界**，不是 OS 级进程隔离 —— GUI 与 sidecar 本来就是同一进程
> （`c1-ipc-contract.md` §1）。真正的 OS 隔离在 L2：内核服务在 session 0，不持有任何账号数据。

---

## 4. 进程与模块划分

```
client-core.exe（Windows 服务，session 0，管理员/SYSTEM）
  └─ 命名管道（单连接，现有契约 v1.1）… 无账号数据
        ▲
client-gui.exe（用户会话，与 sidecar 同进程）
  ├─ [新] panel/        ← 位于 client-sidecar crate（lib）
  │    ├─ http.rs       扩展：GET/POST + JSON + Authorization + 状态码/错误映射
  │    ├─ client.rs      PanelClient trait、站点配置、限流与指数退避、TLS 强校验
  │    ├─ auth.rs        login / check / logout
  │    ├─ account.rs     info / getSubscribe / resetSecurity / getStat / changePassword / update
  │    ├─ shop.rs        plan / coupon / order/*
  │    ├─ model.rs       脱敏 DTO（serde，字段 Option/Default，无凭据字段）
  │    └─ secret.rs      Secret<T>：无 Debug/Display/Serialize，Drop 清零
  ├─ [新] credential/   CredentialStore trait
  │    ├─ windows.rs     Windows Credential Manager（按用户隔离、DPAPI 保护）
  │    └─ stub.cfg      非 Windows：内存桩，保证双平台 CI 可编译
  ├─ [既有] snapshot/   订阅拉取 / 原子写 / 5 版回滚 —— source 由 panel 注入
  ├─ [既有] task.rs     内核 IPC 任务（metrics / nodes 轮询、事件、命令）
  ├─ [新] panel_task.rs 账号任务（登录、刷新、下单、订单轮询），与 IPC 任务并列
  ├─ state.rs           AuthState 状态机 + 账户/订阅/商店/订单视图（全部脱敏）
  └─ UI：login.rs / home.rs（代理·订阅·商店·账户四区）/ shop.rs / account.rs
```

**依赖方向**：`UI → panel_task → PanelClient → http`；`credential` 只被 `panel` 调用；
`snapshot` 只接收 `subscribe_url`（由 panel 传入内存），不反向暴露。
GUI 模块**不 import** `secret` / `credential`（用 code review + 目录边界约束）。

---

## 5. 凭据与敏感数据边界（本方案的核心）

| 数据 | 生命周期 | 存储 | UI | 日志 / IPC / 截图 |
|---|---|---|---|---|
| 密码 | 一次登录请求体 | 内存，提交后立即清零 | 密码框（不回显、禁复制） | **禁止** |
| `auth_data`（≈密码） | 会话；「记住我」时跨会话 | **OS 凭据库**（用户隔离）；默认不持久 | **禁止** | **禁止**；`Debug` 输出 `[REDACTED]` |
| `token`（login 返回） | 会话 | 内存 | 禁止 | 禁止 |
| 订阅 URL / 订阅 token | 会话；快照系统持有 | 控制面内存 + 现有快照机制 | **禁止**（只显示「已同步」与来源类别） | 禁止；日志只写 `source="url"` |
| `Authorization` 头 | 单次请求 | 内存 | 禁止 | 禁止 |
| 账户 DTO | UI 状态 | 内存 + 可选脱敏缓存（不含凭据） | 邮箱掩码 `a***@x.com` | 可写聚合计数 |
| 支付二维码 / 付款 URL | 短时 | 内存 | 可显示（不含凭据） | 禁止写 URL 全文 |
| `trade_no` | 订单会话 | 内存 / 日志 | 可显示 | 可写（不含凭据） |

**实现约束**：

- `Secret<T>`：不实现 `Debug` / `Display` / `Serialize`，只在 `panel`/`credential` 内 `expose()`；`Drop` 调用 `zeroize`。
- 密码输入框：禁用复制、禁用自动填充缓存、失焦即清空；**实机回归脚本的 PrintWindow 截图与 OCR 必须用假账号，且不得把密码框内容写进产物**（现有 `s11b` 用截图 + WinRT OCR，登录页要专门加防护）。
- 日志：结构化字段只写「方法名、HTTP 状态码、耗时、错误类别」，不写 URL 全文 / body / header。
- 崩溃转储：不启用自定义 dump；若启用，须评估凭据内存残留（**已知限制，不声称完全消除**）。
- 现有 `--source-url` 命令行方式会把订阅 URL 暴露在进程命令行（任务管理器可见）；新方案由 API 注入内存，**不再经命令行**，安全性反而提升。手工导入模式仍保留该风险，需在 UI 标注。

---

## 6. 登录态与 UI 信息架构

### 6.1 状态机

```
LoggedOut { site, last_error? }
   │ login(email, password)
   ▼
Authenticating（按钮禁用，防重复提交）
   │ 成功：Secret<AuthData> + 并发拉 info / getSubscribe
   ▼
Authenticated { account: AccountView, subscription: SubscriptionView }
   ├── Refreshing / Degraded（网络失败 → 显示缓存 DTO 与「上次更新于 X」）
   ├── Expired（401 / checkLogin 失败）→ 清 Secret → LoggedOut
   └── LoggedOut（登出 / 改密 / 重置链接 → 清凭据库 + 内存 + 缓存 DTO）
```

- 「记住我」关闭：进程退出即丢失凭据，下次启动进登录页。
- 「记住我」开启：启动先读凭据库 → `auth/check` 或 `checkLogin` 校验 → 有效则直接进主界面（骨架屏），无效则回登录页。
- 网络失败**不影响代理**：已写入快照的订阅继续由内核使用。

### 6.2 登录页

站点（预置，可折叠「高级设置」修改）、邮箱、密码、记住我（默认关闭）、登录按钮；
按 `/passport/comm/config` 动态渲染扩展位：邮箱验证、`is_recaptcha` 的人机验证、服务条款链接；
注册 / 忘记密码 → 系统浏览器打开 `app_url` / `tos_url`。

### 6.3 主界面

- 顶部账户条：邮箱掩码、套餐名、到期倒计时、连接状态；
- 左侧导航：**代理 / 订阅 / 商店 / 账户**（沿用 `theme.rs` 设计 token，中文标签待字体方案）；
- 代理页保留现有 `KERNEL` / `CONNECTIONS & TRAFFIC` / `NODE PROBE` / `EVENTS` / `NODES & BALANCE` 区块，
  新增「连接开关 + 系统代理」区（roadmap P1.2/P1.3）；
- 订阅页：流量条（整数分/字节换算）、到期、重置日、手动刷新、重置订阅链接（二次确认）；
- 商店页：套餐卡片 → 周期 → 优惠券 → 支付方式 → 下单 → 二维码 / 打开浏览器 → 状态轮询 → 完成后自动刷新订阅；
- 账户页：资料、余额/佣金、通知开关、修改密码、登出、清除已保存凭据。

---

## 7. 关键流程

### 7.1 首次登录 → 订阅生效

1. `GET /passport/comm/config`（可缓存）→ 决定登录页字段。
2. `POST /passport/auth/login` → 得到 `Secret<AuthData>`；密码立即清零。
3. 并发 `GET /user/info` + `GET /user/getSubscribe` → 构造脱敏 DTO。
4. 把 `subscribe_url` **作为内存 source 注入既有 `snapshot` 流程**（不落命令行、不进 IPC 帧）。
5. 内容哈希变化才写新快照 → 触发 `reload` → 收到 `applied` 事件 → UI 显示「已同步」。
6. 若「记住我」开启，把 `auth_data` 写入 OS 凭据库。

### 7.2 订阅同步

- 定时刷新（建议默认 5 分钟、可配置）+ 手动刷新 + 支付完成 / 重置链接后即时刷新；
- 复用 `reload::hash_bytes` 去重；`resetSecurity` 返回新 URL 后必须重新注入并重拉；
- 失败只降级为 `Degraded`，不回滚已生效快照。

### 7.3 购买

1. `GET /user/plan/fetch` → 只展示 `show=1` 的套餐，价格按整数分渲染 `¥`；
2. 选周期 → `POST /user/coupon/check`（可选）→ `GET /user/order/getPaymentMethod`；
3. `POST /user/order/save` → `trade_no` → `POST /user/order/checkout`；
4. `type=0`：`qrcode` crate 生成位图 → egui 纹理渲染；`type=1`：系统浏览器打开；
5. 轮询 `GET /user/order/check?trade_no=`（指数退避，上限 5 分钟，可取消）；
6. `status=3` → 触发订阅刷新；`2/4` → 展示终止态。

**资金安全**：客户端只做展示与下单，不缓存支付页、不接触卡号、不代收任何款项；
二维码与 URL 均由面板返回，短时内存持有，不写入日志或快照。

### 7.4 登出 / 改密 / 重置订阅链接

- 登出：尽力调用 `GET /user/logout` → 清凭据库 + `Secret` + 缓存 DTO → 登录页；
  **保持现有订阅与代理可用**（A7），并在 UI 明示「已退出账号，代理仍可用至到期」。
- 改密成功：旧 `auth_data` 立即失效 → 强制回登录页。
- 重置链接成功：旧订阅 URL 失效 → 立即重新注入并 reload，否则会断网。

---

## 8. 与既有契约 / 快照 / reload 的衔接

| 既有机制 | 变化 |
|---|---|
| `c1-ipc-contract.md` v1.1 方法集 | **不新增内核 IPC 方法**；账号态在同进程 `mpsc` 传递，内核契约保持 7 个方法 |
| `reload::Source::File / Url` | 新增来源类别：`Panel`（由 `panel` 模块提供 URL）；`source_kind` 可按契约新增枚举值（向后兼容，新增字段不改 `v`） |
| `snapshot` 原子写 / 5 版 / 回滚 | 完全复用；账号模式与手工导入模式共用同一套快照 |
| `--source` / `--source-url` CLI | 保留为兼容 / 离线入口；账号登录成功后**覆盖**当前 source，登出后不自动回退 |
| `status` / `reload` / 事件推送 | 语义不变；订阅变更仍走 `reload` 的 `applied` / `rejected` 事件 |
| `metrics` / `nodes` / `probe_nodes` / `select_node` | 不变，代理页继续消费 |
| `log_tail` / `diagnostics` | 仍为规划方法；实现时必须把账号相关字段加入脱敏规则 |

---

## 9. 错误、离线与限流

| 场景 | 策略 |
|---|---|
| 网络不可达 / DNS 失败 | `Degraded`：显示缓存 DTO 与「上次更新于 X」；代理不受影响 |
| 401 / `auth/check` 失效 | 清 `Secret` → 登录页，提示重新登录（不自动重试密码） |
| 429 / 5xx / 连接超时 | 指数退避（100 ms → 5 s，沿用 sidecar 现有策略）+ 抖动；不给面板制造异常流量 |
| 字段缺失 / 版本漂移 | DTO 字段全 `Option` + `#[serde(default)]`；未知字段忽略；不因可选字段解析失败而整体失败 |
| 支付状态长时间不变 | 轮询 5 分钟后停止，提示「请在网站确认订单」 |
| 面板返回非标准结构 | `PanelClient` trait 隔离，标记「不兼容」并回退到手工导入模式 |

**离线缓存**：账户/订阅 DTO 可缓存到本地用户配置目录（与快照目录分离），只含脱敏字段，带 TTL；
**绝不缓存** `auth_data`、订阅 URL、付款 URL。

---

## 10. 安全与合规清单（发布前逐项验证）

1. 强制 HTTPS；证书校验**不可关闭**（区别于内核 `--insecure` 的测试用途）。
2. 站点基址预置 + 白名单，防钓鱼与 SSRF；用户改站点需二次确认。
3. `Secret<T>` 不实现 `Debug`/`Display`/`Serialize`，`Drop` 清零；`unsafe_code = "deny"` 不放松（凭据库调用走 safe crate）。
4. 日志过滤器：拦截 `Authorization`、`auth_data`、`password`、`subscribe_url`、`token`、付款 URL。
5. 实机回归：新增**自动化断言**验证凭据不落盘、不进日志、不进 IPC 帧、不进截图产物。
6. 新增依赖（`qrcode`、凭据库 crate、`zeroize`）必须过 `cargo deny` 许可扫描 —— 同时关闭 G1 §8.3 第 3 条。
7. 金额用整数分、时间用 UTC 时间戳；不在 UI 展示完整邮箱或订阅链接。
8. 多用户 ACL：凭据库按 Windows 用户隔离；但 IPC 单连接仍允许所有交互登录用户连内核（既有风险，roadmap P2.6 收紧项保留）。
9. 崩溃转储残留为**已知限制**，文档明示，不声称完全消除。
10. 支付只做展示与跳转，不触碰资金信息。

---

## 11. 对既有进度计划的重排（见 `docs/c1-roadmap-2026-11-12.md`）

| 原计划 | 调整 |
|---|---|
| P1.1 字体 / P1.2 系统代理 / P1.3 托盘 / P1.5 安装器 | 保留，但让位于账号集成的骨架阶段；系统代理与托盘并入主界面「代理」区 |
| P1.4 订阅导入 UI | **升级为账号登录 + 订阅自动同步**（本方案 §7.1），手工导入降级为兼容入口 |
| P3 C2 MVP（自动更新、日志导出、连接诊断、分流规则） | 新增「账号与商店」为 C2 主体；自动更新仍是 **FA 硬前置** |
| P1.6 C1 出口评审 | 出口条件新增「账号链路端到端通过（mock 面板）+ 凭据边界断言」 |
| P2.1 `cargo deny` | **优先级上调**：新依赖引入前必须完成 |
| 新增 | `A0` HTTP 扩展与 `Secret`/凭据抽象；`A1` 登录 + 主界面骨架；`A2` 订阅同步；`A3` 商店与订单；`A4` 账户管理；`A5` 安全与回归 |

**关键路径变化**：账号集成成为 C1 → C2 的主线，安装器与托盘降为并行项；
**FA 冻结时点不变**（所有功能冻结后才跑 24 h）。

---

## 12. 风险表

| 风险 | 影响 | 缓解 |
|---|---|---|
| `auth_data ≈ 密码` 且长期有效 | **高** | 最坏情况设计（§5）；默认不记住；OS 凭据库；改密/登出即清；建议面板侧改可撤销会话 |
| 面板补丁被 `git reset --hard` 覆盖 | **高** | L4 只用标准 API；订阅 URL 由 API 返回，不拼接、不依赖自定义字段 |
| 第三方 API 漂移 / 分支差异 | 中高 | `PanelClient` trait + 全 `Option` DTO + mock 面板服务器单测与实机 |
| 资金流程被客户端介入 | **高** | 只展示与跳转，二维码/URL 短时内存，不缓存、不代收 |
| 凭据经截图 / OCR / 日志 / 转储泄漏 | **高** | `Secret<T>` + 日志过滤 + 密码框防护 + 回归脚本专用假账号；转储残留列为已知限制 |
| 为支付引入 WebView 推翻 D1 | 中 | L3 锁点：原生二维码 + 系统浏览器 |
| 引入 `reqwest` 抬高体积与内存 | 中 | L5 复用自有 HTTP/TLS 栈 |
| 新依赖许可与供应链 | 中 | 发布前 `cargo deny`；MPL-2.0 / CC-BY-SA 声明沿用 README |
| 面板风控 / 限流 | 中 | 退避 + 抖动、缓存配置、按需刷新、ETag（若分支支持） |
| 登录失败/过期导致误判断网 | 中 | 账号态与数据面解耦（L2），缓存订阅继续可用 |
| 中文 UI 字体推高 RSS | 中 | 沿用 UI RSS ≤150 MiB 预算，字体方案单独实测 |

---

## 13. 待用户确认的输入

| # | 输入 | 影响 | 建议默认值 |
|---|---|---|---|
| 1 | 面板版本 / 分支（1.6.0 系 vs wyx2685 等） | `auth_data` 语义、是否有会话管理 / 节点列表接口 | 按 1.6.0 最坏情况设计 |
| 2 | 站点基址是否固定预置、是否允许用户修改 | 登录页字段、防钓鱼策略 | 预置 + 高级设置可改 |
| 3 | 是否内置注册 / 找回密码 | 登录页范围 | 跳转系统浏览器 |
| 4 | 「记住我」默认策略 | 凭据持久化面 | 默认关闭 |
| 5 | 支付是否允许跳转系统浏览器 | L3 可行性 | 允许 |
| 6 | 登出后是否保持代理可用 | 断网体验 | 保持 |
| 7 | 首版是否多账号 | 凭据库复杂度 | 单账号 |
| 8 | 中文 UI 字体 / 审美方案 | P1.1 | 待提供 |
| 9 | 是否需要客户端展示节点列表 | 是否依赖分支特有 `/user/server/fetch` | 首版不做 |
| 10 | 真实账号联调授权 | 端到端验证 | 先用 mock 面板，联调前再授权 |

---

## 14. 明确不做

WebView / Tauri / Electron 壳、面板服务端改动与补丁依赖、代收支付与卡信息处理、
多账号、面板管理端 API、遥测、自有 `v2node://` 订阅格式（首版）、TUN（D2 复核通过后另议）、
在 DIRECT 回环上先做性能优化（ADR 0003 §6）。

---

## 15. 变更记录

| 日期 | 版本 | 内容 |
|---|---|---|
| 2026-11-12 | v0.1 | 需求变更后首版设计草案；核实 v2board 1.6.0 用户端 API 与 `auth_data` 语义；确立 L1–L6 架构锁点；重排 roadmap |
