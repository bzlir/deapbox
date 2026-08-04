//! Feature 1: Stdio 进程通信
//!
//! 通过子进程 stdin/stdout 与 coding agent 通信。
//! 提供 AgentProcess trait 的 stdio 实现。

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
}
