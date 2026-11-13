//! 更新 manifest 的 Ed25519 签名与验签（FA 硬前置，单一事实源）。
//!
//! 签名消息为 `version\nurl\nsha256` 的 UTF-8 字节（字段顺序固定）；
//! manifest JSON 以 `signature` 字段携带 64 字节签名的 hex 编码。
//! 验签失败一律拒绝更新（硬拒绝，无降级路径）。

use ring::signature::{self, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};

/// 签名消息：三个字段按固定顺序以换行拼接。
#[must_use]
pub fn signed_message(version: &str, url: &str, sha256: &str) -> Vec<u8> {
    format!("{version}\n{url}\n{sha256}").into_bytes()
}

/// 生成新密钥对，返回 `(PKCS#8 私钥 DER, 原始公钥 32 字节)`。
///
/// # Errors
///
/// 系统随机源不可用或密钥生成失败时返回错误。
pub fn generate_keypair() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| anyhow::anyhow!("generate ed25519 keypair"))?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow::anyhow!("parse generated keypair"))?;
    Ok((pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec()))
}

/// 用 PKCS#8 私钥 DER 对消息签名，返回 64 字节签名。
///
/// # Errors
///
/// 私钥格式非法时返回错误。
pub fn sign(pkcs8_der: &[u8], message: &[u8]) -> anyhow::Result<Vec<u8>> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8_der)
        .map_err(|_| anyhow::anyhow!("invalid ed25519 private key"))?;
    Ok(pair.sign(message).as_ref().to_vec())
}

/// 用原始公钥（32 字节）验签；失败即拒绝。
///
/// # Errors
///
/// 公钥长度非法或签名不匹配时返回错误。
pub fn verify(public_key: &[u8], message: &[u8], signature_hex: &str) -> anyhow::Result<()> {
    let sig = hex_decode(signature_hex)?;
    if sig.len() != 64 {
        anyhow::bail!("signature must be 64 bytes");
    }
    UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(message, &sig)
        .map_err(|_| anyhow::anyhow!("manifest signature verification failed"))
}

/// 小写 hex 编码。
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 解码 hex（长度须为偶数，字符须为 hex）。
///
/// # Errors
///
/// 输入非法时返回错误。
pub fn hex_decode(text: &str) -> anyhow::Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        anyhow::bail!("hex length must be even");
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| anyhow::anyhow!("bad hex")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{generate_keypair, hex_encode, sign, signed_message, verify};

    #[test]
    fn sign_verify_roundtrip() {
        let (priv_key, pub_key) = generate_keypair().expect("keygen");
        let msg = signed_message("0.2.0", "https://example.invalid/setup.exe", "ab12");
        let sig = sign(&priv_key, &msg).expect("sign");
        verify(&pub_key, &msg, &hex_encode(&sig)).expect("verify");
    }

    #[test]
    fn tampered_message_rejected() {
        let (priv_key, pub_key) = generate_keypair().expect("keygen");
        let msg = signed_message("0.2.0", "https://example.invalid/a.exe", "ab12");
        let sig = sign(&priv_key, &msg).expect("sign");
        let tampered = signed_message("0.2.0", "https://example.invalid/b.exe", "ab12");
        assert!(verify(&pub_key, &tampered, &hex_encode(&sig)).is_err());
    }

    #[test]
    fn wrong_key_rejected() {
        let (priv_key, _) = generate_keypair().expect("keygen");
        let (_, other_pub) = generate_keypair().expect("keygen");
        let msg = signed_message("0.2.0", "u", "s");
        let sig = sign(&priv_key, &msg).expect("sign");
        assert!(verify(&other_pub, &msg, &hex_encode(&sig)).is_err());
    }

    #[test]
    fn bad_signature_shapes_rejected() {
        let (_, pub_key) = generate_keypair().expect("keygen");
        let msg = signed_message("0.2.0", "u", "s");
        assert!(verify(&pub_key, &msg, "").is_err());
        assert!(verify(&pub_key, &msg, "zz").is_err());
        assert!(verify(&pub_key, &msg, &"00".repeat(63)).is_err());
        assert!(verify(&pub_key, &msg, &"00".repeat(64)).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [0u8, 1, 15, 16, 255];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "00010f10ff");
        assert_eq!(super::hex_decode(&encoded).expect("decode"), bytes);
        assert!(super::hex_decode("0").is_err());
        assert!(super::hex_decode("gg").is_err());
    }
}
