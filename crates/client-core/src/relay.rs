//! 双向中继。

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::metrics::KernelMetrics;

/// 在两个双向流之间做全双工转发；任一方向结束即收尾。
///
/// 两个方向各自的 `tokio::io::copy` 完成时累加观测字节数。`select!` 会取消
/// 另一方向，因此被取消方向的尾部字节可能少计；契约中 `bytes_up` / `bytes_down`
/// 明确是「观测值」，不用于流量计费。
pub async fn bidirectional<A, B>(a: A, b: B, metrics: &Arc<KernelMetrics>) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_reader, mut a_writer) = tokio::io::split(a);
    let (mut b_reader, mut b_writer) = tokio::io::split(b);
    let a_to_b = async {
        let copied = tokio::io::copy(&mut a_reader, &mut b_writer).await?;
        metrics.add_bytes(copied, 0);
        Ok::<(), std::io::Error>(())
    };
    let b_to_a = async {
        let copied = tokio::io::copy(&mut b_reader, &mut a_writer).await?;
        metrics.add_bytes(0, copied);
        Ok::<(), std::io::Error>(())
    };
    tokio::select! {
        result = a_to_b => { result?; }
        result = b_to_a => { result?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn counts_bytes_on_completed_direction() {
        let metrics = Arc::new(KernelMetrics::default());
        let (a, mut a_peer) = tokio::io::duplex(64 * 1024);
        let (b, mut b_peer) = tokio::io::duplex(64 * 1024);
        let counter = Arc::clone(&metrics);
        let relay = tokio::spawn(async move { bidirectional(a, b, &counter).await });

        a_peer.write_all(b"hello").await.expect("write");
        let mut buf = [0u8; 5];
        b_peer.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"hello");

        a_peer.shutdown().await.expect("shutdown");
        relay.await.expect("join").expect("relay");
        let (_, _, up, down) = metrics.snapshot();
        assert_eq!(up, 5);
        assert_eq!(down, 0);
    }
}
