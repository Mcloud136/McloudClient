//! P1.2 系统代理接管：WinINET 用户级代理（HKCU Internet Settings）。
//!
//! 接管前记录原配置，释放/正常退出时恢复；**强杀不恢复属已知限制**
//! （与内核日志 flush 边界同类）。广播 WM_SETTINGCHANGE 用固定 PowerShell
//! 脚本（无用户输入拼接），避免在客户端代码引入 unsafe FFI。

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

/// 广播 WM_SETTINGCHANGE("Internet Settings")，HWND_BROADCAST。
const BROADCAST: &str = "Add-Type -Namespace W -Name N -MemberDefinition \
'[DllImport(\"user32.dll\")] public static extern IntPtr SendMessageTimeout(IntPtr h, uint m, \
UIntPtr w, string l, uint f, uint t, out UIntPtr r);'; $r=[UIntPtr]::Zero; \
[W.N]::SendMessageTimeout([IntPtr]0xffff,0x1A,[UIntPtr]::Zero,'Internet Settings',2,5000,[ref]$r) \
| Out-Null";

/// 用户级代理配置快照。
#[derive(Clone, Debug)]
pub struct ProxySnapshot {
    pub enable: u32,
    pub server: String,
    pub override_list: String,
}

/// 读取当前用户级代理配置（缺省值视为关闭/空）。
///
/// # Errors
///
/// 注册表键不可读时返回错误。
pub fn read() -> Result<ProxySnapshot> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(KEY, KEY_READ)
        .context("open Internet Settings")?;
    Ok(ProxySnapshot {
        enable: key.get_value("ProxyEnable").unwrap_or(0),
        server: key.get_value("ProxyServer").unwrap_or_default(),
        override_list: key.get_value("ProxyOverride").unwrap_or_default(),
    })
}

/// 写入用户级代理配置并广播设置变更。
///
/// # Errors
///
/// 注册表键不可写时返回错误。
pub fn write(snapshot: &ProxySnapshot) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(KEY)
        .context("open Internet Settings for write")?;
    key.set_value("ProxyEnable", &snapshot.enable)
        .context("set ProxyEnable")?;
    key.set_value("ProxyServer", &snapshot.server)
        .context("set ProxyServer")?;
    key.set_value("ProxyOverride", &snapshot.override_list)
        .context("set ProxyOverride")?;
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", BROADCAST])
        .spawn();
    Ok(())
}

/// 接管会话：持有原配置，Drop 时恢复。
pub struct SystemProxy {
    original: Option<ProxySnapshot>,
    target: String,
}

impl SystemProxy {
    /// `target` 形如 `127.0.0.1:1080`（内核 SOCKS/HTTP 入站地址）。
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            original: None,
            target: target.into(),
        }
    }

    /// 是否已接管。
    #[must_use]
    pub fn is_taken(&self) -> bool {
        self.original.is_some()
    }

    /// 接管：记录原配置后指向内核入站；override 置空使回环也走代理（可观测）。
    ///
    /// # Errors
    ///
    /// 注册表读写失败时返回错误（不改变接管状态）。
    pub fn takeover(&mut self) -> Result<()> {
        if self.original.is_none() {
            self.original = Some(read()?);
        }
        write(&ProxySnapshot {
            enable: 1,
            server: self.target.clone(),
            override_list: String::new(),
        })?;
        Ok(())
    }

    /// 释放：恢复接管前的原配置。
    ///
    /// # Errors
    ///
    /// 注册表写失败时返回错误。
    pub fn release(&mut self) -> Result<()> {
        if let Some(original) = self.original.take() {
            write(&original)?;
        }
        Ok(())
    }
}

impl Drop for SystemProxy {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
