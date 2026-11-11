//! 事件探针：连接管道、订阅事件、发起一次方法调用，并打印事件与响应。
//!
//! 用法：cargo run -p client-sidecar --example event_probe -- '\\.\pipe\name' [method]

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use client_sidecar::IpcTransport;
    use client_sidecar::ipc::pipe::PipeClient;
    use client_sidecar::protocol::Message;
    use serde_json::json;

    let mut args = std::env::args().skip(1);
    let name = args
        .next()
        .ok_or("usage: event_probe <pipe-name> [method]")?;
    let method = args.next().unwrap_or_else(|| "reload".to_owned());

    let client = PipeClient::connect(&name).await?;
    let mut events = client.events();

    let printer = tokio::spawn(async move {
        while let Ok(message) = events.recv().await {
            if let Message::Event { kind, payload, .. } = message {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |value| value.as_micros() as u64);
                println!("event {kind} received_us={now} payload={payload}");
            }
        }
    });

    match client.request(&method, json!({})).await {
        Ok(value) => println!("response=ok {value}"),
        Err(error) => println!("response=err {error}"),
    }

    // 事件与响应在同一 writer 上顺序写出，短暂等待确保打印任务清空队列。
    tokio::time::sleep(Duration::from_millis(300)).await;
    printer.abort();
    client.close().await;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("event_probe requires Windows (named pipes)");
}
