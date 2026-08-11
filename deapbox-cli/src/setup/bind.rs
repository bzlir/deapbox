//! BIND 模式：校验已有 app_id/app_secret + 写回 config.toml。
//!
//! 对标 `cc-connect/cmd/cc-connect/feishu.go:464-532`（`validateAppCredentials`）。

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::args::BindArgs;
use super::error::SetupError;

const FEISHU_BASE: &str = "https://open.feishu.cn";
const LARK_BASE: &str = "https://open.larksuite.com";
const VALIDATE_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";

#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    #[serde(rename = "tenant_access_token", default)]
    tenant_access_token: String,
}

/// 凭证校验器抽象。生产用 `HttpCredentialValidator`，测试注入 fake。
#[async_trait::async_trait]
pub trait CredentialValidator: Send + Sync {
    /// 返回 `Ok(platform)` 表示校验通过（platform = "feishu" / "lark"）。
    async fn validate(&self, app_id: &str, app_secret: &str) -> Result<String, SetupError>;
}

/// 真实 HTTP 校验器：先打 feishu 域名，失败再打 lark 域名。
#[derive(Debug, Clone, Default)]
pub struct HttpCredentialValidator;

#[async_trait::async_trait]
impl CredentialValidator for HttpCredentialValidator {
    async fn validate(&self, app_id: &str, app_secret: &str) -> Result<String, SetupError> {
        match try_validate_at_base(FEISHU_BASE, app_id, app_secret).await {
            Ok(()) => Ok("feishu".to_string()),
            Err(e @ SetupError::OAuth { .. }) => Err(e),
            Err(_) => match try_validate_at_base(LARK_BASE, app_id, app_secret).await {
                Ok(()) => Ok("lark".to_string()),
                Err(e) => Err(e),
            },
        }
    }
}

async fn try_validate_at_base(
    base: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<(), SetupError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| SetupError::Http(e.to_string()))?;

    let url = format!("{base}{VALIDATE_PATH}");
    let body = serde_json::json!({
        "app_id": app_id,
        "app_secret": app_secret,
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| SetupError::Http(e.to_string()))?;

    let parsed: TenantTokenResponse = resp
        .json()
        .await
        .map_err(|e| SetupError::Http(format!("decode response: {e}")))?;

    if parsed.code == 0 && !parsed.tenant_access_token.is_empty() {
        return Ok(());
    }

    Err(SetupError::OAuth {
        code: parsed.code,
        msg: parsed.msg,
    })
}

/// BIND 模式入口（生产）。注入 `HttpCredentialValidator`。
pub async fn run_bind(args: BindArgs) -> Result<(), SetupError> {
    run_bind_with(args, &HttpCredentialValidator).await
}

/// BIND 模式入口（注入校验器）。测试用 fake validator 调用本函数。
pub async fn run_bind_with<V: CredentialValidator>(
    args: BindArgs,
    validator: &V,
) -> Result<(), SetupError> {
    let BindArgs {
        app_id,
        app_secret,
        config_path,
    } = args;

    println!("validating credentials against Feishu/Lark...");
    let platform = validator.validate(&app_id, &app_secret).await?;
    println!("✅ credentials verified (platform: {platform})");

    write_back_config(&config_path, &app_id, &app_secret)?;
    println!("✅ wrote [lark] section to {}", config_path.display());
    println!();
    println!("next steps:");
    println!("  cargo run -- --check-config   # verify config.toml");
    println!("  cargo run -- --dry-run        # construct Lark API");
    Ok(())
}

/// 写回 config.toml：若文件存在则 merge `[lark]` 段（保序保注释），不存在则新建。
pub fn write_back_config(path: &Path, app_id: &str, app_secret: &str) -> Result<(), SetupError> {
    let content = if path.exists() {
        std::fs::read_to_string(path)
            .map_err(|e| SetupError::WriteConfig(format!("read existing config: {e}")))?
    } else {
        String::new()
    };

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| SetupError::WriteConfig(format!("parse existing config: {e}")))?;

    let app_id_item = toml_edit::value(app_id);
    let app_secret_item = toml_edit::value(app_secret);

    if let Some(item) = doc.get_mut("lark") {
        let table = item.as_table_mut().ok_or_else(|| {
            SetupError::WriteConfig("[lark] section exists but is not a table".into())
        })?;
        table.insert("app_id", app_id_item);
        table.insert("app_secret", app_secret_item);
    } else {
        let mut lark_table = toml_edit::Table::new();
        lark_table.insert("app_id", app_id_item);
        lark_table.insert("app_secret", app_secret_item);
        doc.insert("lark", toml_edit::Item::Table(lark_table));
    }

    std::fs::write(path, doc.to_string())
        .map_err(|e| SetupError::WriteConfig(format!("write config: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct FakeValidator {
        result: Result<String, SetupError>,
    }

    #[async_trait]
    impl CredentialValidator for FakeValidator {
        async fn validate(&self, _app_id: &str, _app_secret: &str) -> Result<String, SetupError> {
            match &self.result {
                Ok(p) => Ok(p.clone()),
                Err(e) => Err(clone_error(e)),
            }
        }
    }

    fn clone_error(e: &SetupError) -> SetupError {
        match e {
            SetupError::InvalidArgs(s) => SetupError::InvalidArgs(s.clone()),
            SetupError::Http(s) => SetupError::Http(s.clone()),
            SetupError::OAuth { code, msg } => SetupError::OAuth {
                code: *code,
                msg: msg.clone(),
            },
            SetupError::WriteConfig(s) => SetupError::WriteConfig(s.clone()),
            SetupError::NotImplemented(s) => SetupError::NotImplemented(s),
        }
    }

    fn bind_args(dir: &Path, app_id: &str, app_secret: &str) -> BindArgs {
        BindArgs {
            app_id: app_id.to_string(),
            app_secret: app_secret.to_string(),
            config_path: dir.join("config.toml"),
        }
    }

    #[tokio::test]
    async fn bind_writes_config_when_validator_passes() {
        let dir = tempdir().unwrap();
        let validator = FakeValidator {
            result: Ok("feishu".into()),
        };

        run_bind_with(bind_args(dir.path(), "cli_a", "sec_b"), &validator)
            .await
            .unwrap();

        let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(written.contains("app_id = \"cli_a\""));
        assert!(written.contains("app_secret = \"sec_b\""));
        assert!(written.contains("[lark]"));
    }

    #[tokio::test]
    async fn bind_does_not_write_when_validator_fails() {
        let dir = tempdir().unwrap();
        let validator = FakeValidator {
            result: Err(SetupError::OAuth {
                code: 99,
                msg: "invalid credentials".into(),
            }),
        };

        let err = run_bind_with(bind_args(dir.path(), "bad", "bad"), &validator)
            .await
            .unwrap_err();
        assert!(matches!(err, SetupError::OAuth { .. }));
        assert!(!dir.path().join("config.toml").exists());
    }

    #[test]
    fn write_back_creates_new_file_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_back_config(&path, "cli_x", "sec_y").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[lark]"));
        assert!(written.contains("app_id = \"cli_x\""));
        assert!(written.contains("app_secret = \"sec_y\""));
    }

    #[test]
    fn write_back_merges_into_existing_file_preserving_other_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"# my comment
[lark]
app_id = "old_id"
app_secret = "old_secret"

[[agents]]
id = "claude-main"
kind = "claude-code"
command = "claude"
"#,
        )
        .unwrap();

        write_back_config(&path, "new_id", "new_secret").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# my comment"));
        assert!(written.contains("app_id = \"new_id\""));
        assert!(written.contains("app_secret = \"new_secret\""));
        assert!(!written.contains("old_id"));
        assert!(!written.contains("old_secret"));
        assert!(written.contains("[[agents]]"));
        assert!(written.contains("claude-main"));
    }

    #[test]
    fn write_back_inserts_lark_section_when_missing_from_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[agents]]\nid = \"x\"\nkind = \"codex\"\ncommand = \"codex\"\n",
        )
        .unwrap();

        write_back_config(&path, "injected_id", "injected_secret").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[lark]"));
        assert!(written.contains("app_id = \"injected_id\""));
        assert!(written.contains("app_secret = \"injected_secret\""));
        assert!(written.contains("[[agents]]"));
    }

    #[test]
    fn write_back_round_trip_is_loadable_by_store_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_back_config(&path, "cli_z", "sec_w").unwrap();

        let loaded = deapbox_store::config::load_config(&path).unwrap();
        assert_eq!(loaded.lark.app_id, "cli_z");
        assert_eq!(loaded.lark.app_secret, "sec_w");
    }

    #[test]
    fn write_back_rejects_when_lark_is_not_a_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "lark = \"not a table\"\n").unwrap();

        let err = write_back_config(&path, "id", "secret").unwrap_err();
        assert!(matches!(err, SetupError::WriteConfig(_)));
    }

    #[test]
    fn write_back_handles_colons_in_app_secret() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let secret = "abc:def_ghi";
        write_back_config(&path, "cli_a", secret).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(&format!("app_secret = \"{secret}\"")));

        let loaded = deapbox_store::config::load_config(&path).unwrap();
        assert_eq!(loaded.lark.app_secret, secret);
    }

    #[allow(dead_code)]
    fn _arc_validator(v: FakeValidator) -> Arc<dyn CredentialValidator> {
        Arc::new(v)
    }
}
