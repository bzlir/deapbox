//! BIND mode — validate existing app_id/app_secret + write back to config.toml.
//!
//! Inspired by `cc-connect/cmd/cc-connect/feishu.go:464-532` (`validateAppCredentials`).
//!
//! Flow:
//! 1. POST to `/open-apis/auth/v3/tenant_access_token/internal` with app_id/app_secret
//! 2. If `code == 0` and `tenant_access_token` is non-empty → valid
//! 3. Try feishu domain first; on network error (not OAuth rejection), try lark
//! 4. On success, write app_id/app_secret to config.toml

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::args::{BindArgs, PlatformType};
use super::write_back_config;
use super::SetupError;

const VALIDATE_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";

#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    #[serde(rename = "tenant_access_token", default)]
    tenant_access_token: String,
}

/// Credential validator seam — production uses `HttpCredentialValidator`,
/// tests inject `FakeCredentialValidator` (two adapters = real seam).
#[async_trait]
pub trait CredentialValidator: Send + Sync {
    /// Returns `Ok(platform)` if credentials are valid (platform = Feishu / Lark).
    async fn validate(&self, app_id: &str, app_secret: &str) -> Result<PlatformType, SetupError>;
}

/// Real HTTP validator. Tries Feishu domain first, falls back to Lark on
/// network errors (NOT on OAuth rejections — those are credential problems).
#[derive(Debug, Clone, Default)]
pub struct HttpCredentialValidator;

#[async_trait]
impl CredentialValidator for HttpCredentialValidator {
    async fn validate(&self, app_id: &str, app_secret: &str) -> Result<PlatformType, SetupError> {
        match try_validate_at(PlatformType::Feishu, app_id, app_secret).await {
            Ok(()) => Ok(PlatformType::Feishu),
            Err(SetupError::OAuth { .. }) => Err(SetupError::OAuth {
                code: "credential_rejected".to_owned(),
                msg: "feishu rejected the credentials".to_owned(),
            }),
            Err(_) => {
                // network error on feishu → try lark
                match try_validate_at(PlatformType::Lark, app_id, app_secret).await {
                    Ok(()) => Ok(PlatformType::Lark),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

async fn try_validate_at(
    platform: PlatformType,
    app_id: &str,
    app_secret: &str,
) -> Result<(), SetupError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| SetupError::Http(format!("build client: {e}")))?;

    let url = format!("{}{}", platform.base_url(), VALIDATE_PATH);
    let body = serde_json::json!({
        "app_id": app_id,
        "app_secret": app_secret,
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| SetupError::Http(format!("request: {e}")))?;

    let parsed: TenantTokenResponse = resp
        .json()
        .await
        .map_err(|e| SetupError::Http(format!("decode response: {e}")))?;

    if parsed.code == 0 && !parsed.tenant_access_token.is_empty() {
        Ok(())
    } else {
        Err(SetupError::OAuth {
            code: parsed.code.to_string(),
            msg: parsed.msg,
        })
    }
}

/// Run BIND mode: validate credentials + write to config.toml.
pub async fn run_bind(args: BindArgs) -> Result<(), SetupError> {
    run_bind_with(args, &HttpCredentialValidator).await
}

/// Injection entry point for tests.
pub async fn run_bind_with(
    args: BindArgs,
    validator: &dyn CredentialValidator,
) -> Result<(), SetupError> {
    let platform = validator.validate(&args.app_id, &args.app_secret).await?;
    write_back_config(&args.config_path, &args.app_id, &args.app_secret)?;
    println!(
        "✅ Credentials verified for app_id={} on platform={}.",
        args.app_id,
        platform.as_str()
    );
    println!("   Config written to: {}", args.config_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::args::{BindArgs, PlatformType};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    /// Fake validator — records calls, returns canned result.
    struct FakeCredentialValidator {
        result: Result<PlatformType, SetupError>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl FakeCredentialValidator {
        fn ok(platform: PlatformType) -> Self {
            Self {
                result: Ok(platform),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn failing(err: SetupError) -> Self {
            Self {
                result: Err(err),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last_call(&self) -> (String, String) {
            self.calls.lock().unwrap().last().cloned().unwrap()
        }
    }

    #[async_trait]
    impl CredentialValidator for FakeCredentialValidator {
        async fn validate(
            &self,
            app_id: &str,
            app_secret: &str,
        ) -> Result<PlatformType, SetupError> {
            self.calls
                .lock()
                .unwrap()
                .push((app_id.to_owned(), app_secret.to_owned()));
            self.result.clone()
        }
    }

    fn bind_args(temp: &NamedTempFile) -> BindArgs {
        BindArgs {
            app_id: "cli_test".to_owned(),
            app_secret: "sec_test".to_owned(),
            config_path: temp.path().to_owned(),
            platform_type: None,
        }
    }

    // ============ success path ============

    #[tokio::test]
    async fn run_bind_success_writes_config_and_returns_ok() {
        let temp = NamedTempFile::new().unwrap();
        let validator = FakeCredentialValidator::ok(PlatformType::Feishu);

        let result = run_bind_with(bind_args(&temp), &validator).await;

        assert!(result.is_ok());
        assert_eq!(validator.call_count(), 1);
        let (id, sec) = validator.last_call();
        assert_eq!(id, "cli_test");
        assert_eq!(sec, "sec_test");

        let contents = std::fs::read_to_string(temp.path()).unwrap();
        assert!(contents.contains("app_id = \"cli_test\""));
        assert!(contents.contains("app_secret = \"sec_test\""));
    }

    #[tokio::test]
    async fn run_bind_success_lark_platform() {
        let temp = NamedTempFile::new().unwrap();
        let validator = FakeCredentialValidator::ok(PlatformType::Lark);

        let result = run_bind_with(bind_args(&temp), &validator).await;

        assert!(result.is_ok());
    }

    // ============ failure paths ============

    #[tokio::test]
    async fn run_bind_credential_rejected_does_not_write_config() {
        let temp = NamedTempFile::new().unwrap();
        let initial_contents = "[lark]\napp_id = \"old\"\n".to_owned();
        std::fs::write(temp.path(), &initial_contents).unwrap();

        let validator = FakeCredentialValidator::failing(SetupError::OAuth {
            code: "10003".to_owned(),
            msg: "invalid param".to_owned(),
        });

        let result = run_bind_with(bind_args(&temp), &validator).await;

        assert!(result.is_err());
        // config should be untouched
        let after = std::fs::read_to_string(temp.path()).unwrap();
        assert_eq!(after, initial_contents);
    }

    #[tokio::test]
    async fn run_bind_http_error_does_not_write_config() {
        let temp = NamedTempFile::new().unwrap();
        let validator =
            FakeCredentialValidator::failing(SetupError::Http("network down".to_owned()));

        let result = run_bind_with(bind_args(&temp), &validator).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SetupError::Http(s) => assert!(s.contains("network down")),
            other => panic!("expected Http, got {:?}", other),
        }
    }
}
