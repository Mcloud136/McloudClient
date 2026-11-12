//! 常驻 mock 面板（开发/实机联调用）：打印基址后一直服务。
//!
//! ```text
//! cargo run -p client-sidecar --example mock_panel_serve
//! ```

use client_sidecar::mock_panel::MockPanel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let panel = MockPanel::start().await?;
    println!("mock panel listening at {}", panel.base_url());
    println!("login: any email with @ + password >= 8 chars");
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
