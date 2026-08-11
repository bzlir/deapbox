//! 飞书 OAuth 设备码注册流程。
//!
//! 对标 `cc-connect/cmd/cc-connect/feishu.go:534-674`（`runRegistrationFlow` +
//! `registrationCall`）。三步：init → begin → poll 循环。
//!
//! 用户扫码后飞书自动创建一个 PersonalAgent archetype 的应用，返回
//! `client_id` + `client_secret` + `user_info.open_id`，写入 config.toml。

use std::time::Duration;

use serde::Deserialize;

use super::error::SetupError;

pub const FEISHU_ACCOUNTS_BASE: &str = "https://accounts.feishu.cn";
pub const LARK_ACCOUNTS_BASE: &str = "https://accounts.larksuite.com";
const REGISTRATION_PATH: &str = "/oauth/v1/app/registration";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InitResponse {
    #[serde(default)]
    pub supported_auth_methods: Vec<String>,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeginResponse {
    #[serde(default)]
    pub device_code: String,
    #[serde(default)]
    pub verification_uri_complete: String,
    #[serde(default, rename = "interval")]
    pub interval: i64,
    #[serde(default, rename = "expire_in")]
    pub expire_in: i64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PollUserInfo {
    #[serde(default)]
    pub open_id: String,
    #[serde(default)]
    pub tenant_brand: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PollResponse {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub user_info: PollUserInfo,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationResult {
    pub app_id: String,
    pub app_secret: String,
    pub owner_open_id: String,
    pub platform: String,
}

/// OAuth HTTP 客户端抽象。生产用 `HttpOAuthClient`（reqwest），测试注入 fake。
#[async_trait::async_trait]
pub trait OAuthClient: Send + Sync {
    /// POST 一次 `oauth/v1/app/registration` 端点。
    /// `action` = "init" / "begin" / "poll"；`extra_params` 是除 action 外的 form 字段。
    async fn registration_call(
        &self,
        base_url: &str,
        action: &str,
        extra_params: &[(&str, &str)],
    ) -> Result<serde_json::Value, SetupError>;
}

/// reqwest 真实实现。
#[derive(Debug, Clone, Default)]
pub struct HttpOAuthClient {
    pub debug: bool,
}

#[async_trait::async_trait]
impl OAuthClient for HttpOAuthClient {
    async fn registration_call(
        &self,
        base_url: &str,
        action: &str,
        extra_params: &[(&str, &str)],
    ) -> Result<serde_json::Value, SetupError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| SetupError::Http(e.to_string()))?;

        let url = format!("{base_url}{REGISTRATION_PATH}");
        let mut form = vec![("action", action)];
        for (k, v) in extra_params {
            form.push((k, v));
        }

        if self.debug {
            eprintln!("[debug] registration action={action} base={base_url}");
        }

        let resp = client
            .post(&url)
            .form(&form)
            .send()
            .await
            .map_err(|e| SetupError::Http(e.to_string()))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SetupError::Http(format!("read body: {e}")))?;

        if self.debug {
            eprintln!("[debug] action={action} status={status} body={body}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| SetupError::Http(format!("decode response: {e}")))?;

        Ok(parsed)
    }
}

/// 拆 `registration_call` 返回的 Value 到具体类型。让测试可注入预定义 JSON。
pub fn parse_init_response(value: serde_json::Value) -> Result<InitResponse, SetupError> {
    serde_json::from_value(value)
        .map_err(|e| SetupError::Http(format!("decode init response: {e}")))
}

pub fn parse_begin_response(value: serde_json::Value) -> Result<BeginResponse, SetupError> {
    serde_json::from_value(value)
        .map_err(|e| SetupError::Http(format!("decode begin response: {e}")))
}

pub fn parse_poll_response(value: serde_json::Value) -> Result<PollResponse, SetupError> {
    serde_json::from_value(value)
        .map_err(|e| SetupError::Http(format!("decode poll response: {e}")))
}

/// 把 `Value` 里的 `error` / `error_description` 抽出成 SetupError::OAuth。
pub fn oauth_error_from_value(value: &serde_json::Value) -> Option<SetupError> {
    let err = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if err.is_empty() {
        return None;
    }
    let desc = value
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Some(SetupError::OAuth {
        code: 0,
        msg: if desc.is_empty() {
            err.to_string()
        } else {
            format!("{err}: {desc}")
        },
    })
}

/// 跑完整 OAuth 注册流程。对标 `cc-connect/feishu.go:534-642`（`runRegistrationFlow`）。
///
/// 调用方提供 `OAuthClient`（HTTP 抽象）和 `on_qr: Fn(&str)` 回调
/// （生产里把 `verification_uri_complete` 渲染成终端 QR / PNG）。
pub async fn run_registration_flow<C, F>(
    client: &C,
    timeout_seconds: u64,
    debug: bool,
    mut on_qr: F,
) -> Result<RegistrationResult, SetupError>
where
    C: OAuthClient,
    F: FnMut(&str),
{
    let _ = debug;

    let mut base_url = FEISHU_ACCOUNTS_BASE.to_string();

    // Step 1: init
    let init_value = client.registration_call(&base_url, "init", &[]).await?;
    if let Some(err) = oauth_error_from_value(&init_value) {
        return Err(err);
    }
    let init = parse_init_response(init_value)?;
    if !init.supported_auth_methods.is_empty()
        && !init
            .supported_auth_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case("client_secret"))
    {
        return Err(SetupError::OAuth {
            code: -1,
            msg: "current environment does not support client_secret auth".into(),
        });
    }

    // Step 2: begin
    let begin_value = client
        .registration_call(
            &base_url,
            "begin",
            &[
                ("archetype", "PersonalAgent"),
                ("auth_method", "client_secret"),
                ("request_user_info", "open_id"),
            ],
        )
        .await?;
    if let Some(err) = oauth_error_from_value(&begin_value) {
        return Err(err);
    }
    let begin = parse_begin_response(begin_value)?;
    if begin.device_code.is_empty() || begin.verification_uri_complete.is_empty() {
        return Err(SetupError::OAuth {
            code: -1,
            msg: "incomplete onboarding response (missing device_code or verification_uri)".into(),
        });
    }

    on_qr(&begin.verification_uri_complete);

    let mut interval = if begin.interval > 0 {
        begin.interval
    } else {
        5
    };
    let expire_in = if begin.expire_in > 0 {
        begin.expire_in
    } else {
        timeout_seconds.min(i64::MAX as u64) as i64
    };

    let deadline = std::time::Instant::now()
        + Duration::from_secs(timeout_seconds.min(expire_in.max(0) as u64));

    // Step 3: poll loop
    let mut platform = "feishu".to_string();
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval.max(1) as u64)).await;

        let poll_value = client
            .registration_call(&base_url, "poll", &[("device_code", &begin.device_code)])
            .await?;
        let poll = parse_poll_response(poll_value)?;

        let tenant_brand = poll.user_info.tenant_brand.trim().to_lowercase();
        if tenant_brand == "lark" && base_url != LARK_ACCOUNTS_BASE {
            base_url = LARK_ACCOUNTS_BASE.to_string();
            platform = "lark".to_string();
            continue;
        }

        if !poll.client_id.is_empty() && !poll.client_secret.is_empty() {
            return Ok(RegistrationResult {
                app_id: poll.client_id,
                app_secret: poll.client_secret,
                owner_open_id: poll.user_info.open_id,
                platform,
            });
        }

        match poll.error.as_str() {
            "" | "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => {
                return Err(SetupError::OAuth {
                    code: -1,
                    msg: "authorization denied by user".into(),
                });
            }
            "expired_token" => {
                return Err(SetupError::OAuth {
                    code: -1,
                    msg: "onboarding session expired".into(),
                });
            }
            other => {
                let desc = if poll.error_description.is_empty() {
                    other.to_string()
                } else {
                    format!("{other}: {}", poll.error_description)
                };
                return Err(SetupError::OAuth {
                    code: -1,
                    msg: desc,
                });
            }
        }
    }

    Err(SetupError::OAuth {
        code: -1,
        msg: "timed out waiting for QR onboarding result".into(),
    })
}

/// 用于测试的预定义响应序列。按 (action, params) 关键字匹配，返回下一个响应。
#[cfg(test)]
pub struct FakeOAuthClient {
    pub init: serde_json::Value,
    pub begin: serde_json::Value,
    pub polls: std::sync::Mutex<std::collections::VecDeque<serde_json::Value>>,
    pub calls: std::sync::Mutex<Vec<(String, String)>>,
}

#[cfg(test)]
impl FakeOAuthClient {
    pub fn new(init: serde_json::Value, begin: serde_json::Value) -> Self {
        Self {
            init,
            begin,
            polls: std::sync::Mutex::new(std::collections::VecDeque::new()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn push_poll(&self, value: serde_json::Value) {
        self.polls.lock().unwrap().push_back(value);
    }

    pub fn calls_snapshot(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl OAuthClient for FakeOAuthClient {
    async fn registration_call(
        &self,
        base_url: &str,
        action: &str,
        extra_params: &[(&str, &str)],
    ) -> Result<serde_json::Value, SetupError> {
        let params_str = extra_params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        self.calls
            .lock()
            .unwrap()
            .push((action.to_string(), params_str));

        let _ = base_url;
        match action {
            "init" => Ok(self.init.clone()),
            "begin" => Ok(self.begin.clone()),
            "poll" => self
                .polls
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| SetupError::Http("fake poll queue exhausted".into())),
            other => Err(SetupError::Http(format!("unknown action: {other}"))),
        }
    }
}

/// 方便构造 HashMap 形式的 JSON 响应。
#[cfg(test)]
pub fn json_map(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut map = std::collections::HashMap::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    serde_json::to_value(map).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn happy_path_returns_credentials_after_poll() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_123")),
            (
                "verification_uri_complete",
                serde_json::json!("https://example.com/qr?dc=123"),
            ),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(60)),
        ]);
        let poll = json_map(&[
            ("client_id", serde_json::json!("cli_created")),
            ("client_secret", serde_json::json!("sec_created")),
            (
                "user_info",
                serde_json::json!({"open_id": "ou_owner", "tenant_brand": "feishu"}),
            ),
        ]);

        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);

        let mut qr_seen = Vec::new();
        let result = run_registration_flow(&client, 30, false, |url| qr_seen.push(url.to_string()))
            .await
            .unwrap();

        assert_eq!(result.app_id, "cli_created");
        assert_eq!(result.app_secret, "sec_created");
        assert_eq!(result.owner_open_id, "ou_owner");
        assert_eq!(result.platform, "feishu");
        assert_eq!(qr_seen, vec!["https://example.com/qr?dc=123"]);

        let calls = client.calls_snapshot();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "init");
        assert_eq!(calls[1].0, "begin");
        assert!(calls[1].1.contains("archetype=PersonalAgent"));
        assert!(calls[1].1.contains("auth_method=client_secret"));
        assert!(calls[1].1.contains("request_user_info=open_id"));
        assert_eq!(calls[2].0, "poll");
        assert!(calls[2].1.contains("device_code=dc_123"));
    }

    #[tokio::test]
    async fn unsupported_auth_method_rejected() {
        let init = json_map(&[("supported_auth_methods", serde_json::json!(["legacy"]))]);
        let begin = json_map(&[]);
        let client = FakeOAuthClient::new(init, begin);

        let err = run_registration_flow(&client, 5, false, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::OAuth { .. }));
        assert!(err.to_string().contains("client_secret"));
    }

    #[tokio::test]
    async fn access_denied_returns_error() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_x")),
            ("verification_uri_complete", serde_json::json!("https://x")),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(30)),
        ]);
        let poll = json_map(&[("error", serde_json::json!("access_denied"))]);

        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);

        let err = run_registration_flow(&client, 10, false, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::OAuth { .. }));
        assert!(err.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn expired_token_returns_error() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_x")),
            ("verification_uri_complete", serde_json::json!("https://x")),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(30)),
        ]);
        let poll = json_map(&[("error", serde_json::json!("expired_token"))]);

        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);

        let err = run_registration_flow(&client, 10, false, |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[tokio::test]
    async fn slow_down_increases_interval_then_succeeds() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_x")),
            ("verification_uri_complete", serde_json::json!("https://x")),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(120)),
        ]);
        let slow = json_map(&[("error", serde_json::json!("slow_down"))]);
        let pending = json_map(&[("error", serde_json::json!("authorization_pending"))]);
        let success = json_map(&[
            ("client_id", serde_json::json!("cli")),
            ("client_secret", serde_json::json!("sec")),
            (
                "user_info",
                serde_json::json!({"open_id": "ou_x", "tenant_brand": "feishu"}),
            ),
        ]);

        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(slow);
        client.push_poll(pending);
        client.push_poll(success);

        let result = run_registration_flow(&client, 60, false, |_| {})
            .await
            .unwrap();
        assert_eq!(result.app_id, "cli");
        assert_eq!(
            client
                .calls_snapshot()
                .iter()
                .filter(|(a, _)| a == "poll")
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn incomplete_begin_response_rejected() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[("device_code", serde_json::json!(""))]); // missing fields
        let client = FakeOAuthClient::new(init, begin);

        let err = run_registration_flow(&client, 5, false, |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("incomplete"));
    }

    #[tokio::test]
    async fn init_with_explicit_error_rejected() {
        let init = json_map(&[
            ("error", serde_json::json!("unsupported_region")),
            ("error_description", serde_json::json!("region not allowed")),
        ]);
        let begin = json_map(&[]);
        let client = FakeOAuthClient::new(init, begin);

        let err = run_registration_flow(&client, 5, false, |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported_region"));
    }

    #[tokio::test]
    async fn unknown_poll_error_propagated() {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_x")),
            ("verification_uri_complete", serde_json::json!("https://x")),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(30)),
        ]);
        let poll = json_map(&[
            ("error", serde_json::json!("some_unexpected")),
            ("error_description", serde_json::json!("weird stuff")),
        ]);

        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);

        let err = run_registration_flow(&client, 10, false, |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("some_unexpected"));
        assert!(err.to_string().contains("weird stuff"));
    }
}
