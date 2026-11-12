//! reactor 渲染层（ADR 0005）：登录页 + 四区主界面 + 内核观测。
//!
//! 凭据边界：`Secret` 只在本模块与 `panel` 内持有；UI 控件只绑定脱敏 DTO。
//! 后台 IPC 任务经 `SharedState` 供数；reactor 侧以 250 ms revision 轮询桥接
//! （纯内存读，无 IPC；`LocalSender` 非 Send，不能跨线程直推消息）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use client_sidecar::credential::CredentialStore;
use client_sidecar::secret::Secret;
use tokio::sync::mpsc;
use windows_reactor::*;

use crate::panel::{AccountView, PanelClient, SubscriptionView};
use crate::state::{Command, LinkStatus, SharedState, fmt_age, fmt_bytes, fmt_uptime};
use crate::task::TaskHandle;

const CREDENTIAL_SERVICE: &str = "mcloud-client";
const TICK: Duration = Duration::from_millis(250);

/// 重句柄：经 OnceLock 传入（reactor 的 Input 要求 Clone+PartialEq，
/// 不能携带 IPC 通道 / 凭据库 / 共享状态）。
pub struct GuiHandles {
    pub shared: Arc<SharedState>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub task: TaskHandle,
    pub panel: PanelClient,
    pub store: Arc<dyn CredentialStore>,
}

static HANDLES: std::sync::OnceLock<GuiHandles> = std::sync::OnceLock::new();

/// 轻配置：作为 reactor Input。
#[derive(Clone, PartialEq)]
pub struct GuiConfig {
    pub site: String,
    pub probe_default: String,
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
    RefreshSub,
    SubDone(Result<Arc<SubscriptionView>, String>),
    SelectZone(Option<String>),
    ProbeInput(String),
    Probe,
    ProbeNodes,
    LockNode(String, String),
    AutoNode(String),
    SessionCheck(Result<bool, String>),
    Tick,
}

pub struct Shell {
    shared: Arc<SharedState>,
    commands: mpsc::UnboundedSender<Command>,
    _task: TaskHandle,
    panel: PanelClient,
    store: Arc<dyn CredentialStore>,
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
        };
        context.spawn_background(|_| {
            std::thread::sleep(TICK);
            Message::Tick
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
                Message::LoginDone(result)
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
                    Message::LoginDone(result)
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
                self.subscription = Some(payload.subscription);
                self.phase = Phase::Authenticated;
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
            Message::RefreshSub => {
                if let Some(auth) = self.auth.clone() {
                    let panel = self.panel.clone();
                    context.spawn_background(move |_| {
                        let result = block_on_panel(async { panel.subscription(&auth).await });
                        Message::SubDone(result.map(Arc::new).map_err(|e| e.to_string()))
                    });
                }
            }
            Message::SubDone(Ok(subscription)) => {
                self.subscription = Some(subscription);
                self.error = None;
            }
            Message::SubDone(Err(error)) => self.error = Some(error),
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
            Message::Tick => {
                self.last_revision = self.shared.revision();
                // 会话撤销检测：面板侧登出/改密后最长 60 s 内本地感知。
                if self.phase == Phase::Authenticated
                    && self.last_check.elapsed() > Duration::from_secs(60)
                {
                    self.last_check = Instant::now();
                    if let Some(auth) = self.auth.clone() {
                        let panel = self.panel.clone();
                        context.spawn_background(move |_| {
                            let result = block_on_panel(async { panel.check_login(&auth).await });
                            Message::SessionCheck(result.map_err(|e| e.to_string()))
                        });
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
        let _ = self.store.delete(CREDENTIAL_SERVICE, "email");
        let _ = self.store.delete(CREDENTIAL_SERVICE, "password");
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
            Zone::Shop => shop_view(),
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
            Button::new()
                .on_click(context.message(Message::RefreshSub))
                .content("手动刷新"),
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
}

fn shop_view() -> View {
    StackPanel::new()
        .spacing(12.0)
        .margin(24.0)
        .children((panel(
            "商店",
            StackPanel::new().spacing(8.0).children((
                TextBlock::new()
                    .text("套餐 / 优惠券 / 订单 / 支付将在 A3 接入。")
                    .font_size(13.0),
                TextBlock::new()
                    .text("mock 面板已就绪（client-sidecar::mock_panel）。")
                    .font_size(12.0),
            )),
        ),))
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

/// 在后台线程里跑面板异步调用（reactor 的 spawn_background 非 async）。
fn block_on_panel<F, T>(future: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, crate::panel::PanelError>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(future).map_err(|error| error.to_string())
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
