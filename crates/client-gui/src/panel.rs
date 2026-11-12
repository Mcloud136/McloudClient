//! v2board 用户端 API 最小客户端（A1 骨架，路由按生产盘点 §2）。
//!
//! 凭据边界（ADR 0004 L1）：密码 / `auth_data` / 订阅 URL 只以 `Secret<T>`
//! 在本模块与控制面内持有；返回给 UI 的 DTO 全部脱敏，不含任何凭据字段。
//! 错误口径：403 → 会话失效；429 → 限流；500 → 业务错误（取 `message`）。

use client_sidecar::fetch::request;
use client_sidecar::secret::Secret;
use serde_json::{Value, json};

/// 面板错误分类（UI 据此决定回登录页 / 提示锁定 / 降级）。
#[derive(Debug)]
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
#[derive(Debug, Clone)]
pub struct AccountView {
    pub email_masked: String,
    pub balance_cents: i64,
    pub expired_at: i64,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub plan_id: i64,
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
