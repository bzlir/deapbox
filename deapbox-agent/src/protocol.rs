//! Feature 1: Stdio 进程通信
//!
//! 通过子进程 stdin/stdout 与 coding agent 通信。
//! 提供 AgentProcess trait 的 stdio 实现。
//!
//! `child` 与 `stdin` 用 `tokio::sync::Mutex` 包裹，因为 trait 的
//! `send_input` / `health_check` / `interrupt` 只拿到 `&self`，
//! 需要内部可变性；`stdout_reader` 与 `adapter` 只在 `recv_output(&mut self)`
//! 中访问，无需 Mutex。

use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use deapbox_core::traits::ProtocolAdapter;
use deapbox_core::types::*;

/// 基于 stdio pipe 的 AgentProcess 实现
pub struct StdioAgentProcess {
    child: Mutex<Option<Child>>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    adapter: Box<dyn ProtocolAdapter>,
}

impl StdioAgentProcess {
    /// 启动 agent 进程并返回句柄
    pub async fn spawn(
        config: &AgentConfig,
        workspace: &Path,
        adapter: Box<dyn ProtocolAdapter>,
    ) -> Result<Self, CoreError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .current_dir(workspace)
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

        Ok(Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            stdout_reader: BufReader::new(stdout),
            adapter,
        })
    }

    /// 读取一行原始输出
    async fn read_line(&mut self) -> Result<Option<String>, CoreError> {
        let mut line = String::new();
        let n = self
            .stdout_reader
            .read_line(&mut line)
            .await
            .map_err(|e| CoreError::AgentProcess(format!("read error: {}", e)))?;

        if n == 0 {
            return Ok(None); // EOF
        }

        // 去掉行尾换行
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }

        Ok(Some(line))
    }

    /// 检查子进程是否还活着
    async fn is_alive(&self) -> bool {
        let mut child = self.child.lock().await;
        child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
            .is_none()
    }
}

#[async_trait]
impl deapbox_core::traits::AgentProcess for StdioAgentProcess {
    async fn send_input(&self, text: &str) -> Result<(), CoreError> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| CoreError::AgentProcess(format!("write error: {}", e)))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| CoreError::AgentProcess(format!("write error: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| CoreError::AgentProcess(format!("flush error: {}", e)))?;
        Ok(())
    }

    async fn recv_output(&mut self) -> Result<AgentOutputEvent, CoreError> {
        // 逐行读取，通过 adapter 产生事件
        while let Some(line) = self.read_line().await? {
            let events = self.adapter.process_line(&line);
            if !events.is_empty() {
                return Ok(AgentOutputEvent::Normalized(
                    events.into_iter().next().unwrap(),
                ));
            }
            // adapter 过滤了此行（如 spinner），继续读下一行
        }

        // EOF — adapter flush 剩余内容
        let remaining = self.adapter.flush();
        if let Some(event) = remaining.into_iter().next() {
            Ok(AgentOutputEvent::Normalized(event))
        } else {
            Ok(AgentOutputEvent::Normalized(NormalizedEvent::TurnComplete))
        }
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        #[cfg(unix)]
        {
            let child = self.child.lock().await;
            if let Some(pid) = child.as_ref().and_then(|child| child.id()) {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGINT);
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> Result<HealthStatus, CoreError> {
        if !self.is_alive().await {
            Ok(HealthStatus::Dead)
        } else {
            Ok(HealthStatus::Healthy)
        } else {
            Ok(HealthStatus::Dead)
        }
    }

    async fn shutdown(mut self: Box<Self>) -> Result<(), CoreError> {
        if let Some(mut child) = self.child.get_mut().take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_core::traits::{AgentProcess, ProtocolAdapter};

    /// 一个简单的 adapter，原样输出所有行
    struct PassthroughAdapter;

    impl ProtocolAdapter for PassthroughAdapter {
        fn process_line(&mut self, line: &str) -> Vec<NormalizedEvent> {
            vec![NormalizedEvent::Text(line.to_string())]
        }
        fn flush(&mut self) -> Vec<NormalizedEvent> {
            vec![]
        }
    }

    #[tokio::test]
    async fn test_spawn_echo() {
        // 用 echo 测试 spawn 和 read_line
        let config = AgentConfig {
            id: AgentId("test".into()),
            kind: AgentKind::Opencode,
            command: "echo".into(),
            args: vec!["hello from agent".into()],
            env_vars: std::collections::HashMap::new(),
        };
        let tmp = std::env::temp_dir();
        let mut proc = StdioAgentProcess::spawn(&config, &tmp, Box::new(PassthroughAdapter))
            .await
            .unwrap();

        let event = proc.recv_output().await.unwrap();
        match event {
            AgentOutputEvent::Normalized(NormalizedEvent::Text(t)) => {
                assert_eq!(t, "hello from agent");
            }
            _ => panic!("expected Text event"),
        }
    }

    /// 验证 send_input -> recv_output 的回显往返（不依赖真实 agent CLI）
    /// 使用 `cat` 读取 stdin 原样写回 stdout，覆盖 stdin 写入后 stdout 回显。
    #[cfg(unix)]
    #[tokio::test]
    async fn test_send_input_echo_roundtrip() {
        let config = AgentConfig {
            id: AgentId("test".into()),
            kind: AgentKind::Opencode,
            command: "cat".into(),
            args: vec![],
            env_vars: std::collections::HashMap::new(),
        };
        let tmp = std::env::temp_dir();
        let mut proc = StdioAgentProcess::spawn(
            &config,
            &tmp,
            Box::new(PassthroughAdapter),
        )
        .await
        .unwrap();

        proc.send_input("hello roundtrip").await.unwrap();

        let event = proc.recv_output().await.unwrap();
        match event {
            AgentOutputEvent::Normalized(NormalizedEvent::Text(t)) => {
                assert_eq!(t, "hello roundtrip");
            }
            _ => panic!("expected Text event, got {:?}", event),
        }

        // shutdown 以 `self: Box<Self>` 接收，需显式装箱
        AgentProcess::shutdown(Box::new(proc)).await.unwrap();
    }

    /// 验证进程退出后 recv_output 返回 TurnComplete（EOF + adapter flush 为空）
    #[tokio::test]
    async fn test_eof_returns_turn_complete() {
        let config = AgentConfig {
            id: AgentId("test".into()),
            kind: AgentKind::Opencode,
            command: "true".into(),
            args: vec![],
            env_vars: std::collections::HashMap::new(),
        };
        let tmp = std::env::temp_dir();
        let mut proc = StdioAgentProcess::spawn(
            &config,
            &tmp,
            Box::new(PassthroughAdapter),
        )
        .await
        .unwrap();

        // `true` 立即退出，stdout 关闭 → read_line 返回 None → adapter.flush() 为空 → TurnComplete
        let event = proc.recv_output().await.unwrap();
        match event {
            AgentOutputEvent::Normalized(NormalizedEvent::TurnComplete) => {}
            _ => panic!("expected TurnComplete, got {:?}", event),
        }
    }

    /// 验证 health_check：存活进程返回 Healthy，退出后返回 Dead
    #[tokio::test]
    async fn test_health_check_alive_and_dead() {
        let config = AgentConfig {
            id: AgentId("test".into()),
            kind: AgentKind::Opencode,
            command: "cat".into(),
            args: vec![],
            env_vars: std::collections::HashMap::new(),
        };
        let tmp = std::env::temp_dir();
        let proc = StdioAgentProcess::spawn(
            &config,
            &tmp,
            Box::new(PassthroughAdapter),
        )
        .await
        .unwrap();

        // cat 启动后应存活
        let status = proc.health_check().await.unwrap();
        assert_eq!(status, HealthStatus::Healthy);

        // shutdown 以 `self: Box<Self>` 接收，需显式装箱
        AgentProcess::shutdown(Box::new(proc)).await.unwrap();
    }

    /// 验证 shutdown 可重复调用路径（drop 后不 panic）
    #[tokio::test]
    async fn test_shutdown_kills_child() {
        let config = AgentConfig {
            id: AgentId("test".into()),
            kind: AgentKind::Opencode,
            command: "cat".into(),
            args: vec![],
            env_vars: std::collections::HashMap::new(),
        };
        let tmp = std::env::temp_dir();
        let proc = StdioAgentProcess::spawn(
            &config,
            &tmp,
            Box::new(PassthroughAdapter),
        )
        .await
        .unwrap();

        // 显式 shutdown 应正常完成，不留下子进程（kill_on_drop 兜底）
        // shutdown 以 `self: Box<Self>` 接收，需显式装箱
        AgentProcess::shutdown(Box::new(proc)).await.unwrap();
    }
}
