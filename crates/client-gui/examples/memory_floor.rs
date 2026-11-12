//! 内存地板测量：不带任何业务逻辑的最小 eframe 窗口。
//!
//! 用途：把「egui + glow 框架在本机的常驻内存」与「业务代码/IPC 的增量」
//! 分开计量（预研 §5 D1/D3 的验证方法）。
//!
//! ```text
//! cargo build --release -p client-gui --example memory_floor
//! target/release/examples/memory_floor.exe
//! ```
//!
//! 启动后保持窗口 5 s 再读 RSS / Private Bytes 即为框架地板。

use eframe::egui::{self, CentralPanel};

fn main() -> eframe::Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("egui-memory-floor")
            .with_inner_size([460.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "egui-memory-floor",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            Ok(Box::new(Floor {
                frames: u64::default(),
            }))
        }),
    )
}

/// 只画一行计数器，维持与面板同频的重绘节奏（1 Hz）。
struct Floor {
    frames: u64,
}

impl eframe::App for Floor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        CentralPanel::default().show(ui, |ui| {
            ui.label(format!("frames: {}", self.frames));
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// 退出时打印 GL 渲染器，用于诊断软件回退（WARP 等）导致的内存放大。
    ///
    /// `glow::HasContext::get_parameter_string` 是 unsafe 接口，
    /// 仅在此诊断示例中局部放行。
    #[allow(unsafe_code)]
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(gl) = gl {
            use eframe::glow::HasContext;
            unsafe {
                let renderer = gl.get_parameter_string(eframe::glow::RENDERER);
                let version = gl.get_parameter_string(eframe::glow::VERSION);
                eprintln!("GL_RENDERER={renderer}");
                eprintln!("GL_VERSION={version}");
            }
        }
    }
}
