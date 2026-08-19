//! Feishu OAuth device-code flow — `init` → `begin` → `poll` loop.
//!
//! Inspired by `cc-connect/cmd/cc-connect/feishu.go:534-642` (`runRegistrationFlow`).
//!
//! Flow:
//! 1. `init` — check environment supports `client_secret` auth method
//! 2. `begin` — get `device_code` + `verification_uri_complete` (scan URL)
//! 3. `poll` loop — every `interval` seconds, until `client_id`+`client_secret`
//!    arrive, or `access_denied` / `expired_token` / timeout
//!
//! Platform auto-switch: if `poll.user_info.tenant_brand == "lark"`, switch the
//! onboarding domain to Lark and continue polling (cc-connect behavior).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::args::PlatformType;
use super::SetupError;

const REGISTRATION_PATH: &str = "/oauth/v1/app/registration";

#[derive(Debug, Deserialize)]
pub struct InitResponse {
    #[serde(default, rename = "supported_auth_methods")]
    supported_auth_methods: Vec<String>,
    #[serde(default)]
    error: String,
    #[serde(default, rename = "error_description")]
    error_description: String,
}

#[derive(Debug, Deserialize)]
pub struct BeginResponse {
    #[serde(default, rename = "device_code")]
    pub device_code: String,
    #[serde(default, rename = "verification_uri_complete")]
    pub verification_uri_complete: String,
    #[serde(default)]
    pub interval: i64,
    #[serde(default, rename = "expire_in")]
    pub expire_in: i64,
    #[serde(default)]
    pub error: String,
    #[serde(default, rename = "error_description")]
    pub error_description: String,
}

#[derive(Debug, Deserialize)]
pub struct PollResponse {
    #[serde(default, rename = "client_id")]
    pub client_id: String,
    #[serde(default, rename = "client_secret")]
    pub client_secret: String,
    #[serde(default, rename = "user_info")]
    pub user_info: PollUserInfo,
    #[serde(default)]
    pub error: String,
    #[serde(default, rename = "error_description")]
    pub error_description: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct PollUserInfo {
    #[serde(default, rename = "open_id")]
    pub open_id: String,
    #[serde(default, rename = "tenant_brand")]
    pub tenant_brand: String,
}

/// Result of a successful onboarding flow — credentials + identity info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationResult {
    pub app_id: String,
    pub app_secret: String,
    pub owner_open_id: String,
    pub platform: PlatformType,
}

/// OAuth client seam — production uses `HttpOAuthClient`, tests inject fakes
/// (two adapters = real seam).
#[async_trait]
pub trait OAuthClient: Send + Sync {
    /// Call one OAuth action (`init` / `begin` / `poll`) and decode the JSON body.
    async fn call(
        &self,
        action: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value, SetupError>;
}

/// Real HTTP OAuth client using `reqwest` form POST.
#[derive(Debug, Clone)]
pub struct HttpOAuthClient {
    base_url: String,
    debug: bool,
}

impl HttpOAuthClient {
    pub fn new(platform: PlatformType, debug: bool) -> Self {
        Self {
            base_url: platform.accounts_base_url().to_owned(),
            debug,
        }
    }

    /// Switch the onboarding domain (feishu ↔ lark) mid-flow.
    pub fn switch_base_url(&mut self, platform: PlatformType) {
        self.base_url = platform.accounts_base_url().to_owned();
    }
}

#[async_trait]
impl OAuthClient for HttpOAuthClient {
    async fn call(
        &self,
        action: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value, SetupError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| SetupError::Http(format!("build client: {e}")))?;

        let url = format!("{}{}", self.base_url, REGISTRATION_PATH);
        let mut form = reqwest::multipart::Form::new();
        form = form.text("action", action.to_owned());
        for (k, v) in params {
            form = form.text((*k).to_owned(), (*v).to_owned());
        }

        let resp = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| SetupError::Http(format!("request: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SetupError::Http(format!("read body: {e}")))?;

        if self.debug {
            eprintln!(
                "[debug] registration action={} status={} body={}",
                action,
                status,
                body.trim()
            );
        }

        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| SetupError::Http(format!("decode response: {e}")))?;
        Ok(value)
    }
}

/// Decode helpers — public for tests, used by `run_registration_flow`.
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

/// Run the full OAuth device-code onboarding flow.
///
/// `on_qr` is called when the verification URL is available — the caller
/// (new.rs) renders the QR code to terminal / saves PNG. The function polls
/// until credentials arrive, the user denies, the session expires, or the
/// deadline (min of `expire_in` and `timeout_seconds`) is reached.
pub async fn run_registration_flow<C, F>(
    client: &C,
    timeout_seconds: u64,
    debug: bool,
    mut on_qr: F,
) -> Result<RegistrationResult, SetupError>
where
    C: OAuthClient,
    F: FnMut(&str) + Send,
{
    let _ = debug;

    // 1. init
    let init_value = client.call("init", &[]).await?;
    let init_res = parse_init_response(init_value)?;
    if !init_res.error.is_empty() {
        return Err(SetupError::OAuth {
            code: init_res.error,
            msg: init_res.error_description,
        });
    }
    if !init_res.supported_auth_methods.is_empty()
        && !init_res
            .supported_auth_methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case("client_secret"))
    {
        return Err(SetupError::OAuth {
            code: "unsupported_method".to_owned(),
            msg: "current environment does not support client_secret auth".to_owned(),
        });
    }

    // 2. begin
    let begin_value = client
        .call(
            "begin",
            &[
                ("archetype", "PersonalAgent"),
                ("auth_method", "client_secret"),
                ("request_user_info", "open_id"),
            ],
        )
        .await?;
    let begin_res = parse_begin_response(begin_value)?;
    if !begin_res.error.is_empty() {
        return Err(SetupError::OAuth {
            code: begin_res.error,
            msg: begin_res.error_description,
        });
    }
    if begin_res.device_code.is_empty() || begin_res.verification_uri_complete.is_empty() {
        return Err(SetupError::OAuth {
            code: "incomplete_response".to_owned(),
            msg: "begin returned no device_code or verification_uri_complete".to_owned(),
        });
    }

    on_qr(&begin_res.verification_uri_complete);

    // 3. poll loop
    let mut interval = if begin_res.interval > 0 {
        begin_res.interval as u64
    } else {
        5
    };
    let expire_in = if begin_res.expire_in > 0 {
        begin_res.expire_in as u64
    } else {
        timeout_seconds
    };
    let deadline =
        std::time::Instant::now() + Duration::from_secs(std::cmp::min(expire_in, timeout_seconds));

    let mut platform = PlatformType::Feishu;

    while std::time::Instant::now() < deadline {
        let poll_value = client
            .call("poll", &[("device_code", begin_res.device_code.as_str())])
            .await?;
        let poll_res = parse_poll_response(poll_value)?;

        // platform auto-switch (informational only in trait-based flow;
        // HttpOAuthClient would switch base_url here — left as future work)
        let tenant_brand = poll_res.user_info.tenant_brand.to_lowercase();
        if tenant_brand == "lark" && platform != PlatformType::Lark {
            platform = PlatformType::Lark;
            // fall through to credential check on the same poll response
        }

        if !poll_res.client_id.is_empty() && !poll_res.client_secret.is_empty() {
            return Ok(RegistrationResult {
                app_id: poll_res.client_id,
                app_secret: poll_res.client_secret,
                owner_open_id: poll_res.user_info.open_id,
                platform,
            });
        }

        match poll_res.error.as_str() {
            "" | "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => return Err(SetupError::AccessDenied),
            "expired_token" => return Err(SetupError::ExpiredToken),
            other => {
                return Err(SetupError::OAuth {
                    code: other.to_owned(),
                    msg: poll_res.error_description,
                });
            }
        }

        tokio::time::sleep(Duration::from_secs(interval)).await;
    }

    Err(SetupError::Timeout(timeout_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Fake OAuthClient that returns canned responses in sequence.
    struct FakeOAuthClient {
        responses: Mutex<Vec<serde_json::Value>>,
        call_count: AtomicUsize,
    }

    impl FakeOAuthClient {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: Mutex::new(responses),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl OAuthClient for FakeOAuthClient {
        async fn call(
            &self,
            _action: &str,
            _params: &[(&str, &str)],
        ) -> Result<serde_json::Value, SetupError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut responses = self.responses.lock().unwrap();
            if !responses.is_empty() {
                Ok(responses.remove(0))
            } else {
                Err(SetupError::Http("no more canned responses".to_owned()))
            }
        }
    }

    fn init_response_ok() -> serde_json::Value {
        serde_json::json!({
            "supported_auth_methods": ["client_secret"],
            "error": "",
            "error_description": ""
        })
    }

    fn begin_response_ok() -> serde_json::Value {
        serde_json::json!({
            "device_code": "dc_test",
            "verification_uri_complete": "https://example.com/qr?device_code=dc_test",
            "interval": 1,
            "expire_in": 60,
            "error": "",
            "error_description": ""
        })
    }

    fn poll_response_pending() -> serde_json::Value {
        serde_json::json!({
            "client_id": "",
            "client_secret": "",
            "user_info": {"open_id": "", "tenant_brand": ""},
            "error": "authorization_pending",
            "error_description": ""
        })
    }

    fn poll_response_success() -> serde_json::Value {
        serde_json::json!({
            "client_id": "cli_new",
            "client_secret": "sec_new",
            "user_info": {"open_id": "ou_owner", "tenant_brand": "feishu"},
            "error": "",
            "error_description": ""
        })
    }

    fn poll_response_lark() -> serde_json::Value {
        serde_json::json!({
            "client_id": "cli_lark",
            "client_secret": "sec_lark",
            "user_info": {"open_id": "ou_lark", "tenant_brand": "lark"},
            "error": "",
            "error_description": ""
        })
    }

    // ============ happy path ============

    #[tokio::test]
    async fn happy_path_init_begin_poll_success() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            begin_response_ok(),
            poll_response_success(),
        ]);
        let qr_calls = Mutex::new(Vec::new());

        let result = run_registration_flow(&client, 30, false, |url| {
            qr_calls.lock().unwrap().push(url.to_owned());
        })
        .await
        .unwrap();

        assert_eq!(result.app_id, "cli_new");
        assert_eq!(result.app_secret, "sec_new");
        assert_eq!(result.owner_open_id, "ou_owner");
        assert_eq!(result.platform, PlatformType::Feishu);

        let qr = qr_calls.lock().unwrap();
        assert_eq!(qr.len(), 1);
        assert!(qr[0].contains("device_code=dc_test"));
    }

    // ============ pending → success ============

    #[tokio::test]
    async fn pending_then_success_eventually_succeeds() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            begin_response_ok(),
            poll_response_pending(),
            poll_response_pending(),
            poll_response_success(),
        ]);

        let result = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap();

        assert_eq!(result.app_id, "cli_new");
    }

    // ============ lark platform auto-switch ============

    #[tokio::test]
    async fn lark_tenant_brand_switches_platform() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            begin_response_ok(),
            poll_response_lark(),
        ]);

        let result = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap();

        assert_eq!(result.platform, PlatformType::Lark);
        assert_eq!(result.app_id, "cli_lark");
    }

    // ============ access_denied ============

    #[tokio::test]
    async fn access_denied_returns_access_denied_error() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            begin_response_ok(),
            serde_json::json!({
                "client_id": "",
                "client_secret": "",
                "user_info": {"open_id": "", "tenant_brand": ""},
                "error": "access_denied",
                "error_description": "user said no"
            }),
        ]);

        let err = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::AccessDenied));
    }

    // ============ expired_token ============

    #[tokio::test]
    async fn expired_token_returns_expired_error() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            begin_response_ok(),
            serde_json::json!({
                "error": "expired_token",
                "error_description": "session expired"
            }),
        ]);

        let err = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::ExpiredToken));
    }

    // ============ init error ============

    #[tokio::test]
    async fn init_error_returns_oauth_error() {
        let client = FakeOAuthClient::new(vec![serde_json::json!({
            "supported_auth_methods": [],
            "error": "unsupported_env",
            "error_description": "env doesn't support this"
        })]);

        let err = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap_err();
        match err {
            SetupError::OAuth { code, .. } => assert_eq!(code, "unsupported_env"),
            other => panic!("expected OAuth, got {:?}", other),
        }
    }

    // ============ unsupported auth method ============

    #[tokio::test]
    async fn init_unsupported_auth_method_rejected() {
        let client = FakeOAuthClient::new(vec![serde_json::json!({
            "supported_auth_methods": ["some_other_method"],
            "error": "",
            "error_description": ""
        })]);

        let err = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap_err();
        match err {
            SetupError::OAuth { code, .. } => assert_eq!(code, "unsupported_method"),
            other => panic!("expected OAuth, got {:?}", other),
        }
    }

    // ============ begin incomplete ============

    #[tokio::test]
    async fn begin_missing_device_code_rejected() {
        let client = FakeOAuthClient::new(vec![
            init_response_ok(),
            serde_json::json!({
                "device_code": "",
                "verification_uri_complete": "https://x.com",
                "error": "",
                "error_description": ""
            }),
        ]);

        let err = run_registration_flow(&client, 30, false, |_| {})
            .await
            .unwrap_err();
        match err {
            SetupError::OAuth { code, .. } => assert_eq!(code, "incomplete_response"),
            other => panic!("expected OAuth, got {:?}", other),
        }
    }

    // ============ timeout ============

    #[tokio::test]
    async fn timeout_when_poll_never_succeeds() {
        // expire_in=1 + timeout=1 → deadline ~1s. All polls pending.
        let begin = serde_json::json!({
            "device_code": "dc",
            "verification_uri_complete": "https://x.com",
            "interval": 1,
            "expire_in": 1,
            "error": "",
            "error_description": ""
        });

        // Provide many pending responses so we don't run out
        let mut responses = vec![init_response_ok(), begin];
        for _ in 0..10 {
            responses.push(poll_response_pending());
        }
        let client = FakeOAuthClient::new(responses);

        let err = run_registration_flow(&client, 1, false, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::Timeout(1)));
    }
}
