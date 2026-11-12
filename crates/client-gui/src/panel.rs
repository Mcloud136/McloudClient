//! v2board 用户端 API 最小客户端（A1 骨架，路由按生产盘点 §2）。
//!
//! 凭据边界（ADR 0004 L1）：密码 / `auth_data` / 订阅 URL 只以 `Secret<T>`
//! 在本模块与控制面内持有；返回给 UI 的 DTO 全部脱敏，不含任何凭据字段。
//! 错误口径：403 → 会话失效；429 → 限流；500 → 业务错误（取 `message`）。

use client_sidecar::fetch::request;
use client_sidecar::secret::Secret;
use serde_json::{Value, json};

/// 面板错误分类（UI 据此决定回登录页 / 提示锁定 / 降级）。
#[derive(Debug, Clone)]
pub enum PanelError {
    /// 403：未登录 / JWT 过期 / 会话被撤销。
    Expired,
    /// 429：登录限流等，不自动重试。
    RateLimited,
    /// 500 等业务错误，携带面板 `message`。
    Business(String),
    /// 网络 / TLS / 解析失败。
    Network(String),
}

impl std::fmt::Display for PanelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => write!(f, "登录已失效，请重新登录"),
            Self::RateLimited => write!(f, "请求过于频繁，请稍后再试"),
            Self::Business(message) => write!(f, "{message}"),
            Self::Network(message) => write!(f, "网络错误：{message}"),
        }
    }
}

impl std::error::Error for PanelError {}

/// 脱敏账户视图（UI 可直接渲染）。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    pub email_masked: String,
    pub balance_cents: i64,
    pub expired_at: i64,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub plan_id: i64,
    pub remind_expire: bool,
    pub remind_traffic: bool,
    pub auto_renewal: bool,
}

/// 脱敏订阅视图；订阅 URL 仍为凭据，只供快照注入，不进 UI。
pub struct SubscriptionView {
    /// A2 快照注入时读取；GUI 渲染层禁止访问（L1）。
    #[allow(dead_code)]
    pub subscribe_url: Secret<String>,
    pub expired_at: i64,
    pub reset_day: i64,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// 邮箱掩码：`a***@domain`。
#[must_use]
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let head: String = local.chars().take(1).collect();
            format!("{head}***@{domain}")
        }
        None => "***".to_owned(),
    }
}

/// 单站点面板客户端。
#[derive(Clone)]
pub struct PanelClient {
    base: String,
}

impl PanelClient {
    /// `base` 形如 `https://panel.example.com`（不含尾斜杠）。
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        auth: Option<&Secret<String>>,
        body: Option<Value>,
    ) -> Result<Value, PanelError> {
        let mut headers = Vec::new();
        if let Some(auth) = auth {
            headers.push(("Authorization".to_owned(), auth.expose().clone()));
        }
        let body_bytes = match body {
            Some(value) => Some(
                serde_json::to_vec(&value)
                    .map_err(|error| PanelError::Network(error.to_string()))?,
            ),
            None => None,
        };
        if body_bytes.is_some() {
            headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        }
        let response = request(
            method,
            &format!("{}{path}", self.base),
            &headers,
            body_bytes.as_deref(),
        )
        .await
        .map_err(|error| PanelError::Network(error.to_string()))?;
        match response.status {
            200 => serde_json::from_slice(&response.body)
                .map_err(|error| PanelError::Network(format!("invalid json: {error}"))),
            403 => Err(PanelError::Expired),
            429 => Err(PanelError::RateLimited),
            _ => {
                let message = serde_json::from_slice::<Value>(&response.body)
                    .ok()
                    .and_then(|value| value["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("HTTP {}", response.status));
                Err(PanelError::Business(message))
            }
        }
    }

    /// 登录；返回 `auth_data`（24 h 可撤销 JWT）。密码仅在此短暂使用。
    ///
    /// # Errors
    ///
    /// 凭据错误 / 限流 / 网络失败时返回对应 [`PanelError`]。
    pub async fn login(
        &self,
        email: &str,
        password: &Secret<String>,
    ) -> Result<Secret<String>, PanelError> {
        let data = self
            .call(
                "POST",
                "/api/v1/passport/auth/login",
                None,
                Some(json!({ "email": email, "password": password.expose() })),
            )
            .await?;
        data["data"]["auth_data"]
            .as_str()
            .map(|token| Secret::new(token.to_owned()))
            .ok_or_else(|| PanelError::Network("missing auth_data".to_owned()))
    }

    /// 校验会话是否有效。
    ///
    /// # Errors
    ///
    /// 网络失败时返回错误；403 视为无效会话返回 `Ok(false)`。
    pub async fn check_login(&self, auth: &Secret<String>) -> Result<bool, PanelError> {
        match self
            .call("GET", "/api/v1/user/checkLogin", Some(auth), None)
            .await
        {
            Ok(data) => Ok(data["data"]["is_login"].as_bool().unwrap_or(false)),
            Err(PanelError::Expired) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// 账户信息（脱敏 DTO）。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn account(&self, auth: &Secret<String>) -> Result<AccountView, PanelError> {
        let data = self
            .call("GET", "/api/v1/user/info", Some(auth), None)
            .await?;
        let data = &data["data"];
        Ok(AccountView {
            email_masked: mask_email(data["email"].as_str().unwrap_or_default()),
            balance_cents: data["balance"].as_i64().unwrap_or_default(),
            expired_at: data["expired_at"].as_i64().unwrap_or_default(),
            used_bytes: data["u"].as_u64().unwrap_or_default()
                + data["d"].as_u64().unwrap_or_default(),
            total_bytes: data["transfer_enable"].as_u64().unwrap_or_default(),
            plan_id: data["plan_id"].as_i64().unwrap_or_default(),
            remind_expire: data["remind_expire"].as_i64().unwrap_or(1) != 0,
            remind_traffic: data["remind_traffic"].as_i64().unwrap_or(1) != 0,
            auto_renewal: data["auto_renewal"].as_i64().unwrap_or(0) != 0,
        })
    }

    /// 订阅信息；`subscribe_url` 保持 `Secret`，仅用于注入快照源。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn subscription(
        &self,
        auth: &Secret<String>,
    ) -> Result<SubscriptionView, PanelError> {
        let data = self
            .call("GET", "/api/v1/user/getSubscribe", Some(auth), None)
            .await?;
        let data = &data["data"];
        let url = data["subscribe_url"]
            .as_str()
            .ok_or_else(|| PanelError::Network("missing subscribe_url".to_owned()))?;
        Ok(SubscriptionView {
            subscribe_url: Secret::new(url.to_owned()),
            expired_at: data["expired_at"].as_i64().unwrap_or_default(),
            reset_day: data["reset_day"].as_i64().unwrap_or_default(),
            used_bytes: data["u"].as_u64().unwrap_or_default()
                + data["d"].as_u64().unwrap_or_default(),
            total_bytes: data["transfer_enable"].as_u64().unwrap_or_default(),
        })
    }

    /// 登出（面板侧清除全部会话）。
    ///
    /// # Errors
    ///
    /// 网络失败时返回错误；会话已失效视为成功。
    pub async fn logout(&self, auth: &Secret<String>) -> Result<(), PanelError> {
        match self
            .call("POST", "/api/v1/user/logout", Some(auth), None)
            .await
        {
            Ok(_) | Err(PanelError::Expired) => Ok(()),
            Err(other) => Err(other),
        }
    }

    /// 修改密码；面板侧清除全部会话（调用方应强制回登录页）。
    ///
    /// # Errors
    ///
    /// 旧密码错误 / 会话失效 / 网络失败时返回错误。
    pub async fn change_password(
        &self,
        auth: &Secret<String>,
        old: &Secret<String>,
        new: &Secret<String>,
    ) -> Result<(), PanelError> {
        self.call(
            "POST",
            "/api/v1/user/changePassword",
            Some(auth),
            Some(json!({ "old_password": old.expose(), "new_password": new.expose() })),
        )
        .await?;
        Ok(())
    }

    /// 通知/续费设置。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn update_settings(
        &self,
        auth: &Secret<String>,
        auto_renewal: bool,
        remind_expire: bool,
        remind_traffic: bool,
    ) -> Result<(), PanelError> {
        self.call(
            "POST",
            "/api/v1/user/update",
            Some(auth),
            Some(json!({
                "auto_renewal": i64::from(auto_renewal),
                "remind_expire": i64::from(remind_expire),
                "remind_traffic": i64::from(remind_traffic),
            })),
        )
        .await?;
        Ok(())
    }

    /// 重置订阅链接；返回新 URL（旧链接立即失效）。响应 `data` 为裸字符串。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn reset_security(
        &self,
        auth: &Secret<String>,
    ) -> Result<Secret<String>, PanelError> {
        let data = self
            .call("GET", "/api/v1/user/resetSecurity", Some(auth), None)
            .await?;
        data["data"]
            .as_str()
            .map(|url| Secret::new(url.to_owned()))
            .ok_or_else(|| PanelError::Network("missing subscribe_url".to_owned()))
    }

    /// 未登录站点配置（登录页扩展位：服务条款 / 邮箱验证 / 人机验证开关）。
    ///
    /// # Errors
    ///
    /// 网络失败时返回错误；调用方应降级为不显示扩展位。
    pub async fn guest_config(&self) -> Result<GuestConfig, PanelError> {
        let data = self
            .call("GET", "/api/v1/guest/comm/config", None, None)
            .await?;
        let data = &data["data"];
        Ok(GuestConfig {
            tos_url: data["tos_url"].as_str().map(str::to_owned),
            app_url: data["app_url"].as_str().map(str::to_owned),
            is_email_verify: data["is_email_verify"].as_i64().unwrap_or(0) != 0,
            is_recaptcha: data["is_recaptcha"].as_i64().unwrap_or(0) != 0,
        })
    }
}

/// 登录页扩展位（脱敏、无凭据）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuestConfig {
    pub tos_url: Option<String>,
    pub app_url: Option<String>,
    pub is_email_verify: bool,
    pub is_recaptcha: bool,
}

/// 套餐视图（金额整数分；周期价格表）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    pub id: i64,
    pub name: String,
    pub content: String,
    pub transfer_enable: u64,
    pub renew: bool,
    pub capacity_limit: Option<i64>,
    /// (period key, 中文标签, 整数分)
    pub prices: Vec<(String, String, i64)>,
}

/// 支付方式视图。
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentMethodView {
    pub id: i64,
    pub name: String,
    pub payment: String,
}

/// 结算结果：0 二维码 / 1 URL / -1 余额已全额支付。
#[derive(Debug, Clone, PartialEq)]
pub struct CheckoutView {
    pub kind: i64,
    pub data: String,
}

/// 订单视图（脱敏：不含凭据）。
#[derive(Debug, Clone, PartialEq)]
pub struct OrderView {
    pub trade_no: String,
    pub status: i64,
    pub total_amount: i64,
    pub period: String,
}

/// 周期 key → 中文标签（与生产 OrderSave 校验枚举一致）。
#[must_use]
pub fn period_label(key: &str) -> &'static str {
    match key {
        "month_price" => "月付",
        "quarter_price" => "季付",
        "half_year_price" => "半年付",
        "year_price" => "年付",
        "two_year_price" => "两年付",
        "three_year_price" => "三年付",
        "onetime_price" => "一次性",
        "reset_price" => "流量重置包",
        _ => "未知周期",
    }
}

impl PanelClient {
    /// 在售套餐列表（show=1，含剩余名额）。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn plans(&self, auth: &Secret<String>) -> Result<Vec<PlanView>, PanelError> {
        let data = self
            .call("GET", "/api/v1/user/plan/fetch", Some(auth), None)
            .await?;
        let list = data["data"].as_array().cloned().unwrap_or_default();
        Ok(list
            .iter()
            .map(|p| PlanView {
                id: p["id"].as_i64().unwrap_or_default(),
                name: p["name"].as_str().unwrap_or_default().to_owned(),
                content: p["content"].as_str().unwrap_or_default().to_owned(),
                transfer_enable: p["transfer_enable"].as_u64().unwrap_or_default(),
                renew: p["renew"].as_i64().unwrap_or(0) != 0,
                capacity_limit: p["capacity_limit"].as_i64(),
                prices: [
                    "month_price",
                    "quarter_price",
                    "half_year_price",
                    "year_price",
                    "two_year_price",
                    "three_year_price",
                    "onetime_price",
                    "reset_price",
                ]
                .iter()
                .filter_map(|key| {
                    p[*key]
                        .as_i64()
                        .map(|cents| ((*key).to_owned(), period_label(key).to_owned(), cents))
                })
                .collect(),
            })
            .collect())
    }

    /// 已启用支付方式。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn payment_methods(
        &self,
        auth: &Secret<String>,
    ) -> Result<Vec<PaymentMethodView>, PanelError> {
        let data = self
            .call(
                "GET",
                "/api/v1/user/order/getPaymentMethod",
                Some(auth),
                None,
            )
            .await?;
        let list = data["data"].as_array().cloned().unwrap_or_default();
        Ok(list
            .iter()
            .map(|m| PaymentMethodView {
                id: m["id"].as_i64().unwrap_or_default(),
                name: m["name"].as_str().unwrap_or_default().to_owned(),
                payment: m["payment"].as_str().unwrap_or_default().to_owned(),
            })
            .collect())
    }

    /// 优惠券校验；返回展示用折扣描述。
    ///
    /// # Errors
    ///
    /// 券无效 / 会话失效 / 网络失败时返回错误。
    pub async fn coupon_check(
        &self,
        auth: &Secret<String>,
        code: &str,
        plan_id: i64,
    ) -> Result<String, PanelError> {
        let data = self
            .call(
                "POST",
                "/api/v1/user/coupon/check",
                Some(auth),
                Some(json!({ "code": code, "plan_id": plan_id })),
            )
            .await?;
        let coupon = &data["data"];
        let name = coupon["name"].as_str().unwrap_or("优惠券");
        let kind = coupon["type"].as_i64().unwrap_or(0);
        let value = coupon["value"].as_i64().unwrap_or(0);
        let label = if kind == 2 {
            format!("{name}：抵扣 ¥{:.2}", value as f64 / 100.0)
        } else {
            format!("{name}：{value}% 折扣")
        };
        Ok(label)
    }

    /// 创建订单；返回 trade_no。
    ///
    /// # Errors
    ///
    /// 存在未完成订单 / 售罄 / 会话失效 / 网络失败时返回错误。
    pub async fn order_save(
        &self,
        auth: &Secret<String>,
        plan_id: i64,
        period: &str,
        coupon_code: Option<&str>,
    ) -> Result<String, PanelError> {
        let mut body = json!({ "plan_id": plan_id, "period": period });
        if let Some(code) = coupon_code {
            body["coupon_code"] = json!(code);
        }
        let data = self
            .call("POST", "/api/v1/user/order/save", Some(auth), Some(body))
            .await?;
        data["data"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| PanelError::Network("missing trade_no".to_owned()))
    }

    /// 结算订单；type 0 二维码 / 1 URL / -1 余额全额。
    ///
    /// # Errors
    ///
    /// 订单不存在 / 支付方式不可用 / 会话失效 / 网络失败时返回错误。
    pub async fn order_checkout(
        &self,
        auth: &Secret<String>,
        trade_no: &str,
        method: i64,
    ) -> Result<CheckoutView, PanelError> {
        let data = self
            .call(
                "POST",
                "/api/v1/user/order/checkout",
                Some(auth),
                Some(json!({ "trade_no": trade_no, "method": method })),
            )
            .await?;
        Ok(CheckoutView {
            kind: data["type"].as_i64().unwrap_or(1),
            data: data["data"].as_str().unwrap_or_default().to_owned(),
        })
    }

    /// 订单状态裸整数：0 待支付 / 1 已支付 / 2 已取消 / 3 已完成 / 4 处理中。
    ///
    /// # Errors
    ///
    /// 订单不存在 / 会话失效 / 网络失败时返回错误。
    pub async fn order_check(
        &self,
        auth: &Secret<String>,
        trade_no: &str,
    ) -> Result<i64, PanelError> {
        let data = self
            .call(
                "GET",
                &format!("/api/v1/user/order/check?trade_no={trade_no}"),
                Some(auth),
                None,
            )
            .await?;
        Ok(data["data"].as_i64().unwrap_or(0))
    }

    /// 订单列表。
    ///
    /// # Errors
    ///
    /// 会话失效 / 网络失败时返回错误。
    pub async fn orders(&self, auth: &Secret<String>) -> Result<Vec<OrderView>, PanelError> {
        let data = self
            .call("GET", "/api/v1/user/order/fetch", Some(auth), None)
            .await?;
        let list = data["data"].as_array().cloned().unwrap_or_default();
        Ok(list
            .iter()
            .map(|o| OrderView {
                trade_no: o["trade_no"].as_str().unwrap_or_default().to_owned(),
                status: o["status"].as_i64().unwrap_or_default(),
                total_amount: o["total_amount"].as_i64().unwrap_or_default(),
                period: o["period"].as_str().unwrap_or_default().to_owned(),
            })
            .collect())
    }

    /// 取消待支付订单。
    ///
    /// # Errors
    ///
    /// 非待支付 / 会话失效 / 网络失败时返回错误。
    pub async fn order_cancel(
        &self,
        auth: &Secret<String>,
        trade_no: &str,
    ) -> Result<(), PanelError> {
        self.call(
            "POST",
            "/api/v1/user/order/cancel",
            Some(auth),
            Some(json!({ "trade_no": trade_no })),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PanelClient, mask_email};
    use client_sidecar::mock_panel::MockPanel;
    use client_sidecar::secret::Secret;

    #[test]
    fn masks_email() {
        assert_eq!(mask_email("alice@example.com"), "a***@example.com");
        assert_eq!(mask_email("invalid"), "***");
    }

    #[tokio::test]
    async fn login_account_subscription_logout_against_mock() {
        let panel = MockPanel::start().await.expect("mock");
        let client = PanelClient::new(panel.base_url());
        let password = Secret::new(String::from("12345678"));
        let auth = client
            .login("alice@example.com", &password)
            .await
            .expect("login");
        assert!(client.check_login(&auth).await.expect("check"));
        let account = client.account(&auth).await.expect("account");
        assert_eq!(account.email_masked, "a***@example.com");
        assert_eq!(account.balance_cents, 1250);
        let subscription = client.subscription(&auth).await.expect("subscription");
        assert!(subscription.subscribe_url.expose().contains("/sub?token="));
        client.logout(&auth).await.expect("logout");
        assert!(!client.check_login(&auth).await.expect("check after logout"));
    }

    #[tokio::test]
    async fn wrong_password_is_business_error() {
        let panel = MockPanel::start().await.expect("mock");
        let client = PanelClient::new(panel.base_url());
        let error = client
            .login("a@b.c", &Secret::new(String::from("short")))
            .await
            .err()
            .expect("must fail");
        assert!(matches!(error, super::PanelError::Business(_)));
    }
}
