//! NEW mode — QR onboarding via OAuth device-code flow.
//!
//! Inspired by `cc-connect/cmd/cc-connect/feishu.go:534-642` + `:685-700`.
//!
//! Flow:
//! 1. Construct `HttpOAuthClient` (Feishu by default, or `--platform-type`)
//! 2. Run `run_registration_flow` — init/begin/poll
//! 3. In `on_qr` callback: render QR to terminal (+ optional PNG via `image` crate)
//! 4. On success: write app_id/app_secret to config.toml

use std::path::Path;

use qrcode::render::unicode;
use qrcode::QrCode;

use super::args::{NewArgs, PlatformType};
use super::oauth::{run_registration_flow, HttpOAuthClient};
use super::{write_back_config, SetupError};

/// Run NEW mode: QR onboarding + write back to config.toml.
pub async fn run_new(args: NewArgs) -> Result<(), SetupError> {
    let platform = args.platform_type.unwrap_or(PlatformType::Feishu);
    let client = HttpOAuthClient::new(platform, args.debug);
    run_new_with(client, &args, platform).await
}

/// Injection entry for tests — accepts any OAuthClient impl.
pub async fn run_new_with<C>(
    client: C,
    args: &NewArgs,
    initial_platform: PlatformType,
) -> Result<(), SetupError>
where
    C: super::oauth::OAuthClient,
{
    let qr_image_path = args.qr_image_path.clone();
    let result = run_registration_flow(&client, args.timeout_seconds, args.debug, |url| {
        render_qr_to_terminal(url);
        if let Some(path) = &qr_image_path {
            if let Err(e) = render_qr_to_png(url, path) {
                eprintln!(
                    "Warning: failed to save QR image to {}: {}",
                    path.display(),
                    e
                );
            } else {
                println!("QR image saved to: {}", path.display());
            }
        }
    })
    .await?;

    write_back_config(&args.config_path, &result.app_id, &result.app_secret)?;
    println!(
        "✅ Onboarding complete. app_id={} platform={}",
        result.app_id,
        result.platform.as_str()
    );
    println!("   Config written to: {}", args.config_path.display());
    let _ = initial_platform; // future: switch client base_url mid-flow if needed
    Ok(())
}

/// Render QR to terminal using `qrcode` crate's unicode renderer.
fn render_qr_to_terminal(content: &str) {
    let code = match QrCode::new(content) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: failed to encode QR: {}", e);
            println!("URL: {}", content);
            return;
        }
    };
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    println!("\n{}\n", image);
    println!("URL: {}\n", content);
}

/// Save QR as a PNG file using `image` crate (replaces legacy self-written encoder).
fn render_qr_to_png(content: &str, path: &Path) -> Result<(), String> {
    let code = QrCode::new(content).map_err(|e| format!("encode QR: {e}"))?;
    let image = code.render::<image::Luma<u8>>().build();
    image.save(path).map_err(|e| format!("save PNG: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::args::{NewArgs, PlatformType};
    use crate::setup::oauth::OAuthClient;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    /// Fake OAuthClient returning a successful registration sequence.
    struct FakeSuccessClient {
        responses: Mutex<Vec<serde_json::Value>>,
    }

    impl FakeSuccessClient {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl OAuthClient for FakeSuccessClient {
        async fn call(
            &self,
            _action: &str,
            _params: &[(&str, &str)],
        ) -> Result<serde_json::Value, SetupError> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Err(SetupError::Http("no more responses".to_owned()))
            } else {
                Ok(responses.remove(0))
            }
        }
    }

    fn init_response() -> serde_json::Value {
        serde_json::json!({
            "supported_auth_methods": ["client_secret"],
            "error": "",
            "error_description": ""
        })
    }

    fn begin_response() -> serde_json::Value {
        serde_json::json!({
            "device_code": "dc",
            "verification_uri_complete": "https://example.com/qr",
            "interval": 1,
            "expire_in": 60,
            "error": "",
            "error_description": ""
        })
    }

    fn poll_success() -> serde_json::Value {
        serde_json::json!({
            "client_id": "cli_new_app",
            "client_secret": "sec_new_secret",
            "user_info": {"open_id": "ou_owner", "tenant_brand": "feishu"},
            "error": "",
            "error_description": ""
        })
    }

    fn new_args(temp: &NamedTempFile) -> NewArgs {
        NewArgs {
            config_path: temp.path().to_owned(),
            platform_type: None,
            timeout_seconds: 30,
            qr_image_path: None,
            debug: false,
        }
    }

    // ============ happy path ============

    #[tokio::test]
    async fn run_new_writes_credentials_on_success() {
        let temp = NamedTempFile::new().unwrap();
        let client =
            FakeSuccessClient::new(vec![init_response(), begin_response(), poll_success()]);

        let result = run_new_with(client, &new_args(&temp), PlatformType::Feishu).await;

        assert!(result.is_ok());
        let contents = std::fs::read_to_string(temp.path()).unwrap();
        assert!(contents.contains("app_id = \"cli_new_app\""));
        assert!(contents.contains("app_secret = \"sec_new_secret\""));
    }

    // ============ onboarding failure ============

    #[tokio::test]
    async fn run_new_oauth_failure_does_not_write_config() {
        let temp = NamedTempFile::new().unwrap();
        let initial = "[lark]\napp_id = \"old\"\n".to_owned();
        std::fs::write(temp.path(), &initial).unwrap();

        let client = FakeSuccessClient::new(vec![serde_json::json!({
            "supported_auth_methods": [],
            "error": "env_broken",
            "error_description": "nope"
        })]);

        let result = run_new_with(client, &new_args(&temp), PlatformType::Feishu).await;

        assert!(result.is_err());
        // config untouched
        let after = std::fs::read_to_string(temp.path()).unwrap();
        assert_eq!(after, initial);
    }

    // ============ qr rendering doesn't panic ============

    #[test]
    fn render_qr_to_terminal_handles_invalid_url() {
        // very long content that fails QR encoding shouldn't panic
        render_qr_to_terminal(&"x".repeat(3000));
    }

    #[test]
    fn render_qr_to_png_saves_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        render_qr_to_png("https://example.com/qr", &path).unwrap();
        assert!(path.exists());
        // PNG signature
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }

    #[test]
    fn render_qr_to_png_invalid_content_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.png");
        let err = render_qr_to_png(&"x".repeat(3000), &path);
        assert!(err.is_err());
    }
}
