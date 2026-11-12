//! 内核内部计数器，供 IPC `metrics` 暴露。
//!
//! 只统计连接与已观测字节，不采样进程资源——RSS / CPU 属于控制面本地职责。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct KernelMetrics {
    connections_total: AtomicU64,
    connections_active: AtomicU64,
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
}

impl KernelMetrics {
    /// 累加两个方向的观测字节数。
    pub fn add_bytes(&self, up: u64, down: u64) {
        if up > 0 {
            self.bytes_up.fetch_add(up, Ordering::Relaxed);
        }
        if down > 0 {
            self.bytes_down.fetch_add(down, Ordering::Relaxed);
        }
    }

    /// 返回 `(total, active, bytes_up, bytes_down)`。
    #[must_use]
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.connections_total.load(Ordering::Relaxed),
            self.connections_active.load(Ordering::Relaxed),
            self.bytes_up.load(Ordering::Relaxed),
            self.bytes_down.load(Ordering::Relaxed),
        )
    }
}

/// 连接关闭时自动减少活跃计数，覆盖所有提前返回路径。
#[derive(Debug)]
pub struct ActiveGuard {
    metrics: Arc<KernelMetrics>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.metrics
            .connections_active
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// 记录一次新接受的 SOCKS5 连接，返回 RAII guard。
#[must_use]
pub fn on_accept(metrics: &Arc<KernelMetrics>) -> ActiveGuard {
    metrics.connections_total.fetch_add(1, Ordering::Relaxed);
    metrics.connections_active.fetch_add(1, Ordering::Relaxed);
    ActiveGuard {
        metrics: Arc::clone(metrics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_count_returns_to_zero_on_drop() {
        let metrics = Arc::new(KernelMetrics::default());
        let guard = on_accept(&metrics);
        assert_eq!(metrics.snapshot(), (1, 1, 0, 0));
        drop(guard);
        assert_eq!(metrics.snapshot(), (1, 0, 0, 0));
    }

    #[test]
    fn byte_counters_accumulate() {
        let metrics = KernelMetrics::default();
        metrics.add_bytes(100, 0);
        metrics.add_bytes(0, 2048);
        metrics.add_bytes(0, 0);
        assert_eq!(metrics.snapshot(), (0, 0, 100, 2048));
    }
}
