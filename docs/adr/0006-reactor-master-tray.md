# ADR 0006：GUI 迁移 windows-reactor master（git rev pin），启用原生托盘

- 日期：2026-11-13
- 状态：已接受（用户裁决「迁 master 拿完整托盘」）
- 关联：ADR 0005（windows-reactor 取代 egui）、ADR 0004（L1/L3 边界）

## 背景

ADR 0005 pin 的发布版 `windows-reactor 0.100.0` 无托盘 API、无窗口隐藏/显示
（`WindowRef` 仅 `request_close`/`request_focus`），C1 出口项 P1.3（托盘/最小化）被阻塞。
用户质询：reactor 本质是 WinUI 封装，能否经 WinUI interop 另辟托盘/隐藏路径？

## 核实（一手证据）

- WinUI3 本身无托盘 API；Windows 托盘 = Win32 `Shell_NotifyIcon`。
- 0.100 与 master 均**不公开** Window/HWND/AppWindow 句柄（内部 `pub(crate)`）；
  「interop freely with XAML」指 WinUI 生态层，不等于 0.100 公开 API 暴露句柄。
- 0.100 侧路可行但 hack：UIA 自查 HWND → `AppWindow.GetFromWindowId` → `Hide/Show`
  （windows-rs safe WinRT）+ tray-icon crate；稳定性劣于原生 API，作 fallback。
- master（crates.io 未发布，唯一发布版仍 0.100.0）提供原生
  `ApplicationView::notify_icon(key, NotifyIcon::new(ico).tooltip().on_activate().menu())`
  + `ApplicationContext::show_window/exit`；官方 notifyicon 样例与 tray 测试在案。
- master 无 `hide_window`：最小化到托盘 = 窗口关闭（Component Drop → closed 回调）后
  从 ApplicationView 移除窗口声明（状态保留在 Application 层），托盘点击 `show_window` 重建。

## 决定

1. `windows-reactor` 改 git rev pin `8ca69fdaa9d69c78b9bcfbee96c6916f2282e7f0`（master HEAD）；
   `deny.toml` allow-git 增加 microsoft/windows-rs。升级需重新评估 API 变动并重跑回归。
2. 架构迁 `Application`/`ApplicationView` 双层：`TrayApp` 持全部会话状态 + 托盘 + 窗口生命周期；
   `MainWindow`(Component) 负责视图与异步 spawn（面板/同步/轮询），消息经 `forward()` 回 Application。
   窗口输入为脱敏 ViewModel + 凭据 Arc（手动 PartialEq 按视图模型 reconciliate）。
3. 关闭主窗口 = 隐藏到托盘；托盘左键/菜单「打开主窗口」恢复；菜单「退出」真退出。
4. 图标 `crates/client-gui/assets/icon.ico` 随安装器分发（`$INSTDIR\assets\`），
   运行时按 exe 目录定位（不用编译期路径）。
5. 凭据边界不变（L1/L3）：Secret 不进 ViewModel；托盘/菜单不携带凭据。

## 后果

- 正面：原生托盘 + 最小化到托盘 + 菜单，C1 P1.3 解锁；无 hack 侧路。
- 负面：依赖未发布预览（rev pin 缓解）；git 依赖克隆慢（本机 git 通道需代理 +
  `CARGO_NET_GIT_FETCH_WITH_CLI=true`）；CI 需能克隆 github（ubuntu/windows runner 直连可用）。
- 回退条件：master API破坏性变动无法迁移或克隆不可靠时，回 0.100 + UIA/AppWindow 侧路或暂缓托盘。