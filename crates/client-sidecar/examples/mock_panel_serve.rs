//! 常驻 mock 面板（开发/实机联调用）：打印基址后一直服务。
//!
//! ```text
//! cargo run -p client-sidecar --example mock_panel_serve -- --port 19099
//! ```

use client_sidecar::mock_panel::MockPanel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args()
        .position(|a| a == "--port")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let panel = MockPanel::start_on(port).await?;
    if let Some(i) = std::env::args().position(|a| a == "--installer")
        && let Some(path) = std::env::args().nth(i + 1)
    {
        panel.set_installer(std::path::Path::new(&path))?;
    }
    println!("mock panel listening at {}", panel.base_url());
    println!("login: any email with @ + password 12345678");
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
