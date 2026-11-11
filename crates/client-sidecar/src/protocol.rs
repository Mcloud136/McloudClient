//! 帧协议：`u32` 大端长度前缀 + UTF-8 JSON。
//!
//! 用长度前缀而非裸 JSON lines，避免消息内含换行时的分帧歧义；
//! 每种消息带 `v` 字段，便于跨版本协商。

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 当前协议版本。不兼容变更时递增。
pub const VERSION: u16 = 1;

/// 单帧上限，防止对端发送超大长度导致内存放大。
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Request {
        v: u16,
        id: u64,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
    Response {
        v: u16,
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorBody>,
    },
    Event {
        v: u16,
        kind: String,
        payload: serde_json::Value,
    },
}

impl Message {
    pub fn version(&self) -> u16 {
        match self {
            Self::Request { v, .. } | Self::Response { v, .. } | Self::Event { v, .. } => *v,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("frame too large: {0} bytes (max {MAX_FRAME})")]
    FrameTooLarge(usize),

    #[error("unsupported protocol version: {got} (expected {VERSION})")]
    VersionMismatch { got: u16 },
}

pub fn encode(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    Ok(serde_json::to_vec(message)?)
}

pub fn decode(bytes: &[u8]) -> Result<Message, ProtocolError> {
    Ok(serde_json::from_slice(bytes)?)
}

pub async fn write_frame<W>(writer: &mut W, message: &Message) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    let body = encode(message)?;
    let len = u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge(body.len()))?;
    if body.len() > MAX_FRAME {
        return Err(ProtocolError::FrameTooLarge(body.len()));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Message, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let size = u32::from_be_bytes(len) as usize;
    if size > MAX_FRAME {
        return Err(ProtocolError::FrameTooLarge(size));
    }
    let mut body = vec![0u8; size];
    reader.read_exact(&mut body).await?;
    decode(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let msg = Message::Request {
            v: VERSION,
            id: 7,
            method: "status".to_owned(),
            params: serde_json::json!({"detail": true}),
        };
        let bytes = encode(&msg).expect("encode");
        assert_eq!(decode(&bytes).expect("decode"), msg);
    }

    #[test]
    fn response_roundtrip_with_and_without_error() {
        let ok = Message::Response {
            v: VERSION,
            id: 1,
            ok: true,
            result: Some(serde_json::json!({"rules": 4})),
            error: None,
        };
        let err = Message::Response {
            v: VERSION,
            id: 2,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: "bad_request".to_owned(),
                message: "unknown method".to_owned(),
            }),
        };
        for msg in [ok, err] {
            let bytes = encode(&msg).expect("encode");
            assert_eq!(decode(&bytes).expect("decode"), msg);
        }
    }

    #[test]
    fn event_roundtrip() {
        let msg = Message::Event {
            v: VERSION,
            kind: "reload".to_owned(),
            payload: serde_json::json!({"state": "applied", "version": 3}),
        };
        let bytes = encode(&msg).expect("encode");
        assert_eq!(decode(&bytes).expect("decode"), msg);
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut header = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        header.extend_from_slice(b"{}");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let err = rt.block_on(async {
            let mut cursor = std::io::Cursor::new(header);
            read_frame(&mut cursor).await
        });
        assert!(matches!(err, Err(ProtocolError::FrameTooLarge(_))));
    }

    #[tokio::test]
    async fn frame_roundtrip_over_stream() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let msg = Message::Event {
            v: VERSION,
            kind: "test".to_owned(),
            payload: serde_json::Value::Null,
        };
        let expected = msg.clone();
        let writer = tokio::spawn(async move { write_frame(&mut a, &msg).await });
        let got = read_frame(&mut b).await.expect("read");
        writer.await.expect("join").expect("write");
        assert_eq!(got, expected);
    }
}
