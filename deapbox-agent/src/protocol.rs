//! stdio spawn 共享工具 — 供 per-kind `AgentSession`（TES-81 claude / kimi 子任务）复用。
//!
//! 不再持有旧 `StdioAgentProcess` / `AgentProcess` trait 实现（随 TES-86 删除）。
//! per-kind session 自己 own `Child` + 跨 turn 读循环 + `broadcast` channel，
//! 这里只提供 `Command` 装配与 `ChildStdin`/`ChildStdout` 拆分。

use std::process::Stdio;

use tokio::io::BufReader;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use deapbox_core::types::{AgentConfig, CoreError, WorkspacePath};

/// spawn 后的 stdio 句柄。`child` 由调用方 own（`kill_on_drop` 兜底）。
///
/// `stderr` 为 `None` 表示用 [`spawn_stdio`]（stderr → `/dev/null`）；
/// `Some` 表示用 [`spawn_stdio_with_stderr`]（stderr piped，供 per-kind session
/// 扫描 resume key 等非结构化输出）。stdout 永远 piped（stream-json 走它）。
pub struct StdioHandles {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub stderr: Option<BufReader<ChildStderr>>,
}

/// 按 `config` 装配 `Command` 并 spawn，拆出 stdin/stdout，stderr 丢弃。
///
/// per-kind driver 负责在调用前把 `--output-format stream-json` 等 flag 塞进
/// `config.args`，并按 `resume` 决定是否追加 `--resume <id>`。stderr 显式
/// `null`：stream-json 模式下 agent 把结构化输出走 stdout，stderr 仅作调试——
/// claude-code（长驻）不需要 stderr，用本函数。
///
/// 需要 stderr 内容（如 kimi 从 stderr 抓 resume key）时用 [`spawn_stdio_with_stderr`]。
pub fn spawn_stdio(
    config: &AgentConfig,
    workspace: &WorkspacePath,
) -> Result<StdioHandles, CoreError> {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .current_dir(&workspace.0)
        .envs(&config.env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| CoreError::AgentProcess(format!("spawn failed: {}", e)))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::AgentProcess("stdin not captured".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::AgentProcess("stdout not captured".into()))?;

    Ok(StdioHandles {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        stderr: None,
    })
}

/// 同 [`spawn_stdio`]，但 stderr 也 piped 并拆出供读取。
///
/// per-turn session（kimi）需要扫 stderr 抓 resume key 时用本函数；调用方负责
/// 消费返回的 `stderr` reader，否则 OS pipe 缓冲会写满阻塞 agent 进程。
pub fn spawn_stdio_with_stderr(
    config: &AgentConfig,
    workspace: &WorkspacePath,
) -> Result<StdioHandles, CoreError> {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .current_dir(&workspace.0)
        .envs(&config.env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| CoreError::AgentProcess(format!("spawn failed: {}", e)))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::AgentProcess("stdin not captured".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::AgentProcess("stdout not captured".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::AgentProcess("stderr not captured".into()))?;

    Ok(StdioHandles {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        stderr: Some(BufReader::new(stderr)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_core::types::{AgentId, AgentKind};
    use std::collections::HashMap;
    use tokio::io::AsyncBufReadExt;

    #[tokio::test]
    async fn spawn_stdio_captures_stdout() {
        let config = AgentConfig {
            id: AgentId("echo-test".into()),
            kind: AgentKind::Opencode,
            command: "echo".into(),
            args: vec!["hello".into()],
            env_vars: HashMap::new(),
        };
        let ws = WorkspacePath(std::env::temp_dir());

        let mut handles = spawn_stdio(&config, &ws).expect("spawn");
        let mut line = String::new();
        handles
            .stdout
            .read_line(&mut line)
            .await
            .expect("read line");
        assert_eq!(line.trim(), "hello");
        let _ = handles.child.wait().await;
    }

    #[tokio::test]
    async fn spawn_stdio_with_stderr_captures_stderr() {
        // sh -c 'echo to-stderr 1>&2; echo to-stdout'
        let config = AgentConfig {
            id: AgentId("stderr-test".into()),
            kind: AgentKind::Opencode,
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo to-stderr 1>&2; echo to-stdout".into(),
                "stderr-test".into(),
            ],
            env_vars: HashMap::new(),
        };
        let ws = WorkspacePath(std::env::temp_dir());

        let mut handles = spawn_stdio_with_stderr(&config, &ws).expect("spawn");
        assert!(handles.stderr.is_some(), "stderr must be piped");

        let mut stderr = handles.stderr.take().expect("stderr reader");
        let mut err_line = String::new();
        stderr.read_line(&mut err_line).await.expect("read stderr");
        assert_eq!(err_line.trim(), "to-stderr");

        let mut out_line = String::new();
        handles
            .stdout
            .read_line(&mut out_line)
            .await
            .expect("read stdout");
        assert_eq!(out_line.trim(), "to-stdout");
        let _ = handles.child.wait().await;
    }
}
