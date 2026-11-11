//! IPC 探针：连接内核管道，依次调用 status / reload / 未知方法。
//!
//! 用法：cargo run -p client-sidecar --example ipc_probe -- '\\.\pipe\name'

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use client_sidecar::IpcTransport;
    use client_sidecar::ipc::pipe::PipeClient;
    use serde_json::json;

    let name = std::env::args()
        .nth(1)
        .ok_or("usage: ipc_probe <pipe-name>")?;
    let client = PipeClient::connect(&name).await?;

    let status = client.request("status", json!({"detail": true})).await?;
    println!("status={}", serde_json::to_string(&status)?);

    let reload = client.request("reload", json!({})).await?;
    println!("reload={}", serde_json::to_string(&reload)?);

    match client.request("no_such_method", json!({})).await {
        Ok(value) => println!("unknown=unexpected_ok:{value}"),
        Err(error) => println!("unknown_error={error}"),
    }

    client.close().await;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ipc_probe requires Windows (named pipes)");
}
