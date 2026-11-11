//! IPC 往返基准：连接内核管道，连续调用 status 并输出分位延迟。
//!
//! 用法：cargo run -p client-sidecar --example ipc_bench -- '\\.\pipe\name' 1000

use std::time::Instant;

use client_sidecar::IpcTransport;
use client_sidecar::ipc::pipe::PipeClient;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .ok_or("usage: ipc_bench <pipe> [n]")?;
    let count: usize = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000);

    let client = PipeClient::connect(&name).await?;
    // 预热一次，排除首次连接后的分配抖动。
    let _ = client.request("status", json!({})).await?;

    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let start = Instant::now();
        client.request("status", json!({})).await?;
        samples.push(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    samples.sort_unstable();

    let at = |q: f64| {
        let index = ((samples.len() as f64 * q) as usize).min(samples.len() - 1);
        samples[index]
    };
    println!(
        "samples={} p50={}us p95={}us p99={}us max={}us",
        samples.len(),
        at(0.50),
        at(0.95),
        at(0.99),
        samples[samples.len() - 1]
    );

    client.close().await;
    Ok(())
}
