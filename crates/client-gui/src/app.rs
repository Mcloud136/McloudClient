//! reactor 渲染层（ADR 0005）：登录页 + 四区主界面 + 内核观测 + A2 订阅同步。
//!
//! 凭据边界：`Secret` 只在本模块与 `panel`/`sync` 内持有；UI 控件只绑定脱敏 DTO。
//! 后台 IPC 任务经 `SharedState` 供数；reactor 侧以 250 ms revision 轮询桥接
//! （纯内存读，无 IPC；`LocalSender` 非 Send，不能跨线程直推消息）。
//!
//! 登录态机（A1）：LoggedOut / Authenticating / Authenticated；面板失败降级为
//! Degraded（保留缓存 DTO + 上次更新时间）；403 即时回登录页。

use std::sync::Arc;
use std::time::{Duration, Instant};

use client_sidecar::credential::CredentialStore;
use client_sidecar::secret::Secret;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use windows_reactor::*;

use crate::panel::{
    AccountView, CheckoutView, GuestConfig, OrderView, PanelClient, PanelError, PaymentMethodView,
    PlanView, SubscriptionView, period_label,
};
use crate::proxy::SystemProxy;
use crate::shop::{OrderSession, ShopState, qr_png};
use crate::state::{Command, LinkStatus, SharedState, fmt_age, fmt_bytes, fmt_uptime};
use crate::sync::{SubscriptionSync, SyncOutcome};
use crate::task::TaskHandle;

const CREDENTIAL_SERVICE: &str = "mcloud-client";
const TICK: Duration = Duration::from_millis(250);
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// 重句柄：经 OnceLock 传入（reactor 的 Input 要求 Clone+PartialEq，
/// 不能携带 IPC 通道 / 凭据库 / 共享状态）。
pub struct GuiHandles {
    pub shared: Arc<SharedState>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub task: TaskHandle,
    pub panel: PanelClient,
    pub store: Arc<dyn CredentialStore>,
    pub sync: Arc<AsyncMutex<SubscriptionSync>>,
}

static HANDLES: std::sync::OnceLock<GuiHandles> = std::sync::OnceLock::new();

/// 轻配置：作为 reactor Input。
#[derive(Clone, PartialEq)]
pub struct GuiConfig {
    pub site: String,
    pub probe_default: String,
    pub proxy_target: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    LoggedOut,
    Authenticating,
    Authenticated,
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Proxy,
    Subscription,
    Shop,
    Account,
}

#[derive(Clone)]
enum SyncState {
    Idle,
    Syncing(u64),
    Synced(u64),
    Failed(String),
}

#[derive(Clone)]
pub struct LoginPayload {
    auth: Arc<Secret<String>>,
    account: AccountView,
    subscription: Arc<SubscriptionView>,
    persist_password: Option<Arc<Secret<String>>>,
}

#[derive(Clone)]
pub enum Message {
    Site(String),
    Email(String),
    Password(String),
    Remember(bool),
    Login,
    LoginDone(Result<LoginPayload, String>),
    Logout,
    LogoutDone(Result<(), String>),
    Refresh,
    RefreshDone(Result<(AccountView, Arc<SubscriptionView>), PanelError>),
    SyncDone(Result<SyncOutcome, String>),
    ResetSecurity,
    ResetDone(Result<Arc<Secret<String>>, PanelError>),
    GuestLoaded(GuestConfig),
    OpenUrl(String),
    SelectZone(Option<String>),
    ProbeInput(String),
    Probe,
    ProbeNodes,
    LockNode(String, String),
    AutoNode(String),
    SessionCheck(Result<bool, String>),
    ShopLoaded(Result<(Vec<PlanView>, Vec<PaymentMethodView>, Vec<OrderView>), PanelError>),
    PlanSel(Option<usize>),
    PeriodSel(Option<usize>),
    MethodSel(Option<usize>),
    CouponInput(String),
    CouponCheck,
    CouponDone(Result<String, PanelError>),
    PlaceOrder,
    PlaceDone(Result<(String, CheckoutView), PanelError>),
    PollDone(Result<i64, PanelError>),
    CancelOrder(String),
    CancelDone(Result<(), PanelError>),
    ToggleRemindExpire(bool),
    ToggleRemindTraffic(bool),
    ToggleAutoRenewal(bool),
    PwOld(String),
    PwNew(String),
    ChangePassword,
    ChangeDone(Result<(), PanelError>),
    SettingsDone(Result<(), PanelError>),
    ToggleProxy,
    Tick,
}

pub struct Shell {
    shared: Arc<SharedState>,
    commands: mpsc::UnboundedSender<Command>,
    _task: TaskHandle,
    panel: PanelClient,
    store: Arc<dyn CredentialStore>,
    sync: Arc<AsyncMutex<SubscriptionSync>>,
    phase: Phase,
    zone: Zone,
    site: String,
    email: String,
    password: String,
    remember: bool,
    error: Option<String>,
    auth: Option<Arc<Secret<String>>>,
    account: Option<AccountView>,
    subscription: Option<Arc<SubscriptionView>>,
    probe_input: String,
    last_revision: u64,
    last_check: Instant,
    last_refresh: Instant,
    last_update: Option<Instant>,
    degraded: bool,
    sync_state: SyncState,
    sync_started: Instant,
    guest: GuestConfig,
    reset_confirm: bool,
    shop: ShopState,
    pw_old: String,
    pw_new: String,
    proxy: SystemProxy,
}

impl Drop for Shell {
    fn drop(&mut self) {
        self._task.stop();
    }
}

impl Component for Shell {
    type Input = GuiConfig;
    type Message = Message;

    fn create(input: &GuiConfig, context: &ComponentContext<Self>) -> Self {
        let handles = HANDLES.get().expect("gui handles set before run");
        let mut shell = Self {
            shared: Arc::clone(&handles.shared),
            commands: handles.commands.clone(),
            _task: handles.task.clone(),
            panel: handles.panel.clone(),
            store: Arc::clone(&handles.store),
            sync: Arc::clone(&handles.sync),
            phase: Phase::LoggedOut,
            zone: Zone::Proxy,
            site: input.site.clone(),
            email: String::new(),
            password: String::new(),
            remember: false,
            error: None,
            auth: None,
            account: None,
            subscription: None,
            probe_input: input.probe_default.clone(),
            last_revision: 0,
            last_check: Instant::now(),
            last_refresh: Instant::now(),
            last_update: None,
            degraded: false,
            sync_state: SyncState::Idle,
            sync_started: Instant::now(),
            guest: GuestConfig::default(),
            reset_confirm: false,
            shop: ShopState::default(),
            pw_old: String::new(),
            pw_new: String::new(),
            proxy: SystemProxy::new(input.proxy_target.clone()),
        };
        context.spawn_background(|_| {
            std::thread::sleep(TICK);
            Message::Tick
        });
        // 登录页扩展位（失败降级为不显示）。
        let panel = shell.panel.clone();
        context.spawn_background(move |_| {
            let result = block_on_panel(async { panel.guest_config().await });
            match result {
                Ok(config) => Message::GuestLoaded(config),
                Err(_) => Message::GuestLoaded(GuestConfig::default()),
            }
        });
        // 「记住我」静默重登：凭据库有保存的邮箱+密码时启动即尝试。
        let remembered = shell
            .store
            .load(CREDENTIAL_SERVICE, "email")
            .ok()
            .flatten()
            .and_then(|email| {
                shell
                    .store
                    .load(CREDENTIAL_SERVICE, "password")
                    .ok()
                    .flatten()
                    .map(|password| (email, password))
            });
        if let Some((email, password)) = remembered {
            shell.email = email.expose().clone();
            shell.phase = Phase::Authenticating;
            let panel = shell.panel.clone();
            let email = shell.email.clone();
            context.spawn_background(move |_| {
                let result = block_on_panel(async {
                    let auth = panel.login(&email, &password).await?;
                    let account = panel.account(&auth).await?;
                    let subscription = panel.subscription(&auth).await?;
                    Ok(LoginPayload {
                        auth: Arc::new(auth),
                        account,
                        subscription: Arc::new(subscription),
                        persist_password: None,
                    })
                });
                Message::LoginDone(result.map_err(|e| e.to_string()))
            });
        }
        shell
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::Site(value) => self.site = value,
            Message::Email(value) => self.email = value,
            Message::Password(value) => self.password = value,
            Message::Remember(value) => self.remember = value,
            Message::GuestLoaded(config) => self.guest = config,
            Message::OpenUrl(url) => open_browser(&url),
            Message::Login => {
                if self.site.is_empty() || self.email.is_empty() || self.password.is_empty() {
                    self.error = Some("请填写站点、邮箱与密码".to_owned());
                    return;
                }
                self.phase = Phase::Authenticating;
                self.error = None;
                let panel = PanelClient::new(self.site.clone());
                self.panel = panel.clone();
                let email = self.email.clone();
                let password = Arc::new(Secret::new(std::mem::take(&mut self.password)));
                let remember = self.remember;
                context.spawn_background(move |_| {
                    let result = block_on_panel(async {
                        let auth = panel.login(&email, &password).await?;
                        let account = panel.account(&auth).await?;
                        let subscription = panel.subscription(&auth).await?;
                        Ok(LoginPayload {
                            auth: Arc::new(auth),
                            account,
                            subscription: Arc::new(subscription),
                            persist_password: if remember { Some(password) } else { None },
                        })
                    });
                    Message::LoginDone(result.map_err(|e| e.to_string()))
                });
            }
            Message::LoginDone(Ok(payload)) => {
                if let Some(password) = &payload.persist_password {
                    let email = Secret::new(self.email.clone());
                    let _ = self.store.store(CREDENTIAL_SERVICE, "email", &email);
                    let _ = self.store.store(CREDENTIAL_SERVICE, "password", password);
                }
                self.auth = Some(payload.auth);
                self.account = Some(payload.account);
                self.subscription = Some(Arc::clone(&payload.subscription));
                self.phase = Phase::Authenticated;
                self.degraded = false;
                self.last_update = Some(Instant::now());
                self.last_refresh = Instant::now();
                self.spawn_sync(context, payload.subscription);
            }
            Message::LoginDone(Err(error)) => {
                self.phase = Phase::LoggedOut;
                self.error = Some(error);
            }
            Message::Logout => {
                if let Some(auth) = self.auth.clone() {
                    let panel = self.panel.clone();
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async { panel.logout(&auth).await });
                        Message::LogoutDone(result.map_err(|e| e.to_string()))
                    });
                } else {
                    self.finish_logout();
                }
            }
            Message::LogoutDone(Err(error)) => {
                self.error = Some(error);
                self.finish_logout();
            }
            Message::LogoutDone(Ok(())) => self.finish_logout(),
            Message::Refresh => self.spawn_refresh(context),
            Message::RefreshDone(Ok((account, subscription))) => {
                self.account = Some(account);
                self.subscription = Some(Arc::clone(&subscription));
                self.degraded = false;
                self.last_update = Some(Instant::now());
                self.last_refresh = Instant::now();
                self.error = None;
                self.spawn_sync(context, subscription);
            }
            Message::RefreshDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::RefreshDone(Err(error)) => {
                // 离线降级：保留缓存 DTO，标记降级与上次更新时间。
                self.degraded = true;
                self.error = Some(error.to_string());
            }
            Message::SyncDone(Ok(SyncOutcome::Changed(seq))) => {
                self.sync_state = SyncState::Syncing(seq);
                self.sync_started = Instant::now();
                let _ = self.commands.send(Command::Reload);
            }
            Message::SyncDone(Ok(SyncOutcome::Unchanged)) => {
                let seq = match &self.sync_state {
                    SyncState::Synced(seq) | SyncState::Syncing(seq) => *seq,
                    _ => 0,
                };
                self.sync_state = SyncState::Synced(seq);
                self.degraded = false;
            }
            Message::SyncDone(Err(error)) => {
                self.sync_state = SyncState::Failed(error.clone());
                self.degraded = true;
                self.error = Some(error);
            }
            Message::ResetSecurity => {
                if !self.reset_confirm {
                    self.reset_confirm = true;
                    return;
                }
                self.reset_confirm = false;
                if let Some(auth) = self.auth.clone() {
                    let panel = self.panel.clone();
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async { panel.reset_security(&auth).await });
                        Message::ResetDone(result.map(Arc::new))
                    });
                }
            }
            Message::ResetDone(Ok(url)) => {
                if let Some(old) = &self.subscription {
                    let updated = SubscriptionView {
                        subscribe_url: Secret::new(url.expose().clone()),
                        expired_at: old.expired_at,
                        reset_day: old.reset_day,
                        used_bytes: old.used_bytes,
                        total_bytes: old.total_bytes,
                    };
                    let updated = Arc::new(updated);
                    self.subscription = Some(Arc::clone(&updated));
                    self.spawn_sync(context, updated);
                }
            }
            Message::ResetDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::ResetDone(Err(error)) => self.error = Some(error.to_string()),
            Message::SelectZone(tag) => {
                self.zone = match tag.as_deref() {
                    Some("subscription") => Zone::Subscription,
                    Some("shop") => Zone::Shop,
                    Some("account") => Zone::Account,
                    _ => Zone::Proxy,
                };
                if self.zone == Zone::Shop && !self.shop.loaded {
                    self.spawn_shop_init(context);
                }
            }
            Message::ProbeInput(value) => self.probe_input = value,
            Message::Probe => {
                let _ = self.commands.send(Command::probe(self.probe_input.clone()));
            }
            Message::ProbeNodes => {
                let _ = self.commands.send(Command::probe_nodes());
            }
            Message::LockNode(group, node) => {
                let _ = self.commands.send(Command::select(group, node));
            }
            Message::AutoNode(group) => {
                let _ = self.commands.send(Command::auto(group));
            }
            Message::SessionCheck(Ok(false)) => {
                self.finish_logout();
                self.error = Some("会话已被撤销，请重新登录".to_owned());
            }
            Message::SessionCheck(_) => {}
            Message::ShopLoaded(Ok((plans, methods, orders))) => {
                self.shop.plans = plans;
                self.shop.methods = methods;
                self.shop.orders = orders;
                self.shop.loaded = true;
                self.shop.plan_idx = 0;
                self.shop.period_idx = 0;
                self.shop.method_idx = 0;
            }
            Message::ShopLoaded(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::ShopLoaded(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::PlanSel(idx) => {
                self.shop.plan_idx = idx.unwrap_or(0);
                self.shop.period_idx = 0;
                self.shop.coupon_label = None;
            }
            Message::PeriodSel(idx) => self.shop.period_idx = idx.unwrap_or(0),
            Message::MethodSel(idx) => self.shop.method_idx = idx.unwrap_or(0),
            Message::CouponInput(code) => self.shop.coupon_code = code,
            Message::CouponCheck => {
                let Some(auth) = self.auth.clone() else {
                    return;
                };
                let Some(plan) = self.shop.plan() else {
                    return;
                };
                let panel = self.panel.clone();
                let code = self.shop.coupon_code.clone();
                let plan_id = plan.id;
                context.spawn_background(move |_| {
                    let result =
                        block_on_panel(async { panel.coupon_check(&auth, &code, plan_id).await });
                    Message::CouponDone(result)
                });
            }
            Message::CouponDone(Ok(label)) => self.shop.coupon_label = Some(label),
            Message::CouponDone(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::PlaceOrder => {
                let Some(auth) = self.auth.clone() else {
                    return;
                };
                let (Some(plan_id), Some(period), Some(method)) = (
                    self.shop.plan().map(|p| p.id),
                    self.shop.period().map(|(key, _, _)| key.clone()),
                    self.shop.method_id(),
                ) else {
                    self.shop.notice = Some("请选择套餐 / 周期 / 支付方式".to_owned());
                    return;
                };
                let coupon = if self.shop.coupon_code.is_empty() {
                    None
                } else {
                    Some(self.shop.coupon_code.clone())
                };
                let panel = self.panel.clone();
                self.shop.notice = None;
                context.spawn_background(move |_| {
                    let result = block_on_panel(async {
                        let trade_no = panel
                            .order_save(&auth, plan_id, &period, coupon.as_deref())
                            .await?;
                        let checkout = panel.order_checkout(&auth, &trade_no, method).await?;
                        Ok((trade_no, checkout))
                    });
                    Message::PlaceDone(result)
                });
            }
            Message::PlaceDone(Ok((trade_no, checkout))) => {
                tracing::info!(trade_no = %trade_no, kind = checkout.kind, "order placed");
                let qr = if checkout.kind == 0 {
                    qr_png(&checkout.data).ok().map(EncodedImage::new)
                } else {
                    None
                };
                if checkout.kind == 1 {
                    open_browser(&checkout.data);
                }
                let status = if checkout.kind == -1 { 3 } else { 0 };
                self.shop.order = Some(OrderSession {
                    trade_no,
                    status,
                    checkout: Some(checkout),
                    qr,
                    started: Instant::now(),
                    last_poll: Instant::now(),
                });
                if status == 3 {
                    self.shop.notice = Some("余额已全额支付，订阅刷新中".to_owned());
                    self.spawn_refresh(context);
                } else {
                    self.shop.notice = Some("订单已创建，等待支付…".to_owned());
                }
            }
            Message::PlaceDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::PlaceDone(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::PollDone(Ok(status)) => {
                tracing::info!(status, "order poll result");
                if let Some(order) = &mut self.shop.order {
                    order.status = status;
                    if status == 3 {
                        self.shop.notice = Some("支付完成，订阅已触发刷新".to_owned());
                        self.spawn_refresh(context);
                    } else if status == 2 {
                        self.shop.notice = Some("订单已取消".to_owned());
                        self.shop.order = None;
                    }
                }
            }
            Message::PollDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::PollDone(Err(error)) => {
                tracing::warn!(%error, "order poll failed");
                self.shop.notice = Some(error.to_string());
            }
            Message::CancelOrder(trade_no) => {
                let Some(auth) = self.auth.clone() else {
                    return;
                };
                let panel = self.panel.clone();
                context.spawn_background(move |_| {
                    let result =
                        block_on_panel(async { panel.order_cancel(&auth, &trade_no).await });
                    Message::CancelDone(result)
                });
            }
            Message::CancelDone(Ok(())) => {
                self.shop.notice = Some("订单已取消".to_owned());
                self.spawn_shop_init(context);
            }
            Message::CancelDone(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::ToggleRemindExpire(value) => {
                if let Some(account) = &mut self.account {
                    account.remind_expire = value;
                }
                self.spawn_settings(context);
            }
            Message::ToggleRemindTraffic(value) => {
                if let Some(account) = &mut self.account {
                    account.remind_traffic = value;
                }
                self.spawn_settings(context);
            }
            Message::ToggleAutoRenewal(value) => {
                if let Some(account) = &mut self.account {
                    account.auto_renewal = value;
                }
                self.spawn_settings(context);
            }
            Message::PwOld(value) => self.pw_old = value,
            Message::PwNew(value) => self.pw_new = value,
            Message::ChangePassword => {
                let Some(auth) = self.auth.clone() else {
                    return;
                };
                if self.pw_old.is_empty() || self.pw_new.len() < 8 {
                    self.error = Some("新密码至少 8 位".to_owned());
                    return;
                }
                let panel = self.panel.clone();
                let old = Arc::new(Secret::new(std::mem::take(&mut self.pw_old)));
                let new = Arc::new(Secret::new(std::mem::take(&mut self.pw_new)));
                context.spawn_background(move |_| {
                    let result =
                        block_on_panel(async { panel.change_password(&auth, &old, &new).await });
                    Message::ChangeDone(result)
                });
            }
            Message::ChangeDone(Ok(())) => {
                self.finish_logout();
                self.error = Some("密码已修改，全部会话已失效，请重新登录".to_owned());
            }
            Message::ChangeDone(Err(error)) => self.error = Some(error.to_string()),
            Message::SettingsDone(Ok(())) => {}
            Message::SettingsDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::SettingsDone(Err(error)) => self.error = Some(error.to_string()),
            Message::ToggleProxy => {
                let result = if self.proxy.is_taken() {
                    self.proxy.release()
                } else {
                    self.proxy.takeover()
                };
                if let Err(error) = result {
                    self.error = Some(format!("系统代理操作失败：{error}"));
                }
            }
            Message::Tick => {
                self.last_revision = self.shared.revision();
                // reload applied 事件到达 → 同步闭环完成。
                if let SyncState::Syncing(seq) = &self.sync_state {
                    let seq = *seq;
                    let applied = self
                        .shared
                        .snapshot()
                        .events_recent_first()
                        .into_iter()
                        .find(|e| e.kind == "reload" && e.summary.starts_with("applied"));
                    if applied.is_some_and(|event| event.received >= self.sync_started) {
                        self.sync_state = SyncState::Synced(seq);
                    }
                }
                if self.phase == Phase::Authenticated {
                    if self.last_check.elapsed() > SESSION_CHECK_INTERVAL {
                        self.last_check = Instant::now();
                        if let Some(auth) = self.auth.clone() {
                            let panel = self.panel.clone();
                            context.spawn_background(move |_| {
                                let result =
                                    block_on_panel(async { panel.check_login(&auth).await });
                                Message::SessionCheck(result.map_err(|e| e.to_string()))
                            });
                        }
                    }
                    if self.last_refresh.elapsed() > REFRESH_INTERVAL {
                        self.last_refresh = Instant::now();
                        self.spawn_refresh(context);
                    }
                    // A3 订单轮询：3 s 一次，上限 5 min。
                    let poll_now = self.shop.order.as_ref().is_some_and(|o| {
                        o.status == 0
                            && o.last_poll.elapsed() > Duration::from_secs(3)
                            && o.started.elapsed() < Duration::from_secs(300)
                    });
                    let timed_out = self.shop.order.as_ref().is_some_and(|o| {
                        o.status == 0 && o.started.elapsed() >= Duration::from_secs(300)
                    });
                    if poll_now {
                        if let Some(order) = &mut self.shop.order {
                            order.last_poll = Instant::now();
                        }
                        let trade_no = self.shop.order.as_ref().map(|o| o.trade_no.clone());
                        if let (Some(auth), Some(trade_no)) = (self.auth.clone(), trade_no) {
                            tracing::info!(trade_no = %trade_no, "order poll spawn");
                            let panel = self.panel.clone();
                            context.spawn_background(move |_| {
                                let result = block_on_panel(async {
                                    panel.order_check(&auth, &trade_no).await
                                });
                                Message::PollDone(result)
                            });
                        }
                    } else if timed_out {
                        self.shop.notice = Some("轮询超时：请在网站确认订单状态".to_owned());
                        self.shop.order = None;
                    }
                }
                context.spawn_background(|_| {
                    std::thread::sleep(TICK);
                    Message::Tick
                });
            }
        }
    }

    fn view(&self, _input: &GuiConfig, context: &mut ViewContext<Self>) -> View {
        context.window_title("McloudClient");
        match self.phase {
            Phase::Authenticated => self.home_view(context),
            _ => self.login_view(context),
        }
    }
}

impl Shell {
    fn finish_logout(&mut self) {
        self.auth = None;
        self.account = None;
        self.subscription = None;
        self.phase = Phase::LoggedOut;
        self.degraded = false;
        self.sync_state = SyncState::Idle;
        let _ = self.store.delete(CREDENTIAL_SERVICE, "email");
        let _ = self.store.delete(CREDENTIAL_SERVICE, "password");
    }

    fn spawn_sync(&self, context: &ComponentContext<Self>, subscription: Arc<SubscriptionView>) {
        let sync = Arc::clone(&self.sync);
        context.spawn_background(move |_| {
            let result = block_on_panel(async {
                let mut guard = sync.lock().await;
                guard
                    .sync_once(&subscription.subscribe_url)
                    .await
                    .map_err(|e| PanelError::Network(e.to_string()))
            });
            Message::SyncDone(result.map_err(|e| e.to_string()))
        });
    }

    fn spawn_refresh(&self, context: &ComponentContext<Self>) {
        let Some(auth) = self.auth.clone() else {
            return;
        };
        let panel = self.panel.clone();
        context.spawn_background(move |_| {
            let result = block_on_panel(async {
                let account = panel.account(&auth).await?;
                let subscription = panel.subscription(&auth).await?;
                Ok((account, Arc::new(subscription)))
            });
            Message::RefreshDone(result)
        });
    }

    fn spawn_settings(&self, context: &ComponentContext<Self>) {
        let Some(auth) = self.auth.clone() else {
            return;
        };
        let Some(account) = self.account.clone() else {
            return;
        };
        let panel = self.panel.clone();
        context.spawn_background(move |_| {
            let result = block_on_panel(async {
                panel
                    .update_settings(
                        &auth,
                        account.auto_renewal,
                        account.remind_expire,
                        account.remind_traffic,
                    )
                    .await
            });
            match result {
                Ok(()) => Message::SettingsDone(Ok(())),
                Err(error) => Message::SettingsDone(Err(error)),
            }
        });
    }

    fn spawn_shop_init(&self, context: &ComponentContext<Self>) {
        let Some(auth) = self.auth.clone() else {
            return;
        };
        let panel = self.panel.clone();
        context.spawn_background(move |_| {
            let result = block_on_panel(async {
                let plans = panel.plans(&auth).await?;
                let methods = panel.payment_methods(&auth).await?;
                let orders = panel.orders(&auth).await?;
                Ok((plans, methods, orders))
            });
            Message::ShopLoaded(result)
        });
    }

    fn login_view(&self, context: &mut ViewContext<Self>) -> View {
        let mut items: Vec<View> = vec![
            TextBlock::new().text("McloudClient").font_size(24.0).into(),
            TextBlock::new()
                .text("登录以同步订阅与账户")
                .font_size(12.0)
                .into(),
            TextBlock::new()
                .text("站点（高级设置）")
                .font_size(12.0)
                .into(),
            TextBox::new()
                .text(self.site.clone())
                .placeholder_text("https://panel.example.com")
                .on_text_changed(context.callback(Message::Site))
                .into(),
            TextBlock::new().text("邮箱").font_size(12.0).into(),
            TextBox::new()
                .text(self.email.clone())
                .placeholder_text("you@example.com")
                .on_text_changed(context.callback(Message::Email))
                .into(),
            TextBlock::new().text("密码").font_size(12.0).into(),
            PasswordBox::new()
                .password(self.password.clone())
                .placeholder_text("至少 8 位")
                .on_password_changed(context.callback(Message::Password))
                .into(),
            CheckBox::new()
                .is_checked(Some(self.remember))
                .on_is_checked_changed(context.callback(Message::Remember))
                .content("记住我（默认关闭，仅存系统凭据库）"),
            Button::new()
                .is_enabled(self.phase != Phase::Authenticating)
                .on_click(context.message(Message::Login))
                .content(if self.phase == Phase::Authenticating {
                    "登录中…"
                } else {
                    "登录"
                }),
        ];
        if let Some(tos) = &self.guest.tos_url {
            items.push(
                Button::new()
                    .on_click(context.message(Message::OpenUrl(tos.clone())))
                    .content("服务条款"),
            );
        }
        if let Some(app) = &self.guest.app_url {
            items.push(
                Button::new()
                    .on_click(context.message(Message::OpenUrl(app.clone())))
                    .content("注册 / 找回密码（浏览器）"),
            );
        }
        if self.guest.is_recaptcha {
            items.push(
                TextBlock::new()
                    .text("站点启用了人机验证：请在浏览器完成注册/找回流程")
                    .font_size(12.0)
                    .into(),
            );
        }
        if self.guest.is_email_verify {
            items.push(
                TextBlock::new()
                    .text("站点注册需邮箱验证码（浏览器流程）")
                    .font_size(12.0)
                    .into(),
            );
        }
        if let Some(error) = &self.error {
            items.push(TextBlock::new().text(error.clone()).font_size(12.0).into());
        }
        StackPanel::new()
            .spacing(12.0)
            .width(380.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .keyed_children(items.into_iter().enumerate())
    }

    fn home_view(&self, context: &mut ViewContext<Self>) -> View {
        let zone_view = match self.zone {
            Zone::Proxy => self.proxy_view(context),
            Zone::Subscription => self.subscription_view(context),
            Zone::Shop => self.shop_view(context),
            Zone::Account => self.account_view(context),
        };
        // MenuItems 槽必须用 collection_slot（fragment 会触发 WinUI StructureUnsupported）。
        let menu = vec![
            (
                "proxy",
                NavigationViewItem::new()
                    .tag("proxy")
                    .slot(NavigationViewItemSlot::Content, "代理"),
            ),
            (
                "subscription",
                NavigationViewItem::new()
                    .tag("subscription")
                    .slot(NavigationViewItemSlot::Content, "订阅"),
            ),
            (
                "shop",
                NavigationViewItem::new()
                    .tag("shop")
                    .slot(NavigationViewItemSlot::Content, "商店"),
            ),
            (
                "account",
                NavigationViewItem::new()
                    .tag("account")
                    .slot(NavigationViewItemSlot::Content, "账户"),
            ),
        ];
        NavigationView::new()
            .is_pane_open(true)
            .pane_title("McloudClient")
            .on_selected_tag_changed(context.callback(Message::SelectZone))
            .slots([
                SlotView::collection(NavigationViewSlot::MenuItems, menu),
                SlotView::new(NavigationViewSlot::Content, zone_view),
            ])
    }

    fn proxy_view(&self, context: &mut ViewContext<Self>) -> View {
        let snapshot = self.shared.snapshot();
        let link = match snapshot.link.status {
            LinkStatus::Connected => "内核已连接",
            LinkStatus::Connecting => "内核连接中…",
            LinkStatus::Disconnected => "内核未连接",
        };
        let mut kernel_items: Vec<View> = vec![kv("链路", link)];
        kernel_items.push(kv(
            "系统代理",
            if self.proxy.is_taken() {
                "已接管（退出时恢复原配置）"
            } else {
                "未接管"
            },
        ));
        kernel_items.push(
            Button::new()
                .on_click(context.message(Message::ToggleProxy))
                .content(if self.proxy.is_taken() {
                    "释放系统代理"
                } else {
                    "接管系统代理"
                }),
        );
        if let Some(metrics) = &snapshot.kernel {
            kernel_items.push(kv("运行时长", &fmt_uptime(metrics.uptime_s)));
            kernel_items.push(kv("规则数", &metrics.rules.to_string()));
            kernel_items.push(kv(
                "连接（活跃/累计）",
                &format!(
                    "{}/{}",
                    metrics.connections_active, metrics.connections_total
                ),
            ));
            kernel_items.push(kv(
                "流量（上/下）",
                &format!(
                    "{} / {}",
                    fmt_bytes(metrics.bytes_up),
                    fmt_bytes(metrics.bytes_down)
                ),
            ));
        }
        let mut node_items: Vec<View> = Vec::new();
        if let Some(nodes) = &snapshot.nodes {
            for group in &nodes.groups {
                node_items.push(kv(
                    &format!("策略组 {}", group.name),
                    &format!("{:?} · 选中 {}", group.mode, group.selected),
                ));
                let mut buttons: Vec<View> = vec![
                    Button::new()
                        .on_click(context.message(Message::AutoNode(group.name.clone())))
                        .content("自动"),
                ];
                for member in &group.members {
                    buttons.push(
                        Button::new()
                            .on_click(
                                context
                                    .message(Message::LockNode(group.name.clone(), member.clone())),
                            )
                            .content(format!("锁定 {member}")),
                    );
                }
                node_items.push(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .keyed_children(buttons.into_iter().enumerate()),
                );
            }
            for node in &nodes.nodes {
                let rtt = node
                    .tcp_rtt_ms
                    .map_or_else(|| "-".to_owned(), |v| format!("{v:.1} ms"));
                node_items.push(kv(&node.name, &rtt));
            }
        }
        if let Some(summary) = &snapshot.last_probe {
            node_items.push(kv("最近测速", summary));
        }
        let mut probe_items: Vec<View> = vec![
            TextBox::new()
                .text(self.probe_input.clone())
                .placeholder_text("host:port")
                .on_text_changed(context.callback(Message::ProbeInput))
                .into(),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .on_click(context.message(Message::Probe))
                        .content("探测"),
                    Button::new()
                        .on_click(context.message(Message::ProbeNodes))
                        .content("全节点测速"),
                )),
        ];
        for probe in snapshot.probes_recent_first().iter().take(5) {
            let status = if probe.reachable {
                format!(
                    "{} ms",
                    probe
                        .rtt_ms
                        .map_or_else(|| "-".to_owned(), |v| format!("{v:.1}"))
                )
            } else {
                probe
                    .error_code
                    .clone()
                    .unwrap_or_else(|| "unreachable".to_owned())
            };
            probe_items.push(kv(
                &format!("{} · {}", probe.address, fmt_age(probe.at.elapsed())),
                &status,
            ));
        }
        let mut event_items: Vec<View> = Vec::new();
        for event in snapshot.events_recent_first().iter().take(8) {
            event_items.push(kv(
                &format!("{} · {}", event.kind, fmt_age(event.received.elapsed())),
                &event.summary,
            ));
        }
        if event_items.is_empty() {
            event_items.push(kv("暂无事件", ""));
        }
        StackPanel::new().spacing(16.0).margin(24.0).children((
            panel("连接与内核", keyed(kernel_items)),
            panel("节点与负载", keyed(node_items)),
            panel("探测", keyed(probe_items)),
            panel("事件", keyed(event_items)),
        ))
    }

    fn subscription_view(&self, context: &mut ViewContext<Self>) -> View {
        let mut items: Vec<View> = Vec::new();
        let sync_label = match &self.sync_state {
            SyncState::Idle => "尚未同步".to_owned(),
            SyncState::Syncing(seq) => format!("同步中（快照 v{seq}，等待内核 applied）"),
            SyncState::Synced(seq) => format!("已同步（快照 v{seq}）"),
            SyncState::Failed(error) => format!("同步失败：{error}"),
        };
        items.push(kv("订阅同步", &sync_label));
        if let Some(updated) = self.last_update {
            items.push(kv(
                "上次更新",
                &format!(
                    "{} 前{}",
                    fmt_age(updated.elapsed()),
                    if self.degraded {
                        "（离线降级，显示缓存）"
                    } else {
                        ""
                    }
                ),
            ));
        }
        if let Some(subscription) = &self.subscription {
            let percent = if subscription.total_bytes > 0 {
                100.0 * subscription.used_bytes as f64 / subscription.total_bytes as f64
            } else {
                0.0
            };
            items.push(kv(
                "已用 / 总量",
                &format!(
                    "{} / {}",
                    fmt_bytes(subscription.used_bytes),
                    fmt_bytes(subscription.total_bytes)
                ),
            ));
            items.push(ProgressBar::new().value(percent).maximum(100.0).into());
            items.push(kv("重置日", &format!("每月 {} 日", subscription.reset_day)));
            let days = (subscription.expired_at - unix_now()) / 86_400;
            items.push(kv("到期剩余", &format!("{days} 天")));
        } else {
            items.push(kv("订阅", "尚未同步"));
        }
        items.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .on_click(context.message(Message::Refresh))
                        .content("手动刷新"),
                    Button::new()
                        .on_click(context.message(Message::ResetSecurity))
                        .content(if self.reset_confirm {
                            "确认重置？旧链接立即失效"
                        } else {
                            "重置订阅链接"
                        }),
                )),
        );
        if let Some(error) = &self.error {
            items.push(TextBlock::new().text(error.clone()).font_size(12.0).into());
        }
        StackPanel::new()
            .spacing(12.0)
            .margin(24.0)
            .children((panel("订阅", keyed(items)),))
    }

    fn account_view(&self, context: &mut ViewContext<Self>) -> View {
        let mut items: Vec<View> = Vec::new();
        if let Some(account) = &self.account {
            items.push(kv("邮箱", &account.email_masked));
            items.push(kv(
                "余额",
                &format!("¥{:.2}", account.balance_cents as f64 / 100.0),
            ));
            items.push(kv("套餐 ID", &account.plan_id.to_string()));
            items.push(kv(
                "流量（已用/总量）",
                &format!(
                    "{} / {}",
                    fmt_bytes(account.used_bytes),
                    fmt_bytes(account.total_bytes)
                ),
            ));
            let days = (account.expired_at - unix_now()) / 86_400;
            items.push(kv("到期剩余", &format!("{days} 天")));
            items.push(
                CheckBox::new()
                    .is_checked(Some(account.remind_expire))
                    .on_is_checked_changed(context.callback(Message::ToggleRemindExpire))
                    .content("到期提醒"),
            );
            items.push(
                CheckBox::new()
                    .is_checked(Some(account.remind_traffic))
                    .on_is_checked_changed(context.callback(Message::ToggleRemindTraffic))
                    .content("流量提醒"),
            );
            items.push(
                CheckBox::new()
                    .is_checked(Some(account.auto_renewal))
                    .on_is_checked_changed(context.callback(Message::ToggleAutoRenewal))
                    .content("自动续费"),
            );
        }
        items.push(
            TextBlock::new()
                .text("修改密码（改密后全部会话失效）")
                .font_size(13.0)
                .into(),
        );
        items.push(
            PasswordBox::new()
                .password(self.pw_old.clone())
                .placeholder_text("旧密码")
                .on_password_changed(context.callback(Message::PwOld))
                .into(),
        );
        items.push(
            PasswordBox::new()
                .password(self.pw_new.clone())
                .placeholder_text("新密码（至少 8 位）")
                .on_password_changed(context.callback(Message::PwNew))
                .into(),
        );
        items.push(
            Button::new()
                .on_click(context.message(Message::ChangePassword))
                .content("修改密码"),
        );
        if let Some(updated) = self.last_update {
            items.push(kv(
                "上次更新",
                &format!(
                    "{} 前{}",
                    fmt_age(updated.elapsed()),
                    if self.degraded {
                        "（离线降级，显示缓存）"
                    } else {
                        ""
                    }
                ),
            ));
        }
        items.push(
            Button::new()
                .on_click(context.message(Message::Logout))
                .content("登出并清除已保存凭据"),
        );
        StackPanel::new()
            .spacing(12.0)
            .margin(24.0)
            .children((panel("账户", keyed(items)),))
    }

    fn shop_view(&self, context: &mut ViewContext<Self>) -> View {
        let shop = &self.shop;
        let mut items: Vec<View> = Vec::new();
        if !shop.loaded {
            items.push(TextBlock::new().text("商店加载中…").font_size(13.0).into());
        } else {
            let plan_names: Vec<String> = shop.plans.iter().map(|p| p.name.clone()).collect();
            items.push(
                ComboBox::new()
                    .items_source(plan_names)
                    .selected_index(shop.plan_idx)
                    .on_selection_changed(context.callback(Message::PlanSel))
                    .into(),
            );
            if let Some(plan) = shop.plan() {
                items.push(kv("套餐", &plan.name));
                items.push(
                    TextBlock::new()
                        .text(format!(
                            "{} · {}",
                            fmt_bytes(plan.transfer_enable),
                            if plan.content.is_empty() {
                                "无描述"
                            } else {
                                &plan.content
                            }
                        ))
                        .font_size(12.0)
                        .into(),
                );
                items.push(kv(
                    "续费 / 名额",
                    &format!(
                        "{} / {}",
                        if plan.renew {
                            "可续费"
                        } else {
                            "不可续费"
                        },
                        plan.capacity_limit
                            .map_or_else(|| "不限".to_owned(), |n| format!("剩 {n}"))
                    ),
                ));
                let period_labels: Vec<String> = plan
                    .prices
                    .iter()
                    .map(|(_, label, cents)| format!("{label} ¥{:.2}", *cents as f64 / 100.0))
                    .collect();
                if let Some((_, label, cents)) = shop.period() {
                    items.push(kv(
                        "当前周期",
                        &format!("{label} ¥{:.2}", *cents as f64 / 100.0),
                    ));
                }
                items.push(
                    ComboBox::new()
                        .items_source(period_labels)
                        .selected_index(shop.period_idx)
                        .on_selection_changed(context.callback(Message::PeriodSel))
                        .into(),
                );
            }
            items.push(
                TextBox::new()
                    .text(shop.coupon_code.clone())
                    .placeholder_text("优惠券码（可选）")
                    .on_text_changed(context.callback(Message::CouponInput))
                    .into(),
            );
            items.push(
                Button::new()
                    .on_click(context.message(Message::CouponCheck))
                    .content("校验优惠券"),
            );
            if let Some(label) = &shop.coupon_label {
                items.push(TextBlock::new().text(label.clone()).font_size(12.0).into());
            }
            let method_names: Vec<String> = shop
                .methods
                .iter()
                .map(|m| format!("{} · {}", m.name, m.payment))
                .collect();
            if let Some(method) = shop.methods.get(shop.method_idx) {
                items.push(kv(
                    "支付渠道",
                    &format!("{}（{}）", method.name, method.payment),
                ));
            }
            items.push(
                ComboBox::new()
                    .items_source(method_names)
                    .selected_index(shop.method_idx)
                    .on_selection_changed(context.callback(Message::MethodSel))
                    .into(),
            );
            items.push(
                Button::new()
                    .on_click(context.message(Message::PlaceOrder))
                    .content("下单"),
            );
        }
        if let Some(order) = &shop.order {
            let mut order_items: Vec<View> = vec![
                kv("订单号", &order.trade_no),
                kv("状态", order_status_label(order.status)),
            ];
            if let Some(qr) = &order.qr {
                order_items.push(
                    Image::new()
                        .source_data(qr.clone())
                        .width(240.0)
                        .height(240.0)
                        .into(),
                );
                order_items.push(
                    TextBlock::new()
                        .text("扫码支付（二维码仅含付款地址）")
                        .font_size(12.0)
                        .into(),
                );
            }
            if let Some(checkout) = order.checkout.as_ref().filter(|c| c.kind == 1) {
                order_items.push(
                    Button::new()
                        .on_click(context.message(Message::OpenUrl(checkout.data.clone())))
                        .content("打开支付页面"),
                );
            }
            if order.status == 0 {
                order_items.push(
                    Button::new()
                        .on_click(context.message(Message::CancelOrder(order.trade_no.clone())))
                        .content(format!("取消 {}", order.trade_no)),
                );
            }
            items.push(panel("当前订单", keyed(order_items)));
        }
        if !shop.orders.is_empty() {
            let mut order_rows: Vec<View> = Vec::new();
            for order in shop.orders.iter().take(6) {
                order_rows.push(kv(
                    &format!("{} · {}", order.trade_no, period_label(&order.period)),
                    &format!(
                        "{} · ¥{:.2}",
                        order_status_label(order.status),
                        order.total_amount as f64 / 100.0
                    ),
                ));
                if order.status == 0 {
                    order_rows.push(
                        Button::new()
                            .on_click(context.message(Message::CancelOrder(order.trade_no.clone())))
                            .content(format!("取消 {}", order.trade_no)),
                    );
                }
            }
            items.push(panel("历史订单", keyed(order_rows)));
        }
        if let Some(notice) = &shop.notice {
            items.push(TextBlock::new().text(notice.clone()).font_size(12.0).into());
        }
        StackPanel::new()
            .spacing(12.0)
            .margin(24.0)
            .children((panel("商店", keyed(items)),))
    }
}

fn order_status_label(status: i64) -> &'static str {
    match status {
        0 => "待支付",
        1 => "已支付",
        2 => "已取消",
        3 => "已完成",
        4 => "处理中",
        _ => "未知",
    }
}

fn kv(key: &str, value: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(16.0)
        .children((
            TextBlock::new().text(key).font_size(13.0),
            TextBlock::new().text(value).font_size(13.0),
        ))
}

fn panel(title: &str, body: View) -> View {
    StackPanel::new()
        .spacing(8.0)
        .children((TextBlock::new().text(title).font_size(14.0), body))
}

fn keyed(items: Vec<View>) -> View {
    StackPanel::new()
        .spacing(6.0)
        .keyed_children(items.into_iter().enumerate())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 系统浏览器打开 URL（L3）：严格校验 scheme 与字符集后才交给 shell。
fn open_browser(url: &str) {
    let safe = (url.starts_with("https://") || url.starts_with("http://"))
        && !url
            .chars()
            .any(|c| c.is_whitespace() || "\"&|<>^%!".contains(c));
    if !safe {
        tracing::warn!("refused to open unsafe url");
        return;
    }
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

/// 在后台线程里跑面板异步调用（reactor 的 spawn_background 非 async）。
fn block_on_panel<F, T>(future: F) -> Result<T, PanelError>
where
    F: std::future::Future<Output = Result<T, PanelError>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| PanelError::Network(error.to_string()))?;
    runtime.block_on(future)
}

/// 运行 GUI（Windows）。
///
/// # Errors
///
/// reactor 运行时失败时返回错误。
pub fn run(handles: GuiHandles, config: GuiConfig) -> Result<(), Box<dyn std::error::Error>> {
    let _ = HANDLES.set(handles);
    App::run_component::<Shell>(config)
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
}
