//! NEW 模式：扫码 onboarding 自动创建飞书应用。
//!
//! 对标 `cc-connect/cmd/cc-connect/feishu.go:534-642`（`runRegistrationFlow`）。
//! 三步 OAuth（init/begin/poll）→ 终端画 QR → 拿到 client_id/client_secret
//! → 复用 bind.rs 的 `write_back_config` 写回 config.toml。

use super::args::NewArgs;
use super::bind::write_back_config;
use super::error::SetupError;
use super::oauth::{run_registration_flow, HttpOAuthClient, OAuthClient};
use super::qr::{AnsiQrRenderer, QrRenderer};

/// 生产入口：默认 `HttpOAuthClient` + `AnsiQrRenderer`。
pub async fn run_new(args: NewArgs) -> Result<(), SetupError> {
    let oauth = HttpOAuthClient { debug: args.debug };
    let renderer = AnsiQrRenderer;
    run_new_with(args, &oauth, &renderer).await
}

/// 注入入口：测试用 fake `OAuthClient` + fake `QrRenderer`。
pub async fn run_new_with<C, R>(args: NewArgs, oauth: &C, renderer: &R) -> Result<(), SetupError>
where
    C: OAuthClient,
    R: QrRenderer,
{
    println!("=== deapbox setup: NEW 模式（QR onboarding）===");
    println!();
    println!("请使用飞书 / Lark 手机 App 完成机器人创建与授权。");
    println!("（飞书会自动创建一个 PersonalAgent 应用并返回凭证。）");
    println!();

    let result = run_registration_flow(oauth, args.timeout_seconds, args.debug, |url| {
        // 总是先打 URL（对标 cc-connect/feishu.go:571-572），
        // 让无 TTY / 无 Unicode 渲染 / 窄终端 / CI 等场景都能复制链接到手机浏览器打开。
        println!("授权链接（复制到手机浏览器或飞书 App 打开）：");
        println!("  {url}");
        println!();

        if !args.no_qr {
            if let Err(err) = renderer.render_terminal(url) {
                eprintln!("warning: failed to render QR to terminal: {err}");
                eprintln!("（可用上方链接代替扫码）");
            }
        }

        if let Some(path) = &args.qr_image {
            if let Err(err) = renderer.render_png(url, path) {
                eprintln!("warning: failed to save QR image: {err}");
            } else {
                println!("QR image saved to: {}", path.display());
            }
        }
    })
    .await?;

    println!();
    println!("✅ onboarding complete (platform: {})", result.platform);
    println!("   app_id:        {}", result.app_id);
    println!("   owner_open_id:  {}", result.owner_open_id);
    println!();

    write_back_config(&args.config_path, &result.app_id, &result.app_secret)?;
    println!("✅ wrote [lark] section to {}", args.config_path.display());
    println!();
    println!("next steps:");
    println!("  cargo run -- --check-config   # verify config.toml");
    println!(
        "  deapbox setup bind --app {}:{}   # re-validate later",
        result.app_id, result.app_secret
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::oauth::{json_map, FakeOAuthClient};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    struct CapturingRenderer {
        terminal_calls: Arc<Mutex<Vec<String>>>,
        png_calls: Arc<Mutex<Vec<(String, std::path::PathBuf)>>>,
    }

    impl QrRenderer for CapturingRenderer {
        fn render_terminal(&self, content: &str) -> Result<(), SetupError> {
            self.terminal_calls
                .lock()
                .unwrap()
                .push(content.to_string());
            Ok(())
        }

        fn render_png(&self, content: &str, path: &std::path::Path) -> Result<(), SetupError> {
            self.png_calls
                .lock()
                .unwrap()
                .push((content.to_string(), path.to_path_buf()));
            Ok(())
        }
    }

    fn new_args(dir: &std::path::Path) -> NewArgs {
        NewArgs {
            config_path: dir.join("config.toml"),
            timeout_seconds: 30,
            qr_image: None,
            debug: false,
            no_qr: false,
        }
    }

    fn happy_client() -> FakeOAuthClient {
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc_x")),
            (
                "verification_uri_complete",
                serde_json::json!("https://example.com/qr?dc=x"),
            ),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(60)),
        ]);
        let poll = json_map(&[
            ("client_id", serde_json::json!("cli_new")),
            ("client_secret", serde_json::json!("sec_new")),
            (
                "user_info",
                serde_json::json!({"open_id": "ou_owner", "tenant_brand": "feishu"}),
            ),
        ]);
        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);
        client
    }

    #[tokio::test]
    async fn new_writes_config_after_successful_onboarding() {
        let dir = tempdir().unwrap();
        let client = happy_client();
        let renderer = CapturingRenderer {
            terminal_calls: Arc::new(Mutex::new(Vec::new())),
            png_calls: Arc::new(Mutex::new(Vec::new())),
        };

        run_new_with(new_args(dir.path()), &client, &renderer)
            .await
            .unwrap();

        assert_eq!(renderer.terminal_calls.lock().unwrap().len(), 1);
        assert!(renderer.terminal_calls.lock().unwrap()[0].contains("example.com/qr"));

        let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(written.contains("app_id = \"cli_new\""));
        assert!(written.contains("app_secret = \"sec_new\""));
    }

    #[tokio::test]
    async fn new_with_qr_image_option_saves_png() {
        let dir = tempdir().unwrap();
        let qr_path = dir.path().join("qr.png");
        let mut args = new_args(dir.path());
        args.qr_image = Some(qr_path.clone());

        let client = happy_client();
        let renderer = CapturingRenderer {
            terminal_calls: Arc::new(Mutex::new(Vec::new())),
            png_calls: Arc::new(Mutex::new(Vec::new())),
        };

        run_new_with(args, &client, &renderer).await.unwrap();

        let png_calls = renderer.png_calls.lock().unwrap();
        assert_eq!(png_calls.len(), 1);
        assert_eq!(png_calls[0].1, qr_path);
    }

    #[tokio::test]
    async fn new_does_not_write_config_when_onboarding_fails() {
        let dir = tempdir().unwrap();
        let init = json_map(&[(
            "supported_auth_methods",
            serde_json::json!(["client_secret"]),
        )]);
        let begin = json_map(&[
            ("device_code", serde_json::json!("dc")),
            ("verification_uri_complete", serde_json::json!("https://x")),
            ("interval", serde_json::json!(1)),
            ("expire_in", serde_json::json!(5)),
        ]);
        let poll = json_map(&[("error", serde_json::json!("access_denied"))]);
        let client = FakeOAuthClient::new(init, begin);
        client.push_poll(poll);

        let renderer = CapturingRenderer {
            terminal_calls: Arc::new(Mutex::new(Vec::new())),
            png_calls: Arc::new(Mutex::new(Vec::new())),
        };

        let err = run_new_with(new_args(dir.path()), &client, &renderer)
            .await
            .unwrap_err();

        assert!(matches!(err, SetupError::OAuth { .. }));
        assert!(!dir.path().join("config.toml").exists());
    }

    #[tokio::test]
    async fn new_writes_lark_section_loadable_by_store() {
        let dir = tempdir().unwrap();
        let client = happy_client();
        let renderer = CapturingRenderer {
            terminal_calls: Arc::new(Mutex::new(Vec::new())),
            png_calls: Arc::new(Mutex::new(Vec::new())),
        };

        run_new_with(new_args(dir.path()), &client, &renderer)
            .await
            .unwrap();

        let loaded = deapbox_store::config::load_config(dir.path().join("config.toml")).unwrap();
        assert_eq!(loaded.lark.app_id, "cli_new");
        assert_eq!(loaded.lark.app_secret, "sec_new");
    }

    #[tokio::test]
    async fn new_with_no_qr_skips_terminal_render() {
        let dir = tempdir().unwrap();
        let mut args = new_args(dir.path());
        args.no_qr = true;
        let client = happy_client();
        let renderer = CapturingRenderer {
            terminal_calls: Arc::new(Mutex::new(Vec::new())),
            png_calls: Arc::new(Mutex::new(Vec::new())),
        };

        run_new_with(args, &client, &renderer).await.unwrap();

        // 链接已通过 println 输出，但 QR 不应被渲染
        assert!(renderer.terminal_calls.lock().unwrap().is_empty());
        // config 仍正常写回
        let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(written.contains("cli_new"));
    }
}
