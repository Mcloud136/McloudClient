//! 全局运行上下文。
//!
//! 路由器需要支持**热重载**（订阅内容变化时原子替换），因此不能简单
//! 地以 `Arc<Router>` 传给各模块。这里用 [`ArcSwap`] 做无锁原子替换：
//! 读路径（每个连接一次）无锁，写路径（配置更新）极少发生。
//!
//! 替换路由器时同步重启健康检查任务——否则旧任务会继续用旧成员探测，
//! 结果写进新路由器的健康表，造成选路错乱。

use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tokio::task::JoinHandle;

use crate::{dns::FakeIp, health, metrics::KernelMetrics, router::Router};

/// 进程内共享状态。
pub struct AppContext {
    router: ArcSwap<Router>,
    /// fake-ip 池（跨配置重载保留，避免已连接的客户端反查失败）。
    pub fakeip: Arc<FakeIp>,
    /// 连接与字节计数器，供 IPC `metrics` 读取。
    pub metrics: Arc<KernelMetrics>,
    health_tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AppContext {
    /// 用初始路由器构造，并启动健康检查。
    #[must_use]
    pub fn new(router: Router, fakeip: Arc<FakeIp>) -> Arc<Self> {
        let context = Arc::new(Self {
            router: ArcSwap::from_pointee(router),
            fakeip,
            metrics: Arc::new(KernelMetrics::default()),
            health_tasks: Mutex::new(Vec::new()),
        });
        context.restart_health();
        context
    }

    /// 取当前路由器（无锁原子读）。
    #[must_use]
    pub fn router(&self) -> Arc<Router> {
        self.router.load_full()
    }

    /// 原子替换路由器，并按新配置重启健康检查。
    ///
    /// 用户手动锁定的节点选择会迁移到新路由器；新配置中已不存在的成员自动丢弃。
    pub fn replace_router(self: &Arc<Self>, router: Router) {
        tracing::info!(rules = router.rule_count(), "router reloaded");
        let previous = self.router.load_full();
        router.inherit_selections(&previous);
        self.router.store(Arc::new(router));
        self.restart_health();
    }

    /// 中止旧探测任务，按当前路由器重新启动。
    fn restart_health(self: &Arc<Self>) {
        let mut guard = match self.health_tasks.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(%error, "health task registry poisoned");
                return;
            }
        };
        for task in guard.drain(..) {
            task.abort();
        }
        let specs = self.router.load().health_specs();
        if specs.is_empty() {
            return;
        }
        guard.extend(health::spawn_all(Arc::clone(self), specs));
    }
}
