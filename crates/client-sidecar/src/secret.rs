//! `Secret<T>`：凭据包装器（ADR 0004 L1）。
//!
//! 不实现 `Debug` / `Display` / `Serialize` / `Clone`：凭据无法被日志、
//! serde、截图 OCR 之外的常规路径意外带出；`Drop` 时清零内存。
//! 仅控制面（`panel` / `credential`）通过 [`Secret::expose`] 读取。

use zeroize::Zeroize;

/// 持有敏感值；离开作用域即清零。
pub struct Secret<T: Zeroize> {
    inner: T,
}

impl<T: Zeroize> Secret<T> {
    /// 包装一个敏感值。
    #[must_use]
    pub fn new(value: T) -> Self {
        Self { inner: value }
    }

    /// 读取敏感值；调用方必须保证不写入日志 / UI / IPC。
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.inner
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}

/// `Secret` 不可 `Debug`：
///
/// ```compile_fail
/// use client_sidecar::secret::Secret;
/// let secret = Secret::new(String::from("password"));
/// println!("{:?}", secret);
/// ```
///
/// `Secret` 不可 `Serialize`：
///
/// ```compile_fail
/// use client_sidecar::secret::Secret;
/// let secret = Secret::new(String::from("password"));
/// let _ = serde_json::to_string(&secret);
/// ```
///
/// `Secret` 不可 `Clone`：
///
/// ```compile_fail
/// use client_sidecar::secret::Secret;
/// let secret = Secret::new(String::from("password"));
/// let _ = secret.clone();
/// ```
#[cfg(doctest)]
pub struct SecretNegativeBounds;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use zeroize::Zeroize;

    use super::Secret;

    #[test]
    fn exposes_value_until_drop() {
        let secret = Secret::new(String::from("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }

    struct Probe {
        value: String,
        zeroized: Arc<AtomicBool>,
    }

    impl Zeroize for Probe {
        fn zeroize(&mut self) {
            self.value.zeroize();
            self.zeroized.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn zeroizes_on_drop() {
        let flag = Arc::new(AtomicBool::new(false));
        let probe = Probe {
            value: String::from("hunter2"),
            zeroized: Arc::clone(&flag),
        };
        let secret = Secret::new(probe);
        assert_eq!(secret.expose().value, "hunter2");
        assert!(!flag.load(Ordering::SeqCst));
        drop(secret);
        assert!(flag.load(Ordering::SeqCst));
    }
}
