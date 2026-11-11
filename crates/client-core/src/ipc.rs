//! 内核侧 IPC 方法处理。
//!
//! 提供 `status` 与 `reload`。`reload` 复用与定时重载相同的
//! 「先构建、后替换」路径，失败时保留现有路由器。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use client_sidecar::{Handler, IpcError};
use serde_json::{Value, json};

use crate::{context::AppContext, geo::GeoData, reload};

pub struct KernelHandler {
    context: Arc<AppContext>,
    source: reload::Source,
    geo: Arc<GeoData>,
    version: AtomicU64,
    started: Instant,
}

impl KernelHandler {
    #[must_use]
    pub fn new(context: Arc<AppContext>, source: reload::Source, geo: Arc<GeoData>) -> Self {
        Self {
            context,
            source,
            geo,
            version: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    async fn reload(&self) -> Result<Value, IpcError> {
        let bytes = reload::fetch(&self.source)
            .await
            .map_err(|error| IpcError::Remote {
                code: "fetch_failed".to_owned(),
                message: error.to_string(),
            })?;
        let router = reload::build_router(&bytes, Arc::clone(&self.geo)).map_err(|error| {
            IpcError::Remote {
                code: "validate_failed".to_owned(),
                message: error.to_string(),
            }
        })?;
        let rules = router.rule_count();
        self.context.replace_router(router);
        let version = self.version.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(json!({ "version": version, "rules": rules }))
    }
}

#[async_trait]
impl Handler for KernelHandler {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        match method {
            "status" => {
                let detail = params
                    .get("detail")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut body = json!({
                    "rules": self.context.router().rule_count(),
                    "version": self.version.load(Ordering::Relaxed),
                    "uptime_s": self.started.elapsed().as_secs(),
                });
                if detail {
                    body["fakeip_entries"] = json!(self.context.fakeip.len());
                    body["source"] = json!(match &self.source {
                        reload::Source::File(path) => format!("file:{}", path.display()),
                        reload::Source::Url(_) => "url".to_owned(),
                    });
                }
                Ok(body)
            }
            "reload" => self.reload().await,
            other => Err(IpcError::Remote {
                code: "unknown_method".to_owned(),
                message: format!("unknown method: {other}"),
            }),
        }
    }
}
