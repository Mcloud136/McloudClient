//! reactor master（ADR 0006）双层架构：
//! - `TrayApp`（Application）：持有全部会话状态 + 原生托盘（notify_icon/菜单/show_window）；
//!   窗口关闭 = 隐藏到托盘（窗口声明移除，状态保留），托盘点击重建窗口。
//! - `MainWindow`（Component）：视图 + 异步 spawn（面板/同步/轮询），消息经 forward 回 Application。
//!
//! 凭据边界（ADR 0004 L1）：`Secret` 只在控制面持有；窗口输入仅含脱敏视图模型与
//! 必要的凭据 Arc（手动 PartialEq 按视图模型 reconciliate）。

use std::rc::Rc;
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
use crate::shop::{ShopState, qr_png};
use crate::state::{Command, LinkStatus, SharedState, fmt_age, fmt_bytes, fmt_uptime};
use crate::sync::{SubscriptionSync, SyncOutcome};
use crate::task::TaskHandle;

const CREDENTIAL_SERVICE: &str = "mcloud-client";
const TICK: Duration = Duration::from_millis(250);
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// 重句柄：经 OnceLock 传入（Application/Component 的 Input 要求 Clone+PartialEq）。
pub struct GuiHandles {
    pub shared: Arc<SharedState>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub task: TaskHandle,
    pub panel: PanelClient,
    pub store: Arc<dyn CredentialStore>,
    pub sync: Arc<AsyncMutex<SubscriptionSync>>,
}

static HANDLES: std::sync::OnceLock<GuiHandles> = std::sync::OnceLock::new();

fn handles() -> &'static GuiHandles {
    HANDLES.get().expect("gui handles set before run")
}

/// 轻配置：Application Input。
#[derive(Clone, PartialEq)]
pub struct GuiConfig {
    pub site: String,
    pub probe_default: String,
    pub proxy_target: String,
    pub update_manifest: String,
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

#[derive(Clone, PartialEq)]
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
    LogoutDone,
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
    ExportLogs,
    RulesInput(String),
    RulesPrefill(String),
    ApplyRules,
    RulesDone(Result<u64, String>),
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
    PlaceDone(Result<(String, CheckoutView, Option<Vec<u8>>), PanelError>),
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
    CheckUpdate,
    UpdateChecked(Result<Option<crate::update::UpdateInfo>, String>),
    ApplyUpdate,
    UpdateReady(Result<std::path::PathBuf, String>),
    WindowClosed,
    TrayActivate,
    TrayMenu(String),
    Tick,
}

/// 脱敏视图模型（窗口输入；不含凭据明文）。
#[derive(Clone, PartialEq)]
struct ViewModel {
    phase: Phase,
    zone: Zone,
    site: String,
    email: String,
    password: String,
    remember: bool,
    error: Option<String>,
    guest: GuestConfig,
    account: Option<AccountView>,
    sub: Option<SubDto>,
    shop: ShopVm,
    probe_input: String,
    rules_text: String,
    rules_status: String,
    sync_label: String,
    update_label: String,
    proxy_taken: bool,
    reset_confirm: bool,
    pw_old: String,
    pw_new: String,
    link: LinkStatus,
    kernel: Option<crate::state::MetricsView>,
    nodes: Option<crate::state::NodesView>,
    last_probe: Option<String>,
    log_export: Option<String>,
    probes: Vec<crate::state::ProbeView>,
    events: Vec<crate::state::EventView>,
    last_refresh: Instant,
    last_check: Instant,
    update_manifest: String,
    update_info: Option<crate::update::UpdateInfo>,
    update_status: String,
    update_check_requested: bool,
}

#[derive(Clone, PartialEq)]
struct SubDto {
    expired_at: i64,
    reset_day: i64,
    used_bytes: u64,
    total_bytes: u64,
}

#[derive(Clone, PartialEq)]
struct ShopVm {
    loaded: bool,
    plans: Vec<PlanView>,
    methods: Vec<PaymentMethodView>,
    orders: Vec<OrderView>,
    plan_idx: usize,
    period_idx: usize,
    method_idx: usize,
    coupon_code: String,
    coupon_label: Option<String>,
    notice: Option<String>,
    order: Option<OrderVm>,
}

#[derive(Clone, PartialEq)]
struct OrderVm {
    trade_no: String,
    status: i64,
    kind: i64,
    qr: Option<Vec<u8>>,
    started: Instant,
    last_poll: Instant,
}

/// 窗口输入：视图模型 + 凭据 Arc（手动 PartialEq：按视图模型 + 凭据指针 reconciliate）。
#[derive(Clone)]
struct WindowInput {
    vm: ViewModel,
    auth: Option<Arc<Secret<String>>>,
    sub_url: Option<Arc<Secret<String>>>,
    send: Callback<Message>,
    closed: Callback<()>,
}

impl PartialEq for WindowInput {
    fn eq(&self, other: &Self) -> bool {
        self.vm == other.vm
            && match (&self.auth, &other.auth) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
            && match (&self.sub_url, &other.sub_url) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// Application 层：状态 + 托盘 + 窗口生命周期。
struct TrayApp {
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
    rules_text: String,
    rules_status: String,
    rules_started: Option<Instant>,
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
    window_visible: bool,
    exiting: bool,
    update_manifest: String,
    update_info: Option<crate::update::UpdateInfo>,
    update_status: String,
    update_check_requested: bool,
}

impl Application for TrayApp {
    type Input = GuiConfig;
    type Message = Message;

    fn create(input: &GuiConfig, context: &ApplicationContext<Self>) -> Self {
        let app = Self {
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
            rules_text: String::new(),
            rules_status: String::new(),
            rules_started: None,
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
            window_visible: true,
            exiting: false,
            update_manifest: input.update_manifest.clone(),
            update_info: None,
            update_status: String::new(),
            update_check_requested: false,
        };
        context.show_window("main");
        app
    }

    fn update(&mut self, message: Message, context: &ApplicationContext<Self>) {
        match message {
            Message::WindowClosed => {
                if !self.exiting {
                    self.window_visible = false;
                }
            }
            Message::TrayActivate => {
                self.window_visible = true;
                context.show_window("main");
            }
            Message::TrayMenu(key) => match key.as_str() {
                "exit" => {
                    self.exiting = true;
                    context.exit();
                }
                "update" => {
                    self.window_visible = true;
                    context.show_window("main");
                    self.update_status = "正在检查更新…".to_owned();
                    self.update_check_requested = true;
                }
                _ => {
                    self.window_visible = true;
                    context.show_window("main");
                }
            },
            Message::Site(value) => self.site = value,
            Message::Email(value) => self.email = value,
            Message::Password(value) => self.password = value,
            Message::Remember(value) => self.remember = value,
            Message::GuestLoaded(config) => self.guest = config,
            Message::OpenUrl(url) => open_browser(&url),
            Message::LoginDone(Ok(payload)) => {
                if let Some(password) = &payload.persist_password {
                    let email = Secret::new(self.email.clone());
                    let _ = handles().store.store(CREDENTIAL_SERVICE, "email", &email);
                    let _ = handles()
                        .store
                        .store(CREDENTIAL_SERVICE, "password", password);
                }
                self.auth = Some(payload.auth);
                self.account = Some(payload.account);
                self.subscription = Some(payload.subscription);
                self.phase = Phase::Authenticated;
                self.degraded = false;
                self.last_update = Some(Instant::now());
                self.last_refresh = Instant::now();
            }
            Message::LoginDone(Err(error)) => {
                self.phase = Phase::LoggedOut;
                self.error = Some(error);
            }
            Message::LogoutDone => self.finish_logout(),
            Message::RefreshDone(Ok((account, subscription))) => {
                self.account = Some(account);
                self.subscription = Some(subscription);
                self.degraded = false;
                self.last_update = Some(Instant::now());
                self.last_refresh = Instant::now();
                self.error = None;
            }
            Message::RefreshDone(Err(PanelError::Expired)) => {
                self.finish_logout();
                self.error = Some("登录已失效，请重新登录".to_owned());
            }
            Message::RefreshDone(Err(error)) => {
                self.degraded = true;
                self.last_refresh = Instant::now();
                self.error = Some(error.to_string());
            }
            Message::SyncDone(Ok(SyncOutcome::Changed(seq))) => {
                self.sync_state = SyncState::Syncing(seq);
                self.sync_started = Instant::now();
                let _ = handles().commands.send(Command::Reload);
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
                // 二次确认：第一次置标志（窗口层拦截第二次并 spawn）。
                self.reset_confirm = true;
            }
            Message::ResetDone(Ok(_)) => {
                self.reset_confirm = false;
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
            }
            Message::ProbeInput(value) => self.probe_input = value,
            Message::Probe => {
                let _ = handles()
                    .commands
                    .send(Command::probe(self.probe_input.clone()));
            }
            Message::ProbeNodes => {
                let _ = handles().commands.send(Command::probe_nodes());
            }
            Message::ExportLogs => {
                let _ = handles().commands.send(Command::ExportLogs);
            }
            Message::RulesInput(value) => self.rules_text = value,
            Message::RulesPrefill(text) => {
                // 启动回显：仅当文本框为空时填入已保存用户规则，不覆盖用户正在编辑的内容。
                if self.rules_text.is_empty() {
                    self.rules_text = text;
                }
            }
            Message::RulesDone(Ok(seq)) => {
                self.rules_status = format!("规则已应用（快照 v{seq}），等待内核生效");
                self.rules_started = Some(Instant::now());
            }
            Message::RulesDone(Err(error)) => {
                self.rules_status = format!("规则未应用：{error}");
            }
            Message::LockNode(group, node) => {
                let _ = handles().commands.send(Command::select(group, node));
            }
            Message::AutoNode(group) => {
                let _ = handles().commands.send(Command::auto(group));
            }
            Message::SessionCheck(Ok(false)) => {
                self.finish_logout();
                self.error = Some("会话已被撤销，请重新登录".to_owned());
            }
            Message::SessionCheck(_) => {
                self.last_check = Instant::now();
            }
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
            Message::CouponDone(Ok(label)) => self.shop.coupon_label = Some(label),
            Message::CouponDone(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::PlaceDone(Ok((trade_no, checkout, qr))) => {
                let status = if checkout.kind == -1 { 3 } else { 0 };
                self.shop.order = Some(crate::shop::OrderSession {
                    trade_no,
                    status,
                    checkout: Some(checkout),
                    qr,
                    started: Instant::now(),
                    last_poll: Instant::now(),
                });
                if status == 3 {
                    self.shop.notice = Some("余额已全额支付，订阅刷新中".to_owned());
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
                if let Some(order) = &mut self.shop.order {
                    order.status = status;
                    order.last_poll = Instant::now();
                    if status == 3 {
                        self.shop.notice = Some("支付完成，订阅已触发刷新".to_owned());
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
                if let Some(order) = &mut self.shop.order {
                    order.last_poll = Instant::now();
                }
                self.shop.notice = Some(error.to_string());
            }
            Message::CancelDone(Ok(())) => {
                self.shop.notice = Some("订单已取消".to_owned());
            }
            Message::CancelDone(Err(error)) => self.shop.notice = Some(error.to_string()),
            Message::ToggleRemindExpire(value) => {
                if let Some(account) = &mut self.account {
                    account.remind_expire = value;
                }
            }
            Message::ToggleRemindTraffic(value) => {
                if let Some(account) = &mut self.account {
                    account.remind_traffic = value;
                }
            }
            Message::ToggleAutoRenewal(value) => {
                if let Some(account) = &mut self.account {
                    account.auto_renewal = value;
                }
            }
            Message::PwOld(value) => self.pw_old = value,
            Message::PwNew(value) => self.pw_new = value,
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
            Message::UpdateChecked(Ok(info)) => {
                self.update_check_requested = false;
                self.update_info = info;
                self.update_status = if self.update_info.is_some() {
                    String::new()
                } else {
                    "已是最新版本".to_owned()
                };
            }
            Message::UpdateChecked(Err(error)) => {
                self.update_check_requested = false;
                self.update_status = format!("检查更新失败：{error}");
            }
            Message::UpdateReady(Ok(path)) => {
                tracing::info!(path = %path.display(), "launching installer");
                if let Err(error) = crate::update::launch_installer(&path) {
                    self.update_status = format!("启动安装器失败：{error}");
                } else {
                    self.exiting = true;
                    context.exit();
                }
            }
            Message::UpdateReady(Err(error)) => {
                self.update_status = format!("更新包校验失败：{error}");
            }
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
                // reload applied 事件到达 → 同步闭环完成。
                if let SyncState::Syncing(seq) = &self.sync_state {
                    let seq = *seq;
                    let applied = handles()
                        .shared
                        .snapshot()
                        .events_recent_first()
                        .into_iter()
                        .find(|e| e.kind == "reload" && e.summary.starts_with("applied"));
                    if applied.is_some_and(|event| event.received >= self.sync_started) {
                        self.sync_state = SyncState::Synced(seq);
                    }
                }
                // 规则编辑生效终态：reload applied 事件晚于规则应用时刻 → 已生效。
                if let Some(started) = self.rules_started {
                    let applied = handles()
                        .shared
                        .snapshot()
                        .events_recent_first()
                        .into_iter()
                        .find(|e| e.kind == "reload" && e.summary.starts_with("applied"));
                    if applied.is_some_and(|event| event.received >= started) {
                        self.rules_status = "规则已生效（内核已重载）".to_owned();
                        self.rules_started = None;
                    }
                }
            }
            // 窗口层拦截处理的命令类消息不会到达这里。
            Message::Login
            | Message::Logout
            | Message::Refresh
            | Message::CouponCheck
            | Message::PlaceOrder
            | Message::CancelOrder(_)
            | Message::CheckUpdate
            | Message::ApplyUpdate
            | Message::ApplyRules
            | Message::ChangePassword => {}
        }
    }

    fn view(&self, _: &GuiConfig, context: &ApplicationViewContext<Self>) -> ApplicationView {
        let mut view = ApplicationView::new();
        if self.window_visible {
            view = view.window::<MainWindow>(
                "main",
                WindowInput {
                    vm: self.project(),
                    auth: self.auth.clone(),
                    sub_url: self
                        .subscription
                        .as_ref()
                        .map(|s| Arc::new(Secret::new(s.subscribe_url.expose().clone()))),
                    send: context.forward(),
                    closed: context.message(Message::WindowClosed),
                },
            );
        }
        view.notify_icon(
            "tray",
            NotifyIcon::new(icon_path())
                .tooltip("McloudClient")
                .on_activate(context.callback(|_| Message::TrayActivate))
                .menu(Menu::new(
                    [
                        MenuItem::item("open", "打开主窗口"),
                        MenuItem::item("update", "检查更新"),
                        MenuItem::separator("sep"),
                        MenuItem::item("exit", "退出"),
                    ],
                    context.callback(|key: Key| {
                        Message::TrayMenu(key.as_str().unwrap_or_default().to_owned())
                    }),
                )),
        )
    }
}

impl Drop for TrayApp {
    fn drop(&mut self) {
        if let Some(handles) = HANDLES.get() {
            handles.task.stop();
        }
    }
}

impl TrayApp {
    fn finish_logout(&mut self) {
        self.auth = None;
        self.account = None;
        self.subscription = None;
        self.phase = Phase::LoggedOut;
        self.degraded = false;
        self.sync_state = SyncState::Idle;
        let _ = handles().store.delete(CREDENTIAL_SERVICE, "email");
        let _ = handles().store.delete(CREDENTIAL_SERVICE, "password");
    }

    fn project(&self) -> ViewModel {
        let snapshot = handles().shared.snapshot();
        let sync_label = match &self.sync_state {
            SyncState::Idle => "尚未同步".to_owned(),
            SyncState::Syncing(seq) => format!("同步中（快照 v{seq}，等待内核 applied）"),
            SyncState::Synced(seq) => format!("已同步（快照 v{seq}）"),
            SyncState::Failed(error) => format!("同步失败：{error}"),
        };
        let update_label = self.last_update.map_or_else(String::new, |updated| {
            format!(
                "{} 前{}",
                fmt_age(updated.elapsed()),
                if self.degraded {
                    "（离线降级，显示缓存）"
                } else {
                    ""
                }
            )
        });
        ViewModel {
            phase: self.phase,
            zone: self.zone,
            site: self.site.clone(),
            email: self.email.clone(),
            password: self.password.clone(),
            remember: self.remember,
            error: self.error.clone(),
            guest: self.guest.clone(),
            account: self.account.clone(),
            sub: self.subscription.as_ref().map(|s| SubDto {
                expired_at: s.expired_at,
                reset_day: s.reset_day,
                used_bytes: s.used_bytes,
                total_bytes: s.total_bytes,
            }),
            shop: ShopVm {
                loaded: self.shop.loaded,
                plans: self.shop.plans.clone(),
                methods: self.shop.methods.clone(),
                orders: self.shop.orders.clone(),
                plan_idx: self.shop.plan_idx,
                period_idx: self.shop.period_idx,
                method_idx: self.shop.method_idx,
                coupon_code: self.shop.coupon_code.clone(),
                coupon_label: self.shop.coupon_label.clone(),
                notice: self.shop.notice.clone(),
                order: self.shop.order.as_ref().map(|o| OrderVm {
                    trade_no: o.trade_no.clone(),
                    status: o.status,
                    kind: o.checkout.as_ref().map_or(1, |c| c.kind),
                    qr: o.qr.clone(),
                    started: o.started,
                    last_poll: o.last_poll,
                }),
            },
            probe_input: self.probe_input.clone(),
            rules_text: self.rules_text.clone(),
            rules_status: self.rules_status.clone(),
            sync_label,
            update_label,
            proxy_taken: self.proxy.is_taken(),
            reset_confirm: self.reset_confirm,
            pw_old: self.pw_old.clone(),
            pw_new: self.pw_new.clone(),
            link: snapshot.link.status,
            kernel: snapshot.kernel.clone().map(|m| crate::state::MetricsView {
                uptime_s: m.uptime_s,
                rules: m.rules,
                connections_active: m.connections_active,
                connections_total: m.connections_total,
                bytes_up: m.bytes_up,
                bytes_down: m.bytes_down,
            }),
            nodes: snapshot.nodes.clone().map(|n| crate::state::NodesView {
                groups: n
                    .groups
                    .iter()
                    .map(|g| crate::state::GroupView {
                        name: g.name.clone(),
                        mode: format!("{:?}", g.mode),
                        selected: g.selected.clone(),
                        members: g.members.clone(),
                    })
                    .collect(),
                nodes: n
                    .nodes
                    .iter()
                    .map(|n| crate::state::NodeRow {
                        name: n.name.clone(),
                        rtt_ms: n.tcp_rtt_ms,
                    })
                    .collect(),
            }),
            last_probe: snapshot.last_probe.clone(),
            log_export: snapshot.log_export.clone(),
            probes: snapshot
                .probes_recent_first()
                .into_iter()
                .take(5)
                .map(|p| crate::state::ProbeView {
                    address: p.address,
                    reachable: p.reachable,
                    rtt_ms: p.rtt_ms,
                    error_code: p.error_code,
                    age: fmt_age(p.at.elapsed()),
                })
                .collect(),
            events: snapshot
                .events_recent_first()
                .into_iter()
                .take(8)
                .map(|e| crate::state::EventView {
                    kind: e.kind,
                    summary: e.summary,
                    age: fmt_age(e.received.elapsed()),
                })
                .collect(),
            last_refresh: self.last_refresh,
            last_check: self.last_check,
            update_manifest: self.update_manifest.clone(),
            update_info: self.update_info.clone(),
            update_status: self.update_status.clone(),
            update_check_requested: self.update_check_requested,
        }
    }
}

/// 窗口层：视图 + 异步 spawn + 消息转发。
struct MainWindow {
    closed: Callback<()>,
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        self.closed.call(());
    }
}

impl Component for MainWindow {
    type Input = WindowInput;
    type Message = Message;

    fn create(input: &WindowInput, context: &ComponentContext<Self>) -> Self {
        context.spawn_background(|_| {
            std::thread::sleep(TICK);
            Message::Tick
        });
        if input.vm.update_check_requested {
            let url = input.vm.update_manifest.clone();
            context.spawn_background(move |_| {
                let result = block_on_panel(async { crate::update::check(&url).await });
                Message::UpdateChecked(result.map_err(|e| e.to_string()))
            });
        }
        let panel = handles().panel.clone();
        context.spawn_background(move |_| {
            let result = block_on_panel(async { panel.guest_config().await });
            match result {
                Ok(config) => Message::GuestLoaded(config),
                Err(_) => Message::GuestLoaded(GuestConfig::default()),
            }
        });
        let store = Arc::clone(&handles().store);
        let remembered = store
            .load(CREDENTIAL_SERVICE, "email")
            .ok()
            .flatten()
            .and_then(|email| {
                store
                    .load(CREDENTIAL_SERVICE, "password")
                    .ok()
                    .flatten()
                    .map(|password| (email, password))
            });
        if let Some((email, password)) = remembered {
            input.send.call(Message::Email(email.expose().clone()));
            let panel = handles().panel.clone();
            let email = email.expose().clone();
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
        // 规则回显：启动时读已保存用户规则预填文本框（仅空时生效，见 RulesPrefill）。
        let sync = Arc::clone(&handles().sync);
        context.spawn_background(move |_| {
            let text = block_on_panel(async {
                let guard = sync.lock().await;
                let dir = guard.dir().to_path_buf();
                let user = crate::rules_edit::load_user_rules(&dir)
                    .map_err(|e| PanelError::Network(e.to_string()))?;
                Ok(user.rules.join("\n"))
            });
            match text {
                Ok(t) if !t.is_empty() => Message::RulesPrefill(t),
                _ => Message::RulesPrefill(String::new()),
            }
        });
        Self {
            closed: input.closed.clone(),
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        let input = context.input().clone();
        match message {
            Message::Login => {
                let email = input.vm.email.clone();
                let password = Arc::new(Secret::new(input.vm.password.clone()));
                let remember = input.vm.remember;
                let site = input.vm.site.clone();
                context.spawn_background(move |_| {
                    let panel = PanelClient::new(site);
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
                spawn_sync(
                    context,
                    Arc::new(Secret::new(
                        payload.subscription.subscribe_url.expose().clone(),
                    )),
                );
                input.send.call(Message::LoginDone(Ok(payload)));
            }
            Message::Logout => {
                if let Some(auth) = input.auth.clone() {
                    let panel = handles().panel.clone();
                    context.spawn_background(move |_| {
                        let _ = block_on_panel(async { panel.logout(&auth).await });
                        Message::LogoutDone
                    });
                } else {
                    input.send.call(Message::LogoutDone);
                }
            }
            Message::Refresh => {
                if let Some(auth) = input.auth.clone() {
                    let panel = handles().panel.clone();
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async {
                            let account = panel.account(&auth).await?;
                            let subscription = panel.subscription(&auth).await?;
                            Ok((account, Arc::new(subscription)))
                        });
                        Message::RefreshDone(result)
                    });
                }
            }
            Message::RefreshDone(Ok((account, subscription))) => {
                spawn_sync(
                    context,
                    Arc::new(Secret::new(subscription.subscribe_url.expose().clone())),
                );
                input
                    .send
                    .call(Message::RefreshDone(Ok((account, subscription))));
            }
            Message::PollDone(Ok(3)) => {
                spawn_refresh_now(context, &input);
                input.send.call(Message::PollDone(Ok(3)));
            }
            Message::PlaceDone(Ok((trade_no, checkout, qr))) if checkout.kind == -1 => {
                spawn_refresh_now(context, &input);
                input
                    .send
                    .call(Message::PlaceDone(Ok((trade_no, checkout, qr))));
            }
            Message::ResetSecurity => {
                if input.vm.reset_confirm {
                    if let Some(auth) = input.auth.clone() {
                        let panel = handles().panel.clone();
                        context.spawn_background(move |_| {
                            let result =
                                block_on_panel(async { panel.reset_security(&auth).await });
                            Message::ResetDone(result.map(Arc::new))
                        });
                    }
                } else {
                    input.send.call(Message::ResetSecurity);
                }
            }
            Message::ResetDone(Ok(url)) => {
                spawn_sync(context, Arc::clone(&url));
                input.send.call(Message::ResetDone(Ok(url)));
            }
            Message::CouponCheck => {
                if let (Some(auth), Some(plan)) = (
                    input.auth.clone(),
                    input.vm.shop.plans.get(input.vm.shop.plan_idx),
                ) {
                    let panel = handles().panel.clone();
                    let code = input.vm.shop.coupon_code.clone();
                    let plan_id = plan.id;
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async {
                            panel.coupon_check(&auth, &code, plan_id).await
                        });
                        Message::CouponDone(result)
                    });
                }
            }
            Message::PlaceOrder => {
                let vm = &input.vm;
                if let (Some(auth), Some(plan), Some(period), Some(method)) = (
                    input.auth.clone(),
                    vm.shop.plans.get(vm.shop.plan_idx),
                    vm.shop
                        .plans
                        .get(vm.shop.plan_idx)
                        .and_then(|p| p.prices.get(vm.shop.period_idx)),
                    vm.shop.methods.get(vm.shop.method_idx),
                ) {
                    let panel = handles().panel.clone();
                    let plan_id = plan.id;
                    let period = period.0.clone();
                    let method_id = method.id;
                    let coupon = if vm.shop.coupon_code.is_empty() {
                        None
                    } else {
                        Some(vm.shop.coupon_code.clone())
                    };
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async {
                            let trade_no = panel
                                .order_save(&auth, plan_id, &period, coupon.as_deref())
                                .await?;
                            let checkout =
                                panel.order_checkout(&auth, &trade_no, method_id).await?;
                            let qr = if checkout.kind == 0 {
                                qr_png(&checkout.data).ok()
                            } else {
                                None
                            };
                            Ok((trade_no, checkout, qr))
                        });
                        Message::PlaceDone(result)
                    });
                } else {
                    input.send.call(Message::PlaceDone(Err(PanelError::Business(
                        "请选择套餐 / 周期 / 支付方式".to_owned(),
                    ))));
                }
            }
            Message::CancelOrder(trade_no) => {
                if let Some(auth) = input.auth.clone() {
                    let panel = handles().panel.clone();
                    context.spawn_background(move |_| {
                        let result =
                            block_on_panel(async { panel.order_cancel(&auth, &trade_no).await });
                        Message::CancelDone(result)
                    });
                }
            }
            Message::ChangePassword => {
                if let Some(auth) = input.auth.clone() {
                    if input.vm.pw_old.is_empty() || input.vm.pw_new.len() < 8 {
                        input
                            .send
                            .call(Message::ChangeDone(Err(PanelError::Business(
                                "新密码至少 8 位".to_owned(),
                            ))));
                        return;
                    }
                    let panel = handles().panel.clone();
                    let old = Arc::new(Secret::new(input.vm.pw_old.clone()));
                    let new = Arc::new(Secret::new(input.vm.pw_new.clone()));
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async {
                            panel.change_password(&auth, &old, &new).await
                        });
                        Message::ChangeDone(result)
                    });
                }
            }
            Message::CheckUpdate => {
                let url = input.vm.update_manifest.clone();
                context.spawn_background(move |_| {
                    let result = block_on_panel(async { crate::update::check(&url).await });
                    Message::UpdateChecked(result.map_err(|e| e.to_string()))
                });
            }
            Message::ApplyRules => {
                let text = input.vm.rules_text.clone();
                spawn_apply_rules(context, text);
            }
            Message::ApplyUpdate => {
                if let Some(info) = input.vm.update_info.clone() {
                    tracing::info!(version = %info.version, "update download start");
                    let sync = Arc::clone(&handles().sync);
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async {
                            let path = crate::update::download(&info).await?;
                            // FA 规格二：升级前记录回滚点（当前快照 seq + 版本）。
                            let seq = {
                                let guard = sync.lock().await;
                                guard
                                    .current_seq()
                                    .map_err(|e| PanelError::Network(e.to_string()))?
                                    .unwrap_or(0)
                            };
                            let dir = { sync.lock().await.dir().to_path_buf() };
                            let install_dir = std::env::current_exe()
                                .ok()
                                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                                .unwrap_or_default();
                            let state = crate::rollback::UpdateState {
                                prev_version: env!("CARGO_PKG_VERSION").to_owned(),
                                prev_snapshot_seq: seq,
                                installer_path: path.display().to_string(),
                                started_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                install_dir: install_dir.display().to_string(),
                            };
                            crate::rollback::record_update_state(&dir, &state)
                                .map_err(|e| PanelError::Network(e.to_string()))?;
                            Ok(path)
                        });
                        match &result {
                            Ok(path) => {
                                tracing::info!(path = %path.display(), "update download verified, rollback point recorded")
                            }
                            Err(error) => tracing::warn!(%error, "update download failed"),
                        }
                        Message::UpdateReady(result.map_err(|e| e.to_string()))
                    });
                } else {
                    tracing::warn!("apply update without info");
                }
            }
            Message::ToggleRemindExpire(v) => {
                input.send.call(Message::ToggleRemindExpire(v));
                if let Some(mut account) = input.vm.account.clone() {
                    account.remind_expire = v;
                    spawn_settings(context, &input, account);
                }
            }
            Message::ToggleRemindTraffic(v) => {
                input.send.call(Message::ToggleRemindTraffic(v));
                if let Some(mut account) = input.vm.account.clone() {
                    account.remind_traffic = v;
                    spawn_settings(context, &input, account);
                }
            }
            Message::ToggleAutoRenewal(v) => {
                input.send.call(Message::ToggleAutoRenewal(v));
                if let Some(mut account) = input.vm.account.clone() {
                    account.auto_renewal = v;
                    spawn_settings(context, &input, account);
                }
            }
            Message::SelectZone(tag) => {
                if tag.as_deref() == Some("shop") && !input.vm.shop.loaded {
                    spawn_shop_init(context, &input);
                }
                input.send.call(Message::SelectZone(tag));
            }
            Message::CancelDone(Ok(())) => {
                spawn_shop_init(context, &input);
                input.send.call(Message::CancelDone(Ok(())));
            }
            Message::Tick => {
                context.spawn_background(|_| {
                    std::thread::sleep(TICK);
                    Message::Tick
                });
                let vm = &input.vm;
                if vm.phase == Phase::Authenticated {
                    if let Some(order) = &vm.shop.order
                        && order.status == 0
                        && order.last_poll.elapsed() > POLL_INTERVAL
                        && order.started.elapsed() < POLL_TIMEOUT
                        && let Some(auth) = input.auth.clone()
                    {
                        let panel = handles().panel.clone();
                        let trade_no = order.trade_no.clone();
                        context.spawn_background(move |_| {
                            let result =
                                block_on_panel(async { panel.order_check(&auth, &trade_no).await });
                            Message::PollDone(result)
                        });
                    }
                    if vm.last_refresh.elapsed() > REFRESH_INTERVAL {
                        spawn_refresh_now(context, &input);
                    }
                    if vm.last_check.elapsed() > SESSION_CHECK_INTERVAL
                        && let Some(auth) = input.auth.clone()
                    {
                        let panel = handles().panel.clone();
                        context.spawn_background(move |_| {
                            let result = block_on_panel(async { panel.check_login(&auth).await });
                            Message::SessionCheck(result.map_err(|e| e.to_string()))
                        });
                    }
                }
                input.send.call(Message::Tick);
            }
            other => input.send.call(other),
        }
    }

    fn view(&self, input: &WindowInput, context: &mut ViewContext<Self>) -> View {
        context.window_title("McloudClient");
        let vm = &input.vm;
        match vm.phase {
            Phase::Authenticated => home_view(vm, context),
            _ => login_view(vm, context),
        }
    }
}

fn spawn_apply_rules(context: &ComponentContext<MainWindow>, text: String) {
    let sync = Arc::clone(&handles().sync);
    let commands = handles().commands.clone();
    context.spawn_background(move |_| {
        let result = block_on_panel(async {
            let mut guard = sync.lock().await;
            let dir = guard.dir().to_path_buf();
            let current = guard
                .current_bytes()
                .map_err(|e| PanelError::Network(e.to_string()))?
                .ok_or_else(|| PanelError::Network("尚未同步订阅，无法编辑规则".to_owned()))?;
            // 合并基底必须是「纯订阅」而非已合并快照，否则旧用户规则会累积。
            let subscription = crate::rules_edit::resolve_subscription_base(&dir, &current)
                .map_err(|e| PanelError::Network(e.to_string()))?;
            let merged = crate::rules_edit::prepare_edit(&dir, &subscription, &text)
                .map_err(|e| PanelError::Network(e.to_string()))?;
            let seq = guard
                .write_snapshot(&merged)
                .map_err(|e| PanelError::Network(e.to_string()))?;
            Ok(seq)
        });
        if result.is_ok() {
            let _ = commands.send(Command::Reload);
        }
        Message::RulesDone(result.map_err(|e| e.to_string()))
    });
}

fn spawn_sync(context: &ComponentContext<MainWindow>, url: Arc<Secret<String>>) {
    let sync = Arc::clone(&handles().sync);
    context.spawn_background(move |_| {
        let result = block_on_panel(async {
            let mut guard = sync.lock().await;
            guard
                .sync_once(&url)
                .await
                .map_err(|e| PanelError::Network(e.to_string()))
        });
        Message::SyncDone(result.map_err(|e| e.to_string()))
    });
}

fn spawn_refresh_now(context: &ComponentContext<MainWindow>, input: &WindowInput) {
    let Some(auth) = input.auth.clone() else {
        return;
    };
    let panel = handles().panel.clone();
    context.spawn_background(move |_| {
        let result = block_on_panel(async {
            let account = panel.account(&auth).await?;
            let subscription = panel.subscription(&auth).await?;
            Ok((account, Arc::new(subscription)))
        });
        Message::RefreshDone(result)
    });
}

fn spawn_settings(
    context: &ComponentContext<MainWindow>,
    input: &WindowInput,
    account: AccountView,
) {
    let Some(auth) = input.auth.clone() else {
        return;
    };
    let panel = handles().panel.clone();
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
        Message::SettingsDone(result)
    });
}

fn spawn_shop_init(context: &ComponentContext<MainWindow>, input: &WindowInput) {
    let Some(auth) = input.auth.clone() else {
        return;
    };
    let panel = handles().panel.clone();
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
fn icon_path() -> String {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let installed = dir.join("assets").join("icon.ico");
        if installed.exists() {
            return installed.to_string_lossy().into_owned();
        }
    }
    // 开发运行：build.rs 拷贝到 OUT_DIR 的副本。
    std::path::Path::new(env!("OUT_DIR"))
        .join("icon.ico")
        .to_string_lossy()
        .into_owned()
}

fn login_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
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
            .text(vm.site.clone())
            .placeholder_text("https://panel.example.com")
            .on_text_changed(context.callback(|s: Rc<str>| Message::Site(s.to_string())))
            .into(),
        TextBlock::new().text("邮箱").font_size(12.0).into(),
        TextBox::new()
            .text(vm.email.clone())
            .placeholder_text("you@example.com")
            .on_text_changed(context.callback(|s: Rc<str>| Message::Email(s.to_string())))
            .into(),
        TextBlock::new().text("密码").font_size(12.0).into(),
        PasswordBox::new()
            .password(vm.password.clone())
            .placeholder_text("至少 8 位")
            .on_password_changed(context.callback(|s: Rc<str>| Message::Password(s.to_string())))
            .into(),
        CheckBox::new()
            .is_checked(Some(vm.remember))
            .on_is_checked_changed(
                context.callback(|v: Option<bool>| Message::Remember(v.unwrap_or(false))),
            )
            .content("记住我（默认关闭，仅存系统凭据库）")
            .into(),
        Button::new()
            .is_enabled(vm.phase != Phase::Authenticating)
            .on_click(context.message(Message::Login))
            .content(if vm.phase == Phase::Authenticating {
                "登录中…"
            } else {
                "登录"
            })
            .into(),
    ];
    if let Some(tos) = &vm.guest.tos_url {
        items.push(
            Button::new()
                .on_click(context.message(Message::OpenUrl(tos.clone())))
                .content("服务条款")
                .into(),
        );
    }
    if let Some(app) = &vm.guest.app_url {
        items.push(
            Button::new()
                .on_click(context.message(Message::OpenUrl(app.clone())))
                .content("注册 / 找回密码（浏览器）")
                .into(),
        );
    }
    if vm.guest.is_recaptcha {
        items.push(
            TextBlock::new()
                .text("站点启用了人机验证：请在浏览器完成注册/找回流程")
                .font_size(12.0)
                .into(),
        );
    }
    if vm.guest.is_email_verify {
        items.push(
            TextBlock::new()
                .text("站点注册需邮箱验证码（浏览器流程）")
                .font_size(12.0)
                .into(),
        );
    }
    if let Some(error) = &vm.error {
        items.push(TextBlock::new().text(error.clone()).font_size(12.0).into());
    }
    items.push(
        Button::new()
            .on_click(context.message(Message::CheckUpdate))
            .content("检查更新")
            .into(),
    );
    if let Some(info) = &vm.update_info {
        items.push(
            Button::new()
                .on_click(context.message(Message::ApplyUpdate))
                .content(format!("安装更新 v{}", info.version))
                .into(),
        );
    }
    if !vm.update_status.is_empty() {
        items.push(
            TextBlock::new()
                .text(vm.update_status.clone())
                .font_size(12.0)
                .into(),
        );
    }
    StackPanel::new()
        .spacing(12.0)
        .width(380.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .children(items)
        .into()
}

fn home_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
    let zone_view = match vm.zone {
        Zone::Proxy => proxy_view(vm, context),
        Zone::Subscription => subscription_view(vm, context),
        Zone::Shop => shop_view(vm, context),
        Zone::Account => account_view(vm, context),
    };
    let menu: Vec<View> = vec![
        NavigationViewItem::new()
            .tag("proxy")
            .content("代理")
            .into(),
        NavigationViewItem::new()
            .tag("subscription")
            .content("订阅")
            .into(),
        NavigationViewItem::new().tag("shop").content("商店").into(),
        NavigationViewItem::new()
            .tag("account")
            .content("账户")
            .into(),
    ];
    NavigationView::new()
        .is_pane_open(true)
        .pane_title("McloudClient")
        .menu_items(menu)
        .on_selected_tag_changed(
            context.callback(|t: Option<Rc<str>>| Message::SelectZone(t.map(|s| s.to_string()))),
        )
        .content(zone_view)
        .into()
}

fn proxy_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
    let link = match vm.link {
        LinkStatus::Connected => "内核已连接",
        LinkStatus::Connecting => "内核连接中…",
        LinkStatus::Disconnected => "内核未连接",
    };
    let mut kernel_items: Vec<View> = vec![kv("链路", link)];
    kernel_items.push(kv(
        "系统代理",
        if vm.proxy_taken {
            "已接管（退出时恢复原配置）"
        } else {
            "未接管"
        },
    ));
    kernel_items.push(
        Button::new()
            .on_click(context.message(Message::ToggleProxy))
            .content(if vm.proxy_taken {
                "释放系统代理"
            } else {
                "接管系统代理"
            })
            .into(),
    );
    if let Some(metrics) = &vm.kernel {
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
    if let Some(nodes) = &vm.nodes {
        for group in &nodes.groups {
            node_items.push(kv(
                &format!("策略组 {}", group.name),
                &format!("{} · 选中 {}", group.mode, group.selected),
            ));
            let mut buttons: Vec<View> = vec![
                Button::new()
                    .on_click(context.message(Message::AutoNode(group.name.clone())))
                    .content("自动")
                    .into(),
            ];
            for member in &group.members {
                buttons.push(
                    Button::new()
                        .on_click(
                            context.message(Message::LockNode(group.name.clone(), member.clone())),
                        )
                        .content(format!("锁定 {member}"))
                        .into(),
                );
            }
            node_items.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children(buttons)
                    .into(),
            );
        }
        for node in &nodes.nodes {
            let rtt = node
                .rtt_ms
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.1} ms"));
            node_items.push(kv(&node.name, &rtt));
        }
    }
    if let Some(summary) = &vm.last_probe {
        node_items.push(kv("最近测速", summary));
    }
    let mut probe_items: Vec<View> = vec![
        TextBox::new()
            .text(vm.probe_input.clone())
            .placeholder_text("host:port")
            .on_text_changed(context.callback(|s: Rc<str>| Message::ProbeInput(s.to_string())))
            .into(),
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                View::from(
                    Button::new()
                        .on_click(context.message(Message::Probe))
                        .content("探测"),
                ),
                View::from(
                    Button::new()
                        .on_click(context.message(Message::ProbeNodes))
                        .content("全节点测速"),
                ),
            ))
            .into(),
    ];
    probe_items.push(
        Button::new()
            .on_click(context.message(Message::ExportLogs))
            .content("导出日志")
            .into(),
    );
    if let Some(export) = &vm.log_export {
        probe_items.push(kv("日志导出", export));
    }
    for probe in &vm.probes {
        let status = if probe.reachable {
            probe
                .rtt_ms
                .map_or_else(|| "-".to_owned(), |v| format!("{v:.1} ms"))
        } else {
            probe
                .error_code
                .clone()
                .unwrap_or_else(|| "unreachable".to_owned())
        };
        probe_items.push(kv(&format!("{} · {}", probe.address, probe.age), &status));
    }
    let mut event_items: Vec<View> = Vec::new();
    for event in &vm.events {
        event_items.push(kv(
            &format!("{} · {}", event.kind, event.age),
            &event.summary,
        ));
    }
    if event_items.is_empty() {
        event_items.push(kv("暂无事件", ""));
    }
    let mut rules_items: Vec<View> = vec![
        TextBox::new()
            .text(vm.rules_text.clone())
            .placeholder_text(
                "每行一条规则，如 DOMAIN-SUFFIX,example.com,DIRECT（用户规则优先于订阅规则）",
            )
            .accepts_return(true)
            .on_text_changed(context.callback(|s: Rc<str>| Message::RulesInput(s.to_string())))
            .into(),
        Button::new()
            .on_click(context.message(Message::ApplyRules))
            .content("应用规则")
            .into(),
    ];
    if !vm.rules_status.is_empty() {
        rules_items.push(kv("规则状态", &vm.rules_status));
    }
    StackPanel::new()
        .spacing(16.0)
        .margin(24.0)
        .children((
            panel("连接与内核", keyed(kernel_items)),
            panel("节点与负载", keyed(node_items)),
            panel("探测", keyed(probe_items)),
            panel("分流规则", keyed(rules_items)),
            panel("事件", keyed(event_items)),
        ))
        .into()
}

fn subscription_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
    let mut items: Vec<View> = vec![kv("订阅同步", &vm.sync_label)];
    if !vm.update_label.is_empty() {
        items.push(kv("上次更新", &vm.update_label));
    }
    if let Some(sub) = &vm.sub {
        let percent = if sub.total_bytes > 0 {
            100.0 * sub.used_bytes as f64 / sub.total_bytes as f64
        } else {
            0.0
        };
        items.push(kv(
            "已用 / 总量",
            &format!(
                "{} / {}",
                fmt_bytes(sub.used_bytes),
                fmt_bytes(sub.total_bytes)
            ),
        ));
        items.push(ProgressBar::new().value(percent).maximum(100.0).into());
        items.push(kv("重置日", &format!("每月 {} 日", sub.reset_day)));
        let days = (sub.expired_at - unix_now()) / 86_400;
        items.push(kv("到期剩余", &format!("{days} 天")));
    } else {
        items.push(kv("订阅", "尚未同步"));
    }
    items.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                View::from(
                    Button::new()
                        .on_click(context.message(Message::Refresh))
                        .content("手动刷新"),
                ),
                View::from(
                    Button::new()
                        .on_click(context.message(Message::ResetSecurity))
                        .content(if vm.reset_confirm {
                            "确认重置？旧链接立即失效"
                        } else {
                            "重置订阅链接"
                        }),
                ),
            ))
            .into(),
    );
    if let Some(error) = &vm.error {
        items.push(TextBlock::new().text(error.clone()).font_size(12.0).into());
    }
    StackPanel::new()
        .spacing(12.0)
        .margin(24.0)
        .children((panel("订阅", keyed(items)),))
        .into()
}

fn shop_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
    let shop = &vm.shop;
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
        if let Some(plan) = shop.plans.get(shop.plan_idx) {
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
            if let Some((_, label, cents)) = plan.prices.get(shop.period_idx) {
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
                .on_text_changed(context.callback(|s: Rc<str>| Message::CouponInput(s.to_string())))
                .into(),
        );
        items.push(
            Button::new()
                .on_click(context.message(Message::CouponCheck))
                .content("校验优惠券")
                .into(),
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
                .content("下单")
                .into(),
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
                    .source_data(EncodedImage::new(qr.clone()))
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
        if order.kind == 1 {
            order_items.push(
                Button::new()
                    .on_click(context.message(Message::OpenUrl(format!(
                        "https://pay.example.invalid/qr/{}",
                        order.trade_no
                    ))))
                    .content("打开支付页面")
                    .into(),
            );
        }
        if order.status == 0 {
            order_items.push(
                Button::new()
                    .on_click(context.message(Message::CancelOrder(order.trade_no.clone())))
                    .content(format!("取消 {}", order.trade_no))
                    .into(),
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
                        .content(format!("取消 {}", order.trade_no))
                        .into(),
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
        .into()
}

fn account_view(vm: &ViewModel, context: &mut ViewContext<MainWindow>) -> View {
    let mut items: Vec<View> = Vec::new();
    if let Some(account) = &vm.account {
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
                .on_is_checked_changed(
                    context.callback(|v: Option<bool>| {
                        Message::ToggleRemindExpire(v.unwrap_or(false))
                    }),
                )
                .content("到期提醒")
                .into(),
        );
        items.push(
            CheckBox::new()
                .is_checked(Some(account.remind_traffic))
                .on_is_checked_changed(
                    context.callback(|v: Option<bool>| {
                        Message::ToggleRemindTraffic(v.unwrap_or(false))
                    }),
                )
                .content("流量提醒")
                .into(),
        );
        items.push(
            CheckBox::new()
                .is_checked(Some(account.auto_renewal))
                .on_is_checked_changed(
                    context
                        .callback(|v: Option<bool>| Message::ToggleAutoRenewal(v.unwrap_or(false))),
                )
                .content("自动续费")
                .into(),
        );
    }
    if !vm.update_label.is_empty() {
        items.push(kv("上次更新", &vm.update_label));
    }
    items.push(
        TextBlock::new()
            .text("修改密码（改密后全部会话失效）")
            .font_size(13.0)
            .into(),
    );
    items.push(
        PasswordBox::new()
            .password(vm.pw_old.clone())
            .placeholder_text("旧密码")
            .on_password_changed(context.callback(|s: Rc<str>| Message::PwOld(s.to_string())))
            .into(),
    );
    items.push(
        PasswordBox::new()
            .password(vm.pw_new.clone())
            .placeholder_text("新密码（至少 8 位）")
            .on_password_changed(context.callback(|s: Rc<str>| Message::PwNew(s.to_string())))
            .into(),
    );
    items.push(
        Button::new()
            .on_click(context.message(Message::ChangePassword))
            .content("修改密码")
            .into(),
    );
    items.push(
        Button::new()
            .on_click(context.message(Message::Logout))
            .content("登出并清除已保存凭据")
            .into(),
    );
    items.push(
        Button::new()
            .on_click(context.message(Message::CheckUpdate))
            .content("检查更新")
            .into(),
    );
    if let Some(info) = &vm.update_info {
        items.push(
            Button::new()
                .on_click(context.message(Message::ApplyUpdate))
                .content(format!("安装更新 v{}", info.version))
                .into(),
        );
    }
    if !vm.update_status.is_empty() {
        items.push(
            TextBlock::new()
                .text(vm.update_status.clone())
                .font_size(12.0)
                .into(),
        );
    }
    StackPanel::new()
        .spacing(12.0)
        .margin(24.0)
        .children((panel("账户", keyed(items)),))
        .into()
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
        .into()
}

fn panel(title: &str, body: View) -> View {
    StackPanel::new()
        .spacing(8.0)
        .children((TextBlock::new().text(title).font_size(14.0), body))
        .into()
}

fn keyed(items: Vec<View>) -> View {
    StackPanel::new().spacing(6.0).children(items).into()
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

/// 在后台线程里跑面板异步调用（spawn_background 非 async）。
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
    App::run_application::<TrayApp>(config)
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
}
