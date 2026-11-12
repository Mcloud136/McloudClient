//! 设计 token：颜色与间距集中在此，后续 GUI 审美输入只改这一个文件。
//!
//! 当前为深色主题（网络工具惯例，与 egui `Visuals::dark` 配套）；
//! 标签使用默认拉丁字体（无 CJK），中文 UI 需先做字体决策（系统字体加载
//! 会显著增加 RSS，或内嵌子集字体增加体积）。

use eframe::egui::Color32;

/// 主色：标题强调、交互高亮。
pub const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x8C, 0xFF);
/// 正常态：已连接、探测可达。
pub const OK: Color32 = Color32::from_rgb(0x35, 0xA0, 0x5A);
/// 过渡态：连接中 / 退避重试。
pub const WARN: Color32 = Color32::from_rgb(0xC8, 0x8A, 0x1E);
/// 异常态：断连、探测失败。
pub const ERR: Color32 = Color32::from_rgb(0xD0, 0x5A, 0x5A);
/// 次级文本：页脚统计、提示语。
pub const DIM: Color32 = Color32::from_rgb(0x90, 0x90, 0x9A);

/// 区块间距。
pub const SPACING: f32 = 8.0;
