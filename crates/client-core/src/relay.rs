//! 双向中继。

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};

/// 在两个双向流之间做全双工转发；任一方向结束即收尾。
pub async fn bidirectional<A, B>(a: A, b: B) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_reader, mut a_writer) = tokio::io::split(a);
    let (mut b_reader, mut b_writer) = tokio::io::split(b);
    let a_to_b = tokio::io::copy(&mut a_reader, &mut b_writer);
    let b_to_a = tokio::io::copy(&mut b_reader, &mut a_writer);
    tokio::select! {
        result = a_to_b => {
            result?;
        }
        result = b_to_a => {
            result?;
        }
    }
    Ok(())
}
