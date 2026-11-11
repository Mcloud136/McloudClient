//! sidecar：控制面最小实现（Windows）。
//!
//! 职责：
//! 1. 维护快照（引导快照 + 原子写 + 保留 5 版）；
//! 2. 连接**由外部托管**的内核 IPC，不持有内核进程的生命周期；
//! 3. 配置源（本地文件或订阅 URL）变化时导入新快照并触发 reload；
//! 4. IPC 断开后按指数退避重连，并补发未完成的 reload。
//!
//! 进程所有权（ADR 0001 §5）：内核由服务宿主或运维独立启动，
//! `kill sidecar` 不会终止内核；sidecar 重启后重新连接即可恢复控制。
//!
//! 安全：订阅 URL 含 `token`，日志只记录源类型（`file` / `url`），不记录 URL 本身。

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use clap::Parser;
    use client_sidecar::ipc::pipe::PipeClient;
    use client_sidecar::logging;
    use client_sidecar::protocol::Message;
    use client_sidecar::snapshot::SnapshotStore;
    use client_sidecar::{IpcError, IpcTransport, VERSION};
    use serde_json::{Value, json};

    /// 引导快照：仅放行全部流量直连，保证首次启动不空配置。
    const BOOTSTRAP: &str = r#"{"proxies":[],"proxy-groups":[],"rules":["MATCH,DIRECT"]}"#;
    const INITIAL_BACKOFF_MS: u64 = 100;
    const MAX_BACKOFF_MS: u64 = 5_000;

    #[derive(Debug, Parser)]
    #[command(name = "client-sidecar", version, about = "McloudClient 控制面")]
    struct Args {
        /// 内核的命名管道名。
        #[arg(long)]
        pipe: String,

        /// 配置源文件（JSON profile）。
        #[arg(long, conflicts_with = "source_url")]
        source: Option<PathBuf>,

        /// 配置源 URL（返回 JSON profile）；日志不会记录该地址。
        #[arg(long, conflicts_with = "source")]
        source_url: Option<String>,

        /// 快照目录。
        #[arg(long)]
        snapshot_dir: PathBuf,

        /// 源轮询间隔（秒）。
        #[arg(long, default_value_t = 2)]
        refresh_secs: u64,

        /// 首次连接内核 IPC 的等待上限（秒）；超时退出，避免静默空转。
        #[arg(long, default_value_t = 30)]
        connect_timeout_secs: u64,

        /// 日志目录；缺省为 `%ProgramData%\McloudClient\logs`。
        #[arg(long)]
        log_dir: Option<PathBuf>,
    }

    /// 配置来源；URL 不参与 `Debug`，避免 token 进入日志。
    #[derive(Clone)]
    enum Origin {
        File(PathBuf),
        Url(String),
    }

    impl Origin {
        fn from_args(args: &Args) -> Result<Self, Box<dyn std::error::Error>> {
            match (&args.source, &args.source_url) {
                (Some(path), None) => Ok(Self::File(path.clone())),
                (None, Some(url)) => Ok(Self::Url(url.clone())),
                _ => Err("exactly one of --source or --source-url is required".into()),
            }
        }

        fn label(&self) -> &'static str {
            match self {
                Self::File(_) => "file",
                Self::Url(_) => "url",
            }
        }
    }

    /// 内核连接状态：断开后按指数退避重连，不因单次失败退出。
    struct Link {
        pipe: String,
        client: Option<PipeClient>,
        backoff_ms: u64,
        next_attempt: Instant,
        connects: u64,
        failures: u64,
    }

    impl Link {
        fn new(pipe: String) -> Self {
            Self {
                pipe,
                client: None,
                backoff_ms: INITIAL_BACKOFF_MS,
                next_attempt: Instant::now(),
                connects: 0,
                failures: 0,
            }
        }

        /// 返回可用连接；未到重试时刻或重连失败时返回 `None`。
        async fn ensure(&mut self) -> Option<&PipeClient> {
            if self.client.is_none() {
                if Instant::now() < self.next_attempt {
                    return None;
                }
                match PipeClient::connect(&self.pipe).await {
                    Ok(client) => {
                        self.connects += 1;
                        self.backoff_ms = INITIAL_BACKOFF_MS;
                        tracing::info!(
                            connects = self.connects,
                            failures = self.failures,
                            "kernel ipc connected"
                        );
                        spawn_event_printer(&client);
                        self.client = Some(client);
                    }
                    Err(error) => {
                        self.failures += 1;
                        tracing::warn!(
                            %error,
                            retry_ms = self.backoff_ms,
                            "kernel ipc unavailable; retry in {} ms",
                            self.backoff_ms
                        );
                        self.next_attempt = Instant::now() + Duration::from_millis(self.backoff_ms);
                        self.backoff_ms = (self.backoff_ms * 2).min(MAX_BACKOFF_MS);
                        return None;
                    }
                }
            }
            self.client.as_ref()
        }

        /// 连接类错误才丢弃连接；内核拒绝（`Remote`）保持连接。
        fn mark(&mut self, error: &IpcError) {
            if !is_connection_error(error) {
                return;
            }
            tracing::warn!(%error, "kernel ipc lost; reconnecting");
            self.client = None;
            self.next_attempt = Instant::now() + Duration::from_millis(self.backoff_ms);
        }
    }

    fn is_connection_error(error: &IpcError) -> bool {
        matches!(
            error,
            IpcError::Io(_)
                | IpcError::Closed
                | IpcError::Timeout
                | IpcError::Protocol(_)
                | IpcError::Unexpected
        )
    }

    /// 事件接收时刻（Unix 微秒），与内核 `ts_us` 相减即端到端延迟。
    fn now_us() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |value| value.as_micros() as u64)
    }

    /// 把内核推送的事件记录到日志；连接关闭后任务自行退出。
    fn spawn_event_printer(client: &PipeClient) {
        let mut events = client.events();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Message::Event { kind, payload, .. }) => {
                        // 保留 "event <kind> received_us=… payload=…" 的连续格式，
                        // 便于抓包与既有测试脚本按行解析。
                        tracing::info!(received_us = now_us(), %payload, "event {kind}");
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "events dropped");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    #[tokio::main]
    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args = Args::parse();
        let _log_guard = logging::init_with(args.log_dir.clone(), "sidecar")?;
        tracing::info!(protocol = VERSION, "sidecar starting");

        let origin = Origin::from_args(&args)?;
        let store = SnapshotStore::new(&args.snapshot_dir, 5);
        store.ensure_dir()?;
        store.bootstrap(BOOTSTRAP.as_bytes())?;

        let mut source_hash = 0;
        let mut pending = import_source(&origin, &store, &mut source_hash).await;
        if pending {
            tracing::info!(source = origin.label(), "source imported; reload pending");
        }

        let mut link = Link::new(args.pipe.clone());
        wait_initial(&mut link, args.connect_timeout_secs).await?;
        if let Some(client) = link.client.as_ref() {
            let status = client.request("status", json!({ "detail": true })).await?;
            tracing::info!(%status, "kernel status");
        }

        let mut ticks = tokio::time::interval(Duration::from_secs(args.refresh_secs));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    result?;
                    tracing::info!("ctrl-c received, shutting down");
                    break;
                }
                _ = ticks.tick() => {
                    if import_source(&origin, &store, &mut source_hash).await {
                        pending = true;
                    }
                    match tick(&mut link, pending).await {
                        TickOutcome::Idle => {}
                        TickOutcome::Applied(value) => {
                            pending = false;
                            tracing::info!(%value, "reload applied");
                        }
                        TickOutcome::ConnectionLost(error) => {
                            link.mark(&error);
                            tracing::warn!(%error, pending, "ipc lost, reload pending");
                        }
                        TickOutcome::Rejected(error) => {
                            // 内核拒绝：重试同一份配置没有意义，等源变化再试。
                            pending = false;
                            tracing::warn!(%error, "kernel_rejected");
                        }
                    }
                }
            }
        }

        if let Some(client) = link.client.take() {
            client.close().await;
        }
        Ok(())
    }

    /// 首次连接：在超时内反复尝试，成功后退避状态由 `Link` 接管。
    async fn wait_initial(
        link: &mut Link,
        timeout_secs: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while link.ensure().await.is_none() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "kernel ipc not available within {timeout_secs} s: {}",
                    link.pipe
                )
                .into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// 单个 tick 的结果。
    enum TickOutcome {
        /// 无连接或无事可做。
        Idle,
        /// reload 成功。
        Applied(Value),
        /// 连接层失败：需要丢弃连接并退避重连。
        ConnectionLost(IpcError),
        /// 内核拒绝了配置：重试同一份配置没有意义。
        Rejected(IpcError),
    }

    /// 每 tick 先探活，再有 pending 时补发 reload。
    ///
    /// 没有心跳时，内核退出后 sidecar 会一直持有已失效的连接对象，
    /// 直到下一次 reload 才暴露；`status` 请求开销可忽略（p99 307 µs）。
    async fn tick(link: &mut Link, pending: bool) -> TickOutcome {
        let Some(client) = link.ensure().await else {
            return TickOutcome::Idle;
        };
        if let Err(error) = client.request("status", json!({})).await {
            return TickOutcome::ConnectionLost(error);
        }
        if !pending {
            return TickOutcome::Idle;
        }
        match client.request("reload", json!({})).await {
            Ok(value) => TickOutcome::Applied(value),
            Err(error) if is_connection_error(&error) => TickOutcome::ConnectionLost(error),
            Err(error) => TickOutcome::Rejected(error),
        }
    }

    /// 拉取一次源内容；错误信息中不含 URL（含 token）与正文。
    async fn read_source(origin: &Origin) -> Result<Vec<u8>, String> {
        match origin {
            Origin::File(path) => {
                std::fs::read(path).map_err(|error| format!("fetch_failed: {error}"))
            }
            Origin::Url(url) => client_sidecar::fetch::fetch_url(url)
                .await
                .map_err(|error| format!("fetch_failed: {error}")),
        }
    }

    /// 拉取源并在变化时写入新快照；返回是否需要 reload。
    async fn import_source(origin: &Origin, store: &SnapshotStore, source_hash: &mut u64) -> bool {
        let bytes = match read_source(origin).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, source = origin.label(), "source unavailable");
                return false;
            }
        };
        let new_hash = hash(&bytes);
        if new_hash == *source_hash {
            return false;
        }
        if let Err(error) = validate(&bytes) {
            tracing::warn!(%error, "validate_failed");
            return false;
        }
        match store.write(&bytes) {
            Ok(seq) => {
                *source_hash = new_hash;
                tracing::info!(seq, bytes = bytes.len(), "imported source as snapshot");
                true
            }
            Err(error) => {
                tracing::warn!(%error, "snapshot_write_failed");
                false
            }
        }
    }

    fn validate(bytes: &[u8]) -> Result<(), String> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|error| format!("json: {error}"))?;
        let rules = value
            .get("rules")
            .and_then(Value::as_array)
            .ok_or("missing rules")?;
        if rules.is_empty() {
            return Err("rules is empty".to_owned());
        }
        if value.get("proxies").and_then(Value::as_array).is_none() {
            return Err("missing proxies".to_owned());
        }
        Ok(())
    }

    fn hash(bytes: &[u8]) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("client-sidecar 目前仅支持 Windows（命名管道 IPC）");
}
