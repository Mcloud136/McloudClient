# ADR 0005：GUI 路线改用 windows-reactor（原生 WinUI），取代 egui

- 日期：2026-11-12
- 状态：已接受（用户授权「按你认为最优解来推进」，spike 数据定案）
- 关联：ADR 0004（账号集成边界）、`docs/c1-gui-tun-prestudy.md` §9/§11（D1）、`docs/c1-roadmap-2026-11-12.md`

## 背景

D1（2026-11-12 17:44）裁决维持 egui + glow，依据是「低内存优先」与当时公开/实测对比。
18:00 需求变更为账号登录 + 四区主界面（表单密集型消费级 UI），决策前提发生变化：
egui 需手写密码框/表单/中文输入法验证，而该裁决做出时账号需求尚未进入。
用户随后要求重估 WebView 路线，并授权按最优解推进（可改可重建）。

## 新证据（spike 实测，2026-11-12 晚，本机 release，整进程树口径）

| 指标 | Tauri 2.12 | windows-reactor 0.100 | egui 0.37（既有） |
|---|---|---|---|
| 空闲 RSS | 346 MiB（7 进程） | 119 MiB（1 进程） | ~124 MiB |
| 空闲 CPU（整机 32 核） | 0.002% | 0.000% | ≤0.833%（可见窗口） |
| 启动到可见窗口 | 0.06 s | 0.26 s | 快 |
| exe 体积 | 7.6 MB | 2.4 MB | 9.8 MB |
| ≤150 MiB 预算 | 不达标 | 达标 | 达标 |

- 预研 §11 的「Tauri 142 MB」**非整进程树口径**；WebView2 渲染/GPU 子进程另计约 200 MiB。
  整树口径才是公平判据，本 ADR 以整树实测为准。
- 两个 spike 均实现登录页 + 四区骨架并截图验证渲染（含中文），产物在
  仓库外 `_gui_spike/` 目录：`tauri.png` / `reactor.png`、`measure_gui.py`。
- Electron 未进 spike：体积/内存/node 工具链/Rust 隔离成 sidecar 全踩约束，无胜出维度。

## 决定

1. **client-gui 改用 `windows-reactor = "=0.100.0"`**（精确 pin，预览期 API 会变）。
   原生 WinUI 控件、Elm 风格组件模型、纯 cargo 构建（无 XAML/C#/node）。
2. **不引入 WebView**：ADR 0004 L3 的精神保留且更强——无 JS 层，凭据边界为纯模块边界，
   devtools/脚本注入泄露面不存在；支付二维码仍用纯 Rust `qrcode` 位图 + 原生 Image 控件。
3. **Windows-only GUI**：`client-gui` 以 `#[cfg(windows)]` 门控真实实现；
   非 Windows 编译桩（打印「GUI 仅支持 Windows」并退出），保证双平台 CI 可编译。
   内核/sidecar 跨平台不变。
4. **运行时依赖**：用户机需 Windows App Runtime（Win10 1809+；本机已装 1.8/2.x）。
   C3 打包阶段用 `windows-reactor-setup` 提供自包含选项。
5. egui 实现整体替换（用户授权推倒重做）；既有 IPC 后台任务与内核面板能力迁移保留，
   不丢失已验证功能（s10/s11b/s12 回归需按新 UI 重做断言）。
6. UI 内存预算 **RSS ≤150 MiB 维持**（整进程树口径，绑定本机 GPU/驱动，换机型重校准）。

## 后果

- 正面：内存达标、二进制最小、中文/IME/Fluent 原生零成本、凭据泄露面最小、
  表单类 UI 开发成本低于 egui。
- 负面：预览期 API 风险（pin + 升级需回归）；生态年轻（crates 下载量 ~8k）；
  Linux 无 GUI（桩）；GUI 自动化回归需改用 WinUI 控件树/截图断言（s11b 的
  AttachThreadInput 前台修复经验仍适用）。
- 重新评估条件：windows-reactor 停止维护、API 破坏性变更无法迁移、
  或实测 RSS 超预算且无法优化时，回退候选为 egui（spike 与回归资产保留）。