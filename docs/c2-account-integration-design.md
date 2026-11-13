# C2 设计方案：v2board 账号体系与订阅电商集成（2026-11-12 草案）

> **状态**：设计草案，待用户确认；**不产生生产代码**。
> **触发**：用户临时变更需求 —— 客户端集成 v2board 的账号订阅数据、账号管理、订阅购买，
> 采用「输入账号密码登录 → 进入主界面」的形态（类比 QQ），主界面提供代理功能、
> 订阅信息、订阅购买、账户管理四类功能。
> **不撤销既有决议**：G1 Go、ADR 0001（自研 R2）、ADR 0003（维持自研）。
> GUI 框架决议已由 **ADR 0005** 更新：windows-reactor 原生 WinUI 取代 egui（D1 被新需求与 spike 实测重估）。
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

### 1.3 默认假设（2026-11-12 用户已按默认确认，见 §13）

| 编号 | 假设 |
|---|---|
| A1 | 面板基址与订阅路径**可配置、构建时预置**，用户可在「高级设置」修改；首版不自动发现 |
| A2 | 首版单账号、单站点 |
| A3 | 登录方式为邮箱 + 密码；注册 / 找回密码**跳转系统浏览器**，不在客户端内实现（但保留接口扩展位） |
| A4 | 「记住我」默认**关闭**；开启时只持久化**密码**（OS 凭据库），不持久化 JWT（生产 JWT 仅 24 h 有效，见 §2.1） |
| A5 | 支付二维码由原生 UI 渲染（WinUI Image 控件）；需要网页支付时跳转系统浏览器 |
| A6 | `auth_data` 按 bearer 凭据从严处理：不落日志 / UI / IPC，可一键清除（生产实测为 24 h 可撤销 JWT，好于 v0.1 最坏假设） |
| A7 | 登出后已缓存的订阅继续可用至过期，避免「退出登录即断网」 |
| A8 | 中文 UI 沿用现有深色主题 + 开源中文字体（思源黑体），不做品牌定制 |

---

## 2. 事实基线（2026-11-12 生产源码一手盘点，取代 v0.1 的公开文档假设）

> 核实方式：只读 SSH 读取生产部署源码（Laravel 路由 / 控制器 / 中间件 / `AuthService`）与 DB schema，
> 未调用任何生产账号 / 订单 / 支付接口，未修改任何生产状态。
> 原始输出存仓库外 `_c0d_test/panel-inventory-raw*.txt`（不入库）；
> 站点域名、订阅路径值、token 等敏感值一律不写入本文档。

### 2.1 部署与认证机制（决定性约束）

| 项 | 生产事实 |
|---|---|
| 部署一致性 | 部署代码与面板 git HEAD 内容一致（115 个文件仅 mode 变更、0 行内容 diff）；流量推送幂等补丁已入 HEAD；`update.sh` 的 `git reset --hard` 仍会覆盖未入库改动 |
| 框架 | Laravel 13.39 / PHP 8.5.12 / `firebase/php-jwt` ^7.0（深度二开分支，**非** 1.6.0 系） |
| `auth_data` | **HS256 JWT**：`{id, session, iat, exp}`，密钥为面板 `app.key`；普通用户 TTL **24 h**，管理员/员工 30 min |
| 会话模型 | **可撤销**：`USER_SESSIONS` 缓存登记 session；`logout` / `changePassword` / `forget` 清除全部会话；`getActiveSession` / `removeActiveSession` 可管理单会话 |
| 校验缓存 | JWT 校验结果缓存 ≤900 s → 封禁/撤销最长 15 min 传播延迟 |
| Web cookie 会话 | 仅对 Origin/Referer 白名单内的浏览器请求生效；原生客户端不带 Origin，走 JWT 路径 |
| 凭据传递 | `Authorization` 头**裸 JWT**（无 `Bearer ` 前缀），或 `auth_data` 请求参数 |
| 登录响应 | `{data: {token, is_admin, auth_data}}`；`token` 是订阅 token，不是会话凭据 |
| 错误口径 | 业务错误 HTTP 500 + `{message}`；鉴权失败/封禁 403；登录密码错误限流 429（默认 5 次 / 60 min，按邮箱计）；参数校验 422 |

**结论**：`auth_data` 不是 v0.1 假设的 `base64(email:password)` 等价密码，而是 24 h 可撤销 bearer 凭据，
安全画像显著好于最坏情况；但 L1–L8 锁点全部保留（泄露的 JWT 在有效期内仍可完全冒用账号）。
**24 h 过期是「记住我」的决定性约束**：持久化必须存密码（OS 凭据库）并在启动/过期时静默重登，
只存 `auth_data` 的方案每天必然失效一次（见 §6.1）。

### 2.2 接口清单（生产路由逐一核对，前缀 `/api/v1`）

| 功能 | 方法 | 路径 | 响应要点（`data` 内） |
|---|---|---|---|
| 站点配置（未登录） | GET | `/guest/comm/config` | `tos_url`、`app_url`、`is_email_verify`、`is_invite_force`、`is_recaptcha`、`recaptcha_site_key`、`logo`、`email_whitelist_suffix`。**不存在 `/passport/comm/config`** |
| 站点配置（登录后） | GET | `/user/comm/config` | `currency`、`currency_symbol`、`stripe_pk`、提现/佣金配置 |
| 登录 | POST | `/passport/auth/login` | `email`（strict 校验）+ `password`（≥8 位）→ `{token, is_admin, auth_data}` |
| 登录校验 | GET | `/user/checkLogin` | `{is_login, is_admin?}`（替代 v0.1 假设的 `/passport/auth/check`） |
| 登出 | **POST** | `/user/logout` | `true`，**清除该用户全部会话** |
| 会话管理 | GET / POST | `/user/getActiveSession`、`/user/removeActiveSession` | 会话列表（ip / login_at / ua）；按 `session_id` 撤销 |
| 账户信息 | GET | `/user/info` | `email`、`transfer_enable`、`u`、`d`、`expired_at`、`balance`、`commission_balance`、`plan_id`、`banned`、`avatar_url`、`device_limit`、`auto_renewal`、`remind_expire`、`remind_traffic`、`discount`、`commission_rate`、`telegram_id`、`uuid`、`last_login_at`、`created_at` |
| 订阅信息 | GET | `/user/getSubscribe` | `plan_id`、`token`、**`subscribe_url`**、`u`、`d`、`transfer_enable`、`expired_at`、`reset_day`、`plan`（完整套餐对象）、`alive_ip`（在线设备数）、`allow_new_period`、`device_limit`、`email`、`uuid` |
| 重置订阅链接 | GET | `/user/resetSecurity` | **裸字符串** = 新 `subscribe_url`（旧链接立即失效） |
| 待办统计 | GET | `/user/getStat` | `[待付订单数, 工单数, 邀请人数]` |
| 修改密码 | POST | `/user/changePassword` | `old_password` + `new_password` → `true`，**清除全部会话**（改密后必须重登） |
| 通知/续费设置 | POST | `/user/update` | 仅接受 `auto_renewal` / `remind_expire` / `remind_traffic` |
| 套餐商店 | GET | `/user/plan/fetch` | `show=1` 按 `sort` 排序；`id`、`name`、`content`、各周期价格、`transfer_enable`、`renew`、`device_limit`、`speed_limit`、`capacity_limit`（已扣减在售名额）；可选 `id` 查单套餐 |
| 支付方式 | GET | `/user/order/getPaymentMethod` | `id`、`name`、`payment`、`icon`、`handling_fee_fixed`、`handling_fee_percent`（仅 `enable=1`） |
| 优惠券校验 | POST | `/user/coupon/check` | `code` + `plan_id` → 优惠券对象 |
| 创建订单 | POST | `/user/order/save` | `plan_id` + `period` → `trade_no`；**同一用户同时只允许一个未完成订单**；余额自动折抵；`plan_id=0` 为充值单（首版不做） |
| 结算订单 | POST | `/user/order/checkout` | `trade_no` + `method` → `{type, data}`：**0=二维码 / 1=URL / -1=余额已全额支付**；手续费在此步计入 |
| 订单状态 | GET | `/user/order/check?trade_no=` | 裸整数：**0 待支付 / 1 已支付 / 2 已取消 / 3 已完成 / 4 处理中** |
| 订单列表 / 详情 / 取消 | GET/POST | `/user/order/fetch`、`/user/order/detail`、`/user/order/cancel` | 详情是 `detail`（非 v0.1 的 `details`）；仅待支付可取消 |
| 节点列表 | GET | `/user/server/fetch` | 存在；首版不做（§13 决议 9） |

**周期枚举**（`order/save` 的 `period`）：`month_price` / `quarter_price` / `half_year_price` / `year_price` /
`two_year_price` / `three_year_price` / `onetime_price` / `reset_price`（流量重置包，需有效订阅）。

**金额口径**：`balance` / 各 `*_price` / `total_amount` 均为**整数分**。DTO 层保持整数，UI 层按
`/user/comm/config` 的 `currency_symbol` 格式化，禁止浮点中间量。

**流量口径**：`u` / `d` / `transfer_enable` 为字节整数；`expired_at` 为 Unix 秒，UI 本地化展示。

### 2.3 本地环境事实

- 生产订阅路径为**自定义值**（非源码默认 `/api/v1/client/subscribe`），订阅 URL 必须由 `getSubscribe` /
  `resetSecurity` 返回，不能客户端拼接。
- **订阅基址是多值逗号分隔列表，每次调用随机取一个** → 同一账号连续两次拿到的 `subscribe_url`
  可能域名不同而内容相同。订阅同步必须按**拉取内容哈希**去重（复用 `reload::hash_bytes`），
  不能按 URL 字符串判等，否则会反复写快照 / 触发 reload。
- `show_subscribe_method=0`（普通 token 模式）；若面板切到 1/2（一次性 / TOTP token），订阅 URL
  会短时失效，客户端按「拉取失败 → 重新 `getSubscribe` 取新 URL」降级处理。
- 已启用支付渠道：**AlipayF2F（支付宝二维码，type=0）** 与 **EPay（易支付，type=0/1）**；
  渠道随面板配置变化，客户端以 `getPaymentMethod` 返回为准，不硬编码。
- 生产开关现值：`email_verify=1`（注册需邮箱验证码）、`recaptcha_enable=0`、`invite_force=0`、
  `stop_register=0`、`password_limit_enable=1`（5 次 / 60 min）、`currency=CNY`。
  开关可变，登录页按 `/guest/comm/config` 动态渲染，不硬编码。
- 面板存在自定义补丁且 `update.sh` 的 `git reset --hard` 会覆盖未入库改动 → 客户端不得依赖任何
  非标准接口或补丁字段（L4）。本清单核对的是当前部署，面板升级后需按 A5 回归复验。
- 现有 `client_sidecar::fetch` 是最小 HTTP/1.1 客户端：只支持 `GET` + `200` + `Content-Length`/chunked；
  已具备 `rustls` + `tokio-rustls` + `rustls-native-certs`，**TLS 能力可复用**。A0 需扩展
  POST / JSON / 自定义头 / 非 200 状态码与 `{message}` 错误体解析。

---

## 3. 架构锁点（详见 `docs/adr/0004-account-integration-boundaries.md`）

| 编号 | 锁点 | 理由 |
|---|---|---|
| L1 | **凭据不出控制面**：密码 / `auth_data` / 订阅 URL 只在 `client-sidecar` 的 `panel` 与 `credential` 模块内以 `Secret<T>` 持有；GUI 只拿脱敏 DTO | `auth_data` 是 24 h 内可完全冒用账号的 bearer 凭据；UI 层一旦接触，截图、OCR、日志、崩溃转储都会成为泄露面 |
| L2 | **内核服务完全不碰账号**：`client-core`（session 0）只接收订阅内容变更 → 快照 → reload | 账号态与数据面解耦；服务进程无用户凭据，减少提权进程的风险面 |
| L3 | **不引入 WebView**：二维码用纯 Rust `qrcode` 生成位图在原生 UI 渲染（ADR 0005 后为 WinUI Image 控件）；URL 用系统浏览器 | 无 JS 层，凭据边界为纯模块边界；WebView 会引入 devtools/脚本注入泄露面 |
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
| 密码 | 一次登录请求体；「记住我」时跨会话 | 内存，提交后立即清零；「记住我」时存 **OS 凭据库**（24 h JWT 过期后静默重登的唯一依据） | 密码框（不回显、禁复制） | **禁止** |
| `auth_data`（24 h 可撤销 JWT） | 会话；过期或 403 即失效 | 控制面内存；**不持久化**（持久化密码代替） | **禁止** | **禁止**；`Debug` 输出 `[REDACTED]` |
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
- 「记住我」开启：OS 凭据库存**密码**（不存 JWT）。启动先读凭据库 → 静默 `login` 换新 JWT →
  成功直接进主界面（骨架屏）；密码错误 / 429 限流则回登录页并提示。
  运行中 JWT 到期（24 h）或收到 403：有保存密码则静默重登一次，失败才回登录页。
- 网络失败**不影响代理**：已写入快照的订阅继续由内核使用。

### 6.2 登录页

站点（预置，可折叠「高级设置」修改）、邮箱、密码、记住我（默认关闭）、登录按钮；
按 `/guest/comm/config` 动态渲染扩展位：邮箱验证、`is_recaptcha` 的人机验证、服务条款链接；
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

1. `GET /guest/comm/config`（可缓存）→ 决定登录页字段。
2. `POST /passport/auth/login` → 得到 `Secret<AuthData>`；密码立即清零。
3. 并发 `GET /user/info` + `GET /user/getSubscribe` → 构造脱敏 DTO。
4. 把 `subscribe_url` **作为内存 source 注入既有 `snapshot` 流程**（不落命令行、不进 IPC 帧）。
5. 内容哈希变化才写新快照 → 触发 `reload` → 收到 `applied` 事件 → UI 显示「已同步」。
6. 若「记住我」开启，把**密码**写入 OS 凭据库（JWT 24 h 过期，存 `auth_data` 无法跨天免登）。

### 7.2 订阅同步

- 定时刷新（建议默认 5 分钟、可配置）+ 手动刷新 + 支付完成 / 重置链接后即时刷新；
- 复用 `reload::hash_bytes` 去重；`resetSecurity` 返回新 URL 后必须重新注入并重拉；
- 失败只降级为 `Degraded`，不回滚已生效快照。

### 7.3 购买

1. `GET /user/plan/fetch` → 只展示 `show=1` 的套餐，价格按整数分渲染 `¥`；
2. 选周期 → `POST /user/coupon/check`（可选）→ `GET /user/order/getPaymentMethod`；
3. `POST /user/order/save` → `trade_no` → `POST /user/order/checkout`；
4. `type=0`：`qrcode` crate 生成位图 → 原生 Image 控件渲染；`type=1`：系统浏览器打开；
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
| `log_tail` / `diagnostics` | ✅ 已实现（2026-11-13，契约 v1.2）。`log_tail` 只读**内核**日志（L2：内核不产生账号数据），账号字段不入脱敏面；面板秘密由 GUI 导出文件的零命中断言守护（`verify_export.ps1`） |

---

## 9. 错误、离线与限流

| 场景 | 策略 |
|---|---|
| 网络不可达 / DNS 失败 | `Degraded`：显示缓存 DTO 与「上次更新于 X」；代理不受影响 |
| 403 / JWT 过期或会话被撤销 | 有保存密码则静默重登一次；失败清 `Secret` → 登录页（不循环重试密码） |
| 429（登录限流，默认 5 次错 / 60 min） | 显示锁定提示，**不自动重试**；其它接口 429 / 5xx / 超时走指数退避（100 ms → 5 s + 抖动） |
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
| `auth_data` 泄露（24 h 内可完全冒用账号） | **高** | §5 边界；默认不记住；记住我存密码于 OS 凭据库；改密/登出即清全部会话；JWT 不入 UI/日志/IPC |
| 「记住我」密码持久化 | **高** | 仅 OS 凭据库（DPAPI、按用户隔离）；默认关闭；一键清除；静默重登失败即回登录页 |
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

## 13. 用户确认输入（2026-11-12 全部按建议默认确认）

| # | 输入 | 决议 |
|---|---|---|
| 1 | 面板版本 / 分支 | **已由生产源码一手盘点确定**：深度二开分支，Laravel 13.39 + JWT 会话（§2.1），不再是待确认项 |
| 2 | 站点基址是否固定预置、是否允许用户修改 | 预置 + 高级设置可改（改站点二次确认，防钓鱼） |
| 3 | 是否内置注册 / 找回密码 | 跳转系统浏览器 |
| 4 | 「记住我」默认策略 | 默认关闭；开启时只存密码于 OS 凭据库（§6.1） |
| 5 | 支付是否允许跳转系统浏览器 | 允许 |
| 6 | 登出后是否保持代理可用 | 保持（已生效快照继续用至过期） |
| 7 | 首版是否多账号 | 单账号 |
| 8 | 中文 UI 字体 / 审美方案 | 沿用现有深色主题 + 思源黑体（开源可分发），不做品牌定制；RSS ≤150 MiB 预算内实测 |
| 9 | 是否需要客户端展示节点列表 | 首版不做（`/user/server/fetch` 存在但不依赖） |
| 10 | 真实账号联调授权 | 先用 mock 面板；真实账号联调前另行单独授权 |

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
| 2026-11-12 | v0.2 | 生产源码一手盘点取代公开文档假设：`auth_data` 实为 24 h 可撤销 JWT；接口表逐路由核对（guest/comm/config、POST logout、order/detail、订单状态语义、checkout type=-1）；订阅基址多值随机轮换 → 按内容哈希去重；「记住我」改存密码；§13 十项输入用户按默认确认 |
