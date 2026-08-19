//! `deapbox setup` — cold-start onboarding for the Feishu bot.
//!
//! Two flows inspired by `cc-connect/cmd/cc-connect/feishu.go`:
//! - **BIND**: operator already has app_id/app_secret → validate + write to `config.toml`
//! - **NEW**: operator has nothing → QR onboarding creates a Feishu PersonalAgent app
//!   via OAuth device-code flow, then writes credentials back to `config.toml`
//!
//! Auto-detect (no explicit `bind`/`new` subcommand):
//! - `--app` / `--app-id` / `--app-secret` present → BIND
//! - otherwise → NEW

pub mod args;
pub mod bind;
pub mod new;
pub mod oauth;

pub use args::{parse_args, BindArgs, NewArgs, ParseError, PlatformType, SetupCommand};
pub use bind::{run_bind, CredentialValidator, HttpCredentialValidator};
pub use new::run_new;
pub use oauth::{run_registration_flow, HttpOAuthClient, OAuthClient, RegistrationResult};

use std::path::Path;

/// Errors that can occur during setup.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SetupError {
    #[error("invalid arguments: {0}")]
    Args(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Feishu OAuth error: code={code} msg={msg}")]
    OAuth { code: String, msg: String },
    #[error("failed to write config: {0}")]
    WriteConfig(String),
    #[error("onboarding timed out after {0}s")]
    Timeout(u64),
    #[error("authorization denied by user")]
    AccessDenied,
    #[error("onboarding session expired")]
    ExpiredToken,
}

impl From<ParseError> for SetupError {
    fn from(err: ParseError) -> Self {
        SetupError::Args(err.to_string())
    }
}

/// Write app_id + app_secret back into `config.toml` at `path`.
///
/// If `path` doesn't exist, a fresh config is created from the embedded
/// `config.toml.example` template. If it exists, the `[lark]` section is
/// updated in-place preserving order and other sections (`[[agents]]`,
/// `[[sessions]]`, comments).
pub(super) fn write_back_config(
    path: &Path,
    app_id: &str,
    app_secret: &str,
) -> Result<(), SetupError> {
    const TEMPLATE: &str = include_str!("../../../config.toml.example");

    let existing = std::fs::read_to_string(path).unwrap_or_else(|_| TEMPLATE.to_owned());
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| SetupError::WriteConfig(format!("parse existing config: {e}")))?;

    ensure_lark_section(&mut doc);
    doc["lark"]["app_id"] = toml_edit::value(app_id);
    doc["lark"]["app_secret"] = toml_edit::value(app_secret);

    std::fs::write(path, doc.to_string())
        .map_err(|e| SetupError::WriteConfig(format!("write file: {e}")))?;
    Ok(())
}

/// Ensure a `[lark]` section exists at the top level of `doc`.
fn ensure_lark_section(doc: &mut toml_edit::DocumentMut) {
    if !doc.contains_key("lark") {
        doc["lark"] = toml_edit::table();
    }
    if !doc["lark"].is_table() {
        let mut t = toml_edit::Table::new();
        t.fmt();
        doc["lark"] = toml_edit::Item::Table(t);
    }
}

/// Entry point for `deapbox setup` — parse args + dispatch to bind/new.
pub async fn run(args: Vec<String>) -> Result<(), SetupError> {
    let cmd = parse_args(args)?;
    match cmd {
        SetupCommand::Bind(b) => bind::run_bind(b).await,
        SetupCommand::New(n) => new::run_new(n).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ============ write_back_config — fresh file ============

    #[test]
    fn write_back_config_creates_from_template_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");

        write_back_config(&path, "cli_test", "sec_test").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("app_id = \"cli_test\""));
        assert!(contents.contains("app_secret = \"sec_test\""));
        assert!(contents.contains("[lark]"));
        // template should also have brought [[agents]] example comments
        assert!(contents.contains("echo"));
    }

    // ============ write_back_config — existing file merge ============

    #[test]
    fn write_back_config_merges_into_existing_preserving_other_sections() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[lark]
app_id = "old_id"
app_secret = "old_secret"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_x"
agent_id = "echo-a"
"#
        )
        .unwrap();

        write_back_config(f.path(), "new_id", "new_secret").unwrap();

        let contents = std::fs::read_to_string(f.path()).unwrap();
        assert!(contents.contains("app_id = \"new_id\""));
        assert!(contents.contains("app_secret = \"new_secret\""));
        // preserved sections
        assert!(contents.contains("[[agents]]"));
        assert!(contents.contains("id = \"echo-a\""));
        assert!(contents.contains("[[sessions]]"));
        assert!(contents.contains("oc_x"));
        // old values gone
        assert!(!contents.contains("old_id"));
        assert!(!contents.contains("old_secret"));
    }

    // ============ write_back_config — only [lark] section in existing ============

    #[test]
    fn write_back_config_adds_lark_section_if_missing() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[[agents]]
id = "echo-a"
kind = "echo"
command = ""
"#
        )
        .unwrap();

        write_back_config(f.path(), "cli_x", "sec_y").unwrap();

        let contents = std::fs::read_to_string(f.path()).unwrap();
        assert!(contents.contains("[lark]"));
        assert!(contents.contains("app_id = \"cli_x\""));
        assert!(contents.contains("app_secret = \"sec_y\""));
        assert!(contents.contains("[[agents]]"));
    }

    // ============ error propagation ============

    #[test]
    fn parse_error_converts_to_setup_error_args_variant() {
        let err: SetupError = ParseError::UnknownArg("foo".to_owned()).into();
        match err {
            SetupError::Args(s) => assert!(s.contains("foo")),
            other => panic!("expected Args variant, got {:?}", other),
        }
    }
}
