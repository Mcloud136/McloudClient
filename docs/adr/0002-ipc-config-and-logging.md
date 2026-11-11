# ADR 0002：IPC、配置所有权与日志归属

- **状态**：已接受（2026-11-12）
- **上游**：`0001-sidecar-process-boundary.md`（A1 自研内核 + B3 双进程 + Windows 服务）
- **决策人**：用户

## 1. IPC：命名管道 + JSON 帧，抽象为可替换 trait

**决定**：控制面与状态通道使用 Windows 命名管道，消息为「4 字节长度前缀 + UTF-8 JSON」。传输层抽象为 trait，业务层不得直接依赖管道 API。

**理由**：

1. 服务（session 0）与 UI（session 1+）跨会话通信，命名管道可用 `PipeSecurity` 声明式表达 ACL；共享内存需要全局命名对象 + 手写 DACL，且残留环需自行清理。
2. 控制消息频率为「每次用户操作一条」，管道 20–80 μs 的往返对用户不可感知。
3. 可调试性：管道内容可被工具抓取，JSON 可读。

**协议约束**：

| 项 | 约定 |
|---|---|
| 帧格式 | `u32` 大端长度 + JSON body（避免裸 JSON lines 的分帧歧义） |
| 版本 | 每条消息带 `v` 字段；不兼容变更递增主版本，旧版本拒绝并给出错误码 |
| 请求/响应 | `id` 关联；超时 3 s；服务端幂等 |
| 事件推送 | 单向，无 `id`；有界队列，满时丢弃最旧并计数 |
| 安全 | `PipeSecurity` 为保护型 DACL，允许 `SYSTEM` + `INTERACTIVE` + 当前用户；禁用匿名访问 |

### 1.1 Spike 结论（2026-11-12，风险 R1 已关闭）

| 项 | 结果 |
|---|---|
| API | tokio 1.54.1 `ServerOptions::create_with_security_attributes_raw(addr, *mut c_void)` |
| SDDL | `D:P(A;;GA;;;SY)(A;;GA;;;<current_user_sid>)` |
| 实测生效 DACL | `D:P(A;;FA;;;SY)(A;;FA;;;LA)` |
| 客户端往返 | 同用户连接并收发成功 |
| 已知限制 | 本地管理员账户的 SID 被规范化为 `LA`（本地管理员组），无法精确到单个管理员用户；标准用户保留原 SID |
| 待验证 | 服务（session 0）↔ 用户（session 1）跨会话，留待 S3 服务封装后实测 |

结论：采用方案 A（自定义 SDDL + `create_with_security_attributes_raw`），无需回退到默认 ACL。

**跨会话修订（2026-11-12）**：服务进程运行在 session 0 且以 SYSTEM 身份注册管道，`for_current_user()` 取到的 SID 就是 SYSTEM，仅 `SY + 当前用户` 会让用户会话无法连接。SDDL 调整为 `D:P(A;;GA;;;SY)(A;;GA;;;IU)(A;;GA;;;<sid>)`。`IU`（INTERACTIVE）覆盖交互登录用户，后台服务不在该组内，范围仍窄于 `Authenticated Users`。多用户机器如需收紧到单一用户，应在安装时把目标 SID 写入服务配置并在启动时读取。

**替换触发条件（写入验收标准）**：
M1 完成后做本机微基准，若满足以下任一条件，则启动共享内存环替换（限指标通道，控制面仍走管道）：

- 控制命令 RTT 的 P99 > 5 ms（不含首次建连）；
- 状态推送端到端延迟 P99 > 50 ms；
- sidecar 空闲 CPU 占用 > 0.5%（单核折算）。

未触发则保持管道实现，不为"可能的性能问题"预付复杂度。

## 2. 配置所有权：sidecar 推快照

**决定**：订阅由 sidecar 拉取与校验，内核只读快照文件，不持有订阅凭据。

流程：

```
sidecar: 拉取订阅 → schema/规则校验 → 写 snapshots/<seq>.json + 更新 current
        → IPC: {cmd:"reload", version:<seq>}
内核   : 读取 current → 解析 → 构建 router → 原子替换 → IPC: {evt:"applied", version:<seq>}
失败   : 内核保留旧 router 并回 {evt:"rejected", reason}; sidecar 记录并退避重试
```

约束：

- 快照目录 `%ProgramData%\McloudClient\snapshots\`，服务账户可写、用户只读；
- **原子写**：`<seq>.json.tmp` → `fsync` → `rename`；`current.txt` 单独原子替换；
- 保留最近 5 个版本，支持一键回滚到上一版本；
- **引导快照**：安装包内置最小快照（仅 `DIRECT`）；内核启动时若无 `current` 则回退到它，仍无则以退出码 2 结束，由服务重启并上报，不允许空配置启动；
- **失败分类**：`fetch_failed` / `validate_failed` / `kernel_rejected` 三态分开上报，指数退避重试；拉取失败不阻塞数据面；
- sidecar 崩溃不影响内核：内核已加载的 router 与快照均在本地。

## 3. 日志归属：内核写环形文件

**决定**：内核以文件为日志权威来源，`sidecar` 只读展示与导出。

| 项 | 约定 |
|---|---|
| 路径 | `%ProgramData%\McloudClient\logs\core.log`（`--log-dir` 可覆盖；命名见下方修订） |
| 滚动 | 按大小 10 MB，保留 5 个文件（`core.log`、`core.1.log` … `core.4.log`，上限 50 MB） |
| 写入方式 | `tracing-appender` 非阻塞 + `WorkerGuard`；默认 `info`，`RUST_LOG` 可覆盖；控制台与文件双写 |
| 级别 | 默认 `info`；`debug` 需显式开启且自动在 30 min 后回落 |
| 采集 | sidecar 只读文件，做展示/导出；不上传 |

补充：仅「崩溃前状态快照」等关键事件通过 IPC 推送，避免内核崩溃时丢失最后的上下文。日志正文不走 IPC，避免背压阻塞数据面。

### 3.1 实现修订（2026-11-12）

- **命名**：原文为 `core.YYYY-MM-DD.log`，实现改为 `core.log` + 序号轮转。理由：按大小滚动时序号更直观，且日期命名无法约束单日体积上限（一天内仍可能写满磁盘）。「10 MB × 5」的体积约束不变。
- **轮转**：`RollingWriter` 在写入前检查剩余空间，超限即关闭当前文件、后移历史文件并删除最旧一份。
- **panic flush**：`WorkerGuard` 在进程正常退出或 unwind 时 drop 并 flush；未单独注册 panic hook。
- **默认级别**：`info`。服务模式下 stdout 不可见，若沿用 `EnvFilter` 的 ERROR 默认值，启动与停止事件会全部丢失。

## 4. 可替换性设计（IPC trait）

```rust
/// 传输层抽象；业务层只依赖此 trait，不感知管道或共享内存。
#[async_trait]
pub trait IpcTransport: Send + Sync {
    /// 发送请求并等待响应，超时由实现方保证。
    async fn request(&self, method: &str, payload: serde_json::Value) -> Result<Reply, IpcError>;
    /// 订阅服务端事件流；有界队列，溢出时丢弃最旧并计数。
    fn events(&self) -> EventStream;
}
```

- 实现：`NamedPipeTransport`（M1）、`SharedMemoryTransport`（仅在触发条件满足后）。
- 约束：协议编解码独立于传输；新增传输不得修改消息结构。
- 测试：trait 层提供契约测试，两种实现共用同一套用例。

## 5. 验证计划

| 项 | 方法 | 通过标准 |
|---|---|---|
| 管道 RTT | 本机微基准：1000 次 request/response | P99 ≤ 5 ms |
| 事件延迟 | 内核推送 → sidecar 收到的时间戳差 | P99 ≤ 50 ms |
| 空闲开销 | 5 min 空闲的进程 CPU 时间 | ≤ 0.5% 单核 |
| 跨会话可用 | 服务（session 0）↔ 用户进程（session 1）实测 | 双向收发成功 |
| 崩溃取证 | `kill` 内核后检查日志文件尾部 | 含 panic 前最后一条 |

未达标的处理顺序：先优化帧大小与批量合并，仍不达标再按 §1 的触发条件替换指标通道。
