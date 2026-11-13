//! 更新 manifest 签名工具（FA 硬前置）。
//!
//! ```text
//! manifest-sign keygen --out-dir D:\wxmuma\_secrets\mcloud-update
//! manifest-sign sign --key D:\wxmuma\_secrets\mcloud-update\private.pk8 --manifest docs\update-manifest.json
//! ```
//!
//! 私钥文件永不入库；公钥 hex 提交后由 GUI 编译期内嵌。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use client_sidecar::update_sign;

#[derive(Parser)]
#[command(about = "Ed25519 update-manifest keygen/sign")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 生成密钥对：private.pk8（私钥，勿入库）+ public.hex（公钥，入库）。
    Keygen {
        /// 输出目录（仓库外）。
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// 对 manifest 的 version/url/sha256 签名并写回 signature 字段。
    Sign {
        /// PKCS#8 私钥文件。
        #[arg(long)]
        key: PathBuf,
        /// manifest JSON 文件（原地写回）。
        #[arg(long)]
        manifest: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Keygen { out_dir } => keygen(&out_dir),
        Command::Sign { key, manifest } => sign(&key, &manifest),
    }
}

fn keygen(out_dir: &std::path::Path) -> Result<()> {
    let (private, public) = update_sign::generate_keypair().context("keygen")?;
    std::fs::create_dir_all(out_dir).context("create out dir")?;
    let private_path = out_dir.join("private.pk8");
    if private_path.exists() {
        bail!(
            "refusing to overwrite existing private key: {}",
            private_path.display()
        );
    }
    std::fs::write(&private_path, &private).context("write private key")?;
    let public_path = out_dir.join("public.hex");
    std::fs::write(&public_path, update_sign::hex_encode(&public)).context("write public key")?;
    println!("private key: {}", private_path.display());
    println!("public key:  {}", public_path.display());
    Ok(())
}

fn sign(key_path: &std::path::Path, manifest_path: &std::path::Path) -> Result<()> {
    let private = std::fs::read(key_path).context("read private key")?;
    let text = std::fs::read_to_string(manifest_path).context("read manifest")?;
    let mut value: serde_json::Value = serde_json::from_str(&text).context("manifest json")?;
    let version = value["version"].as_str().context("manifest version")?;
    let url = value["url"].as_str().context("manifest url")?;
    let sha256 = value["sha256"].as_str().context("manifest sha256")?;
    if version.is_empty() || url.is_empty() || sha256.len() != 64 {
        bail!("manifest fields incomplete (version/url empty or sha256 not 64 hex chars)");
    }
    let message = update_sign::signed_message(version, url, sha256);
    let signature = update_sign::sign(&private, &message).context("sign")?;
    value["signature"] = serde_json::Value::String(update_sign::hex_encode(&signature));
    std::fs::write(manifest_path, serde_json::to_string_pretty(&value)? + "\n")
        .context("write manifest")?;
    println!("signed {}", manifest_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use client_sidecar::update_sign;

    #[test]
    fn sign_then_verify_with_public_key() {
        let (private, public) = update_sign::generate_keypair().expect("keygen");
        let message =
            update_sign::signed_message("0.2.0", "https://example.invalid/s.exe", &"ab".repeat(32));
        let signature = update_sign::sign(&private, &message).expect("sign");
        update_sign::verify(&public, &message, &update_sign::hex_encode(&signature))
            .expect("verify");
    }
}
