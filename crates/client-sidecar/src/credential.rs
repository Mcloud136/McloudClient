//! OS 凭据库抽象（ADR 0004 L1/L7）：「记住我」的密码只存这里。
//!
//! Windows 走系统凭据管理器（keyring，safe crate，DPAPI 按用户隔离）；
//! 非 Windows 与单测用内存桩——GUI 仅 Windows，Linux CI 不触真实凭据库。

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;

use crate::secret::Secret;

/// 凭据存储边界：只接受/返回 `Secret<String>`，调用方无法绕过清零语义。
pub trait CredentialStore: Send + Sync {
    /// 读取凭据；不存在返回 `None`。
    ///
    /// # Errors
    ///
    /// 底层存储访问失败时返回错误。
    fn load(&self, service: &str, account: &str) -> Result<Option<Secret<String>>>;

    /// 写入/覆盖凭据。
    ///
    /// # Errors
    ///
    /// 底层存储访问失败时返回错误。
    fn store(&self, service: &str, account: &str, value: &Secret<String>) -> Result<()>;

    /// 删除凭据；不存在视为成功。
    ///
    /// # Errors
    ///
    /// 底层存储访问失败时返回错误。
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}

/// 内存桩：测试与非 Windows 平台使用；进程退出即消失。
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<(String, String), String>>,
}

impl CredentialStore for MemoryStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<Secret<String>>> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("store poisoned"))?;
        Ok(entries
            .get(&(service.to_owned(), account.to_owned()))
            .map(|value| Secret::new(value.clone())))
    }

    fn store(&self, service: &str, account: &str, value: &Secret<String>) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("store poisoned"))?;
        entries.insert(
            (service.to_owned(), account.to_owned()),
            value.expose().clone(),
        );
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("store poisoned"))?;
        entries.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// Windows 凭据管理器（按当前用户隔离，DPAPI 保护）。
#[cfg(windows)]
pub struct WindowsStore;

#[cfg(windows)]
impl CredentialStore for WindowsStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<Secret<String>>> {
        let entry = keyring::Entry::new(service, account)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(Secret::new(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn store(&self, service: &str, account: &str, value: &Secret<String>) -> Result<()> {
        let entry = keyring::Entry::new(service, account)?;
        entry.set_password(value.expose())?;
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// 平台默认存储：Windows 凭据管理器，其余内存桩。
#[must_use]
pub fn default_store() -> Box<dyn CredentialStore> {
    #[cfg(windows)]
    {
        Box::new(WindowsStore)
    }
    #[cfg(not(windows))]
    {
        Box::new(MemoryStore::default())
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialStore, MemoryStore};
    use crate::secret::Secret;

    #[test]
    fn memory_store_roundtrip_and_isolation() {
        let store = MemoryStore::default();
        assert!(store.load("svc", "a").expect("load").is_none());
        store
            .store("svc", "a", &Secret::new(String::from("pw")))
            .expect("store");
        assert_eq!(
            store
                .load("svc", "a")
                .expect("load")
                .expect("some")
                .expose(),
            "pw"
        );
        assert!(store.load("svc", "b").expect("load").is_none());
        assert!(store.load("other", "a").expect("load").is_none());
        store.delete("svc", "a").expect("delete");
        assert!(store.load("svc", "a").expect("load").is_none());
        store.delete("svc", "a").expect("delete again");
    }

    /// 真实凭据管理器冒烟：手动运行（`cargo test -p client-sidecar -- --ignored`），
    /// 使用临时条目并清理，CI 不跑。
    #[cfg(windows)]
    #[test]
    #[ignore = "touches real Windows credential manager"]
    fn windows_store_roundtrip() {
        let store = super::WindowsStore;
        let service = "mcloud-client-test";
        let account = "spike@example.com";
        store
            .store(service, account, &Secret::new(String::from("pw")))
            .expect("store");
        assert_eq!(
            store
                .load(service, account)
                .expect("load")
                .expect("some")
                .expose(),
            "pw"
        );
        store.delete(service, account).expect("delete");
        assert!(store.load(service, account).expect("load").is_none());
    }
}
