//! v2board 用户端 API mock 面板（A0 验收项，A1/A2/A5 回归共用）。
//!
//! 路由与响应形状按 2026-11-12 生产源码一手盘点（`docs/c2-account-integration-design.md` §2）：
//! 业务错误 500 + `{message}`、鉴权失败 403、`order/check` 裸整数状态、
//! `checkout` `{type,data}`（0 二维码 / 1 URL / -1 余额全额）、`resetSecurity` 裸字符串。
//! 仅监听回环，供测试与开发联调；不含任何真实凭据。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// mock 面板句柄：地址 + 测试控制面。
pub struct MockPanel {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    port: u16,
    sessions: HashMap<String, String>,
    orders: HashMap<String, u8>,
    sub_version: u32,
    sub_token: String,
    next_trade: u32,
    installer: Option<Vec<u8>>,
    installer_sha: String,
    password: String,
    remind_expire: bool,
    remind_traffic: bool,
    auto_renewal: bool,
}

impl MockPanel {
    /// 在回环随机端口启动 mock 面板。
    ///
    /// # Errors
    ///
    /// 绑定失败时返回错误。
    pub async fn start() -> Result<Self> {
        Self::start_on(0).await
    }

    /// 在回环指定端口启动（0 = 随机）；实机回归需同端口重启以复现会话失效。
    ///
    /// # Errors
    ///
    /// 绑定失败时返回错误。
    pub async fn start_on(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .context("bind mock panel")?;
        let addr = listener.local_addr().context("mock panel addr")?;
        let state = Arc::new(Mutex::new(State {
            port: addr.port(),
            sub_token: "sub-token".to_owned(),
            password: "12345678".to_owned(),
            remind_expire: true,
            remind_traffic: true,
            ..State::default()
        }));
        let server_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let state = Arc::clone(&server_state);
                tokio::spawn(async move {
                    let _ = handle(socket, state).await;
                });
            }
        });
        Ok(Self { addr, state })
    }

    /// 面板基址（`http://127.0.0.1:port`）。
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 测试控制：把订单置为已完成（模拟支付回调）。
    pub fn pay_order(&self, trade_no: &str) {
        let mut state = self.state.lock().expect("mock state");
        state.orders.insert(trade_no.to_owned(), 3);
    }

    /// 测试控制：设置更新安装器字节（manifest 的 sha256 随之计算）。
    ///
    /// # Errors
    ///
    /// 读取安装器文件失败时返回错误。
    pub fn set_installer(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(path)?;
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let mut state = self.state.lock().expect("mock state");
        state.installer = Some(bytes);
        state.installer_sha = sha;
        Ok(())
    }

    /// 测试控制：订阅内容版本 +1（模拟面板侧订阅变更）。
    pub fn bump_subscription(&self) {
        let mut state = self.state.lock().expect("mock state");
        state.sub_version += 1;
    }
}

async fn handle(mut socket: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let length: usize = lines
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + 4 + length {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = buf[head_end + 4..head_end + 4 + length.min(buf.len() - head_end - 4)].to_vec();
    let auth = head
        .lines()
        .find_map(|l| l.strip_prefix("Authorization: "))
        .map(str::to_owned);

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), q.to_owned()),
        None => (target.clone(), String::new()),
    };
    let params: HashMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let json_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    if method == "GET" && path == "/update/installer" {
        let bytes = state.lock().expect("mock state").installer.clone();
        eprintln!("MOCK REQ {method} {target} -> installer");
        match bytes {
            Some(bytes) => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                socket.write_all(head.as_bytes()).await?;
                socket.write_all(&bytes).await?;
            }
            None => {
                let head =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                socket.write_all(head.as_bytes()).await?;
            }
        }
        return Ok(());
    }
    let (status, response) = route(&method, &path, &params, &json_body, auth.as_deref(), &state);
    eprintln!("MOCK REQ {method} {target} -> {status}");
    let payload = serde_json::to_vec(&response)?;
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        payload.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&payload).await?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        403 => "Forbidden",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    }
}

fn route(
    method: &str,
    path: &str,
    params: &HashMap<String, String>,
    body: &Value,
    auth: Option<&str>,
    state: &Arc<Mutex<State>>,
) -> (u16, Value) {
    let mut state = state.lock().expect("mock state");
    match (method, path) {
        ("POST", "/api/v1/passport/auth/login") => {
            let email = body["email"].as_str().unwrap_or_default();
            let password = body["password"].as_str().unwrap_or_default();
            // 固定演示密码：使「错密码」态可在实机回归中复现。
            if !email.contains('@') || password != state.password {
                return (500, json!({ "message": "Incorrect email or password" }));
            }
            let token = format!("mock-jwt-{}", state.sessions.len() + 1);
            state.sessions.insert(token.clone(), email.to_owned());
            (
                200,
                json!({ "data": { "token": "sub-token", "is_admin": false, "auth_data": token } }),
            )
        }
        ("GET", "/api/v1/user/checkLogin") => match auth.and_then(|a| state.sessions.get(a)) {
            Some(_) => (200, json!({ "data": { "is_login": true } })),
            None => (403, json!({ "message": "未登录或登陆已过期" })),
        },
        ("GET", "/api/v1/user/info") => match auth.and_then(|a| state.sessions.get(a)) {
            Some(email) => (
                200,
                json!({ "data": {
                    "email": email, "balance": 1250, "commission_balance": 0,
                    "transfer_enable": 107_374_182_400u64, "u": 26_843_545_600u64,
                    "d": 26_843_545_600u64, "expired_at": 1_789_000_000u64,
                    "plan_id": 1, "banned": 0,
                    "remind_expire": i64::from(state.remind_expire),
                    "remind_traffic": i64::from(state.remind_traffic),
                    "auto_renewal": i64::from(state.auto_renewal),
                    "auto_renewal": 0, "device_limit": 5
                } }),
            ),
            None => (403, json!({ "message": "未登录或登陆已过期" })),
        },
        ("GET", "/api/v1/user/getSubscribe") => match auth.and_then(|a| state.sessions.get(a)) {
            Some(_) => (
                200,
                json!({ "data": {
                        "plan_id": 1, "token": state.sub_token,
                        "subscribe_url": format!("http://127.0.0.1:{}/sub?token={}", state.port, state.sub_token),
                        "u": 26_843_545_600u64, "d": 26_843_545_600u64,
                        "transfer_enable": 107_374_182_400u64, "expired_at": 1_789_000_000u64,
                        "reset_day": 1, "alive_ip": 1, "allow_new_period": 0
                    } }),
            ),
            None => (403, json!({ "message": "未登录或登陆已过期" })),
        },
        ("GET", "/sub") => {
            if params.get("token").map(String::as_str) != Some(state.sub_token.as_str()) {
                return (403, json!({ "message": "token is error" }));
            }
            // 合法最小 profile：rules 数随版本变化（v0=5），供 reload 断言。
            let mut rules = vec!["MATCH,DIRECT".to_owned()];
            for i in 0..(4 + state.sub_version) {
                rules.push(format!("DOMAIN,r{i}.example.invalid,DIRECT"));
            }
            (
                200,
                json!({ "proxies": [], "proxy-groups": [], "rules": rules }),
            )
        }
        ("POST", "/api/v1/user/logout") => {
            if let Some(token) = auth {
                state.sessions.remove(token);
            }
            (200, json!({ "data": true }))
        }
        ("GET", "/api/v1/user/resetSecurity") => match auth.and_then(|a| state.sessions.get(a)) {
            Some(_) => {
                state.next_trade += 1;
                state.sub_token = format!("sub-token-{}", state.next_trade + 100);
                (
                    200,
                    json!({ "data": format!(
                        "http://127.0.0.1:{}/sub?token={}",
                        state.port, state.sub_token
                    ) }),
                )
            }
            None => (403, json!({ "message": "未登录或登陆已过期" })),
        },
        ("GET", "/api/v1/user/order/getPaymentMethod") => (
            200,
            json!({ "data": [ { "id": 3, "name": "支付宝二维码", "payment": "AlipayF2F",
                                "handling_fee_fixed": 0, "handling_fee_percent": 0 } ] }),
        ),
        ("POST", "/api/v1/user/order/save") => {
            if auth.and_then(|a| state.sessions.get(a)).is_none() {
                return (403, json!({ "message": "未登录或登陆已过期" }));
            }
            if state.orders.values().any(|&s| s == 0) {
                return (
                    500,
                    json!({ "message": "You have an unpaid or pending order" }),
                );
            }
            state.next_trade += 1;
            let trade_no = format!("MOCK{:08}", state.next_trade);
            state.orders.insert(trade_no.clone(), 0);
            (200, json!({ "data": trade_no }))
        }
        ("POST", "/api/v1/user/order/checkout") => {
            let trade_no = body["trade_no"].as_str().unwrap_or_default();
            match state.orders.get(trade_no) {
                Some(0) => (
                    200,
                    json!({ "type": 0, "data": "https://pay.example.invalid/qr/mock" }),
                ),
                _ => (
                    500,
                    json!({ "message": "Order does not exist or has been paid" }),
                ),
            }
        }
        ("GET", "/api/v1/user/order/check") => {
            let trade_no = params
                .get("trade_no")
                .map(String::as_str)
                .unwrap_or_default();
            match state.orders.get(trade_no) {
                Some(status) => (200, json!({ "data": status })),
                None => (500, json!({ "message": "Order does not exist" })),
            }
        }
        ("GET", "/update/manifest") => match &state.installer {
            Some(_) => (
                200,
                json!({
                    "version": "0.2.0",
                    "url": format!("http://127.0.0.1:{}/update/installer", state.port),
                    "sha256": state.installer_sha,
                }),
            ),
            None => (404, json!({ "message": "no installer configured" })),
        },
        ("GET", "/update/manifest-bad") => match &state.installer {
            Some(_) => (
                200,
                json!({
                    "version": "0.2.0",
                    "url": format!("http://127.0.0.1:{}/update/installer", state.port),
                    "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                }),
            ),
            None => (404, json!({ "message": "no installer configured" })),
        },
        ("POST", "/mock/bump") => {
            state.sub_version += 1;
            (200, json!({ "data": true }))
        }
        ("POST", "/mock/pay") => {
            let trade_no = body["trade_no"].as_str().unwrap_or_default();
            state.orders.insert(trade_no.to_owned(), 3);
            (200, json!({ "data": true }))
        }
        ("POST", "/api/v1/user/changePassword") => {
            let ok = auth.and_then(|a| state.sessions.get(a)).is_some();
            if !ok {
                return (403, json!({ "message": "未登录或登陆已过期" }));
            }
            let old = body["old_password"].as_str().unwrap_or_default();
            let new = body["new_password"].as_str().unwrap_or_default();
            if old != state.password || new.len() < 8 {
                return (500, json!({ "message": "The old password is wrong" }));
            }
            state.password = new.to_owned();
            state.sessions.clear();
            (200, json!({ "data": true }))
        }
        ("POST", "/api/v1/user/update") => {
            if auth.and_then(|a| state.sessions.get(a)).is_none() {
                return (403, json!({ "message": "未登录或登陆已过期" }));
            }
            if let Some(v) = body["remind_expire"].as_i64() {
                state.remind_expire = v != 0;
            }
            if let Some(v) = body["remind_traffic"].as_i64() {
                state.remind_traffic = v != 0;
            }
            if let Some(v) = body["auto_renewal"].as_i64() {
                state.auto_renewal = v != 0;
            }
            (200, json!({ "data": true }))
        }
        ("GET", "/api/v1/guest/comm/config") => (
            200,
            json!({ "data": {
                "tos_url": null, "app_url": null,
                "is_email_verify": 0, "is_invite_force": 0,
                "is_recaptcha": 0, "recaptcha_site_key": null,
                "app_description": "mock panel", "logo": null
            } }),
        ),
        ("GET", "/api/v1/user/plan/fetch") => (
            200,
            json!({ "data": [
                { "id": 1, "name": "基础套餐", "content": "100 GB · 3 设备",
                  "transfer_enable": 107_374_182_400u64, "show": 1, "renew": 1,
                  "month_price": 1000, "quarter_price": 2700, "year_price": 9600,
                  "capacity_limit": null },
                { "id": 2, "name": "高级套餐", "content": "300 GB · 5 设备",
                  "transfer_enable": 322_122_547_200u64, "show": 1, "renew": 1,
                  "month_price": 2000, "year_price": 19200, "onetime_price": 50000,
                  "capacity_limit": 10 }
            ] }),
        ),
        ("POST", "/api/v1/user/coupon/check") => {
            if body["code"].as_str() == Some("SAVE10") {
                (
                    200,
                    json!({ "data": { "name": "九折券", "type": 1, "value": 10 } }),
                )
            } else {
                (500, json!({ "message": "Coupon failed" }))
            }
        }
        ("GET", "/api/v1/user/order/fetch") => {
            let list: Vec<Value> = state
                .orders
                .iter()
                .map(|(trade_no, status)| {
                    json!({ "trade_no": trade_no, "status": status,
                            "total_amount": 1000, "period": "month_price" })
                })
                .collect();
            (200, json!({ "data": list }))
        }
        ("POST", "/api/v1/user/order/cancel") => {
            let trade_no = body["trade_no"].as_str().unwrap_or_default();
            match state.orders.get(trade_no) {
                Some(0) => {
                    state.orders.insert(trade_no.to_owned(), 2);
                    (200, json!({ "data": true }))
                }
                _ => (
                    500,
                    json!({ "message": "You can only cancel pending orders" }),
                ),
            }
        }
        _ => (404, json!({ "message": "not found" })),
    }
}

#[cfg(test)]
mod tests {
    use super::MockPanel;
    use crate::fetch::request;
    use serde_json::Value;

    async fn login(base: &str) -> String {
        let response = request(
            "POST",
            &format!("{base}/api/v1/passport/auth/login"),
            &[("Content-Type".to_owned(), "application/json".to_owned())],
            Some(br#"{"email":"a@b.c","password":"12345678"}"#),
        )
        .await
        .expect("login");
        assert_eq!(response.status, 200);
        let value: Value = serde_json::from_slice(&response.body).expect("json");
        value["data"]["auth_data"]
            .as_str()
            .expect("auth_data")
            .to_owned()
    }

    fn auth(token: &str) -> Vec<(String, String)> {
        vec![("Authorization".to_owned(), token.to_owned())]
    }

    #[tokio::test]
    async fn full_account_and_order_flow() {
        let panel = MockPanel::start().await.expect("start");
        let base = panel.base_url();
        let token = login(&base).await;

        let check = request(
            "GET",
            &format!("{base}/api/v1/user/checkLogin"),
            &auth(&token),
            None,
        )
        .await
        .expect("check");
        assert_eq!(check.status, 200);

        let subscribe = request(
            "GET",
            &format!("{base}/api/v1/user/getSubscribe"),
            &auth(&token),
            None,
        )
        .await
        .expect("subscribe");
        let value: Value = serde_json::from_slice(&subscribe.body).expect("json");
        let sub_url = value["data"]["subscribe_url"]
            .as_str()
            .expect("url")
            .to_owned();
        let sub = request("GET", &sub_url, &[], None).await.expect("sub");
        assert_eq!(sub.status, 200);
        let profile: Value = serde_json::from_slice(&sub.body).expect("profile json");
        assert_eq!(profile["rules"].as_array().expect("rules").len(), 5);

        let save = request(
            "POST",
            &format!("{base}/api/v1/user/order/save"),
            &auth(&token),
            Some(br#"{"plan_id":1,"period":"month_price"}"#),
        )
        .await
        .expect("save");
        let value: Value = serde_json::from_slice(&save.body).expect("json");
        let trade_no = value["data"].as_str().expect("trade_no").to_owned();

        let checkout = request(
            "POST",
            &format!("{base}/api/v1/user/order/checkout"),
            &auth(&token),
            Some(format!(r#"{{"trade_no":"{trade_no}","method":3}}"#).as_bytes()),
        )
        .await
        .expect("checkout");
        let value: Value = serde_json::from_slice(&checkout.body).expect("json");
        assert_eq!(value["type"], 0);

        let check = request(
            "GET",
            &format!("{base}/api/v1/user/order/check?trade_no={trade_no}"),
            &auth(&token),
            None,
        )
        .await
        .expect("order check");
        assert_eq!(check.body, b"{\"data\":0}");

        panel.pay_order(&trade_no);
        let check = request(
            "GET",
            &format!("{base}/api/v1/user/order/check?trade_no={trade_no}"),
            &auth(&token),
            None,
        )
        .await
        .expect("order check paid");
        assert_eq!(check.body, b"{\"data\":3}");

        let logout = request(
            "POST",
            &format!("{base}/api/v1/user/logout"),
            &auth(&token),
            None,
        )
        .await
        .expect("logout");
        assert_eq!(logout.status, 200);
        let check = request(
            "GET",
            &format!("{base}/api/v1/user/checkLogin"),
            &auth(&token),
            None,
        )
        .await
        .expect("check after logout");
        assert_eq!(check.status, 403);
    }

    #[tokio::test]
    async fn wrong_password_and_missing_auth() {
        let panel = MockPanel::start().await.expect("start");
        let base = panel.base_url();
        let bad = request(
            "POST",
            &format!("{base}/api/v1/passport/auth/login"),
            &[],
            Some(br#"{"email":"a@b.c","password":"short"}"#),
        )
        .await
        .expect("bad login");
        assert_eq!(bad.status, 500);
        let no_auth = request("GET", &format!("{base}/api/v1/user/info"), &[], None)
            .await
            .expect("no auth");
        assert_eq!(no_auth.status, 403);
    }
}
