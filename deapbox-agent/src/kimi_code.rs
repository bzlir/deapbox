//! Kimi (kimi-code) `stream-json` driver and process-per-turn session.
//!
//! 与 [`ClaudeCodeSession`](crate::claude_code) 共享 [`AgentSession`] trait，
//! 生命周期差异藏在实现里：
//!
//! - **claude-code**：长驻进程，stdin 持续喂 stream-json user 帧，跨 turn 复用。
//! - **kimi-code**：**每 `send` spawn 一个新进程**，`--prompt <text>` 传整条消息，
//!   进程跑完退出 → [`AgentEvent::Exited`]（正常退出 = 本轮完成，turn-runner 据此
//!   收尾）。跨 turn 复用的是 `broadcast` channel + resume key 链，不是进程。
//!
//! turn-end 仍由 agent 自己说（`result` 事件 → [`AgentEvent::TurnEnd`]）；进程退出
//! 再发 [`AgentEvent::Exited`]。idle-timeout 仅作 dead-agent 安全网（卡死时 `Failed`）。
//!
//! `--print` 支持按 cc-connect issue #1456 的方向探针：旧版 kimi 需它进非交互 print
//! 模式，新版弃用。探一次缓存结果；探不到则按前向兼容默认不加（新版语义）。

use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex,
};
use std::time::Duration;

use async_trait::async_trait;
use deapbox_core::{
    traits::{AgentDriver, AgentEventReceiver, AgentSession},
    types::{AgentConfig, AgentEvent, CoreError, NormalizedEvent, WorkspacePath},
};
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, ChildStdout, Command},
    sync::{broadcast, Mutex},
};

use crate::{
    adapter::{dispatch_ndjson_line, shared_agent_event, StreamJsonEvent},
    protocol::spawn_stdio_with_stderr,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Dead-agent 安全网：agent 连续这么久没 stdout 输出就视为卡死，kill + `Failed`。
/// 不当 turn 边界探测器（边界由 `result` 事件说，见 working.md lesson #2）。
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// `--print` 支持探针的 `--help` 超时；超时按"不支持"处理（前向兼容新版 kimi）。
const PRINT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

// ============ KimiDriver（工厂） ============

/// Kimi 驱动工厂——装配 per-turn 参数（`--output-format stream-json` / `--print`
/// / `--resume` / `--prompt`），产出 [`KimiSession`]。不在此 spawn：per-turn 模型下
/// 进程在 [`AgentSession::send`] 里起。
#[derive(Debug, Clone)]
pub struct KimiDriver {
    config: AgentConfig,
}

impl KimiDriver {
    pub fn new(config: AgentConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl AgentDriver for KimiDriver {
    async fn start_session(
        &self,
        resume: Option<&str>,
        workspace: &WorkspacePath,
    ) -> Result<Box<dyn AgentSession>, CoreError> {
        let initial_resume = resume.filter(|key| !key.is_empty()).map(str::to_owned);
        Ok(Box::new(KimiSession::new(
            self.config.clone(),
            workspace.clone(),
            initial_resume,
        )))
    }
}

// ============ KimiSession（进程-per-turn） ============

/// 当前 turn 的进程 pid（`None` = turn 空闲）。由 [`KimiSession::send`] 占、
/// [`run_turn`] 退出时清。供 [`KimiSession::interrupt`] / [`KimiSession::close`] 信号。
type CurrentTurn = Arc<Mutex<Option<u32>>>;

pub struct KimiSession {
    config: AgentConfig,
    workspace: WorkspacePath,
    tx: broadcast::Sender<AgentEvent>,
    /// 当前 turn 的 pid；`None` = 无 turn 在跑（kimi 进程-per-turn 的"空闲"态）。
    current: CurrentTurn,
    /// 最近一次 `result` 事件 / stderr 抓到的 resume key。下一次 `send` 用它拼
    /// `--resume <key>`；`None` = 新会话首 turn。
    resume_key: Arc<StdMutex<Option<String>>>,
    /// `Some(true)` = `--print` 支持（旧 kimi），`Some(false)` = 不支持（新 kimi），
    /// `None` = 尚未探针。探一次后缓存。
    print_supported: Arc<StdMutex<Option<bool>>>,
    /// 仅在 [`KimiSession::close`] 时翻 false。注意：kimi 的 `alive` 语义是
    /// "会话未关闭"，**不是**"进程在跑"——per-turn 模型下 turn 间进程本就为空，
    /// 把 alive 绑进程会让 host 误判会话 dead。turn 级卡死走 idle-timeout + `Failed`。
    alive: Arc<AtomicBool>,
    idle_timeout: Duration,
}

impl KimiSession {
    fn new(config: AgentConfig, workspace: WorkspacePath, initial_resume: Option<String>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            config,
            workspace,
            tx,
            current: Arc::new(Mutex::new(None)),
            resume_key: Arc::new(StdMutex::new(initial_resume)),
            print_supported: Arc::new(StdMutex::new(None)),
            alive: Arc::new(AtomicBool::new(true)),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }

    /// 探针 `--print` 支持。cache 命中走快路径（StdMutex guard 不跨 await）；
    /// miss 才 await `--help`，结果写回 cache。幂等——并发双 miss 探两次写同值。
    async fn resolve_print_support(&self) -> bool {
        {
            let cache = self.print_supported.lock().expect("print cache poisoned");
            if let Some(v) = *cache {
                return v;
            }
        }
        let v = probe_print_support(&self.config).await;
        *self.print_supported.lock().expect("print cache poisoned") = Some(v);
        v
    }

    /// 拼单 turn 的 argv：base + `--output-format stream-json` + (if probed)
    /// `--print` + (if resume) `--resume <key>` + `--prompt <text>`。
    fn build_turn_args(&self, text: &str, print_supported: bool) -> Vec<String> {
        let mut args = self.config.args.clone();
        args.push("--output-format".into());
        args.push("stream-json".into());
        if print_supported {
            args.push("--print".into());
        }
        if let Some(key) = self.resume_key.lock().expect("resume key poisoned").clone() {
            args.push("--resume".into());
            args.push(key);
        }
        args.push("--prompt".into());
        args.push(text.to_owned());
        args
    }

    #[cfg(test)]
    fn test_session(
        config: AgentConfig,
        workspace: WorkspacePath,
        initial_resume: Option<String>,
        idle_timeout: Duration,
    ) -> Self {
        let mut s = Self::new(config, workspace, initial_resume);
        s.idle_timeout = idle_timeout;
        s
    }
}

#[async_trait]
impl AgentSession for KimiSession {
    async fn send(&self, text: &str) -> Result<(), CoreError> {
        // 占 turn 槽 + 探针 + spawn 都在 turn mutex 内：避免双 send 漏判占用、
        // 避免探针 cache 的双-miss 竞态落到 spawn 上。spawn 本身不阻塞（无 wait）。
        let (child, stdin, stdout, stderr) = {
            let mut guard = self.current.lock().await;
            if guard.is_some() {
                return Err(CoreError::AgentProcess(
                    "a turn is already in progress".into(),
                ));
            }
            let print_supported = self.resolve_print_support().await;
            let args = self.build_turn_args(text, print_supported);
            let mut turn_config = self.config.clone();
            turn_config.args = args;
            let handles = spawn_stdio_with_stderr(&turn_config, &self.workspace)?;
            let stderr = handles
                .stderr
                .ok_or_else(|| CoreError::AgentProcess("stderr not piped for kimi turn".into()))?;
            let pid = handles
                .child
                .id()
                .ok_or_else(|| CoreError::AgentProcess("process has no pid".into()))?;
            *guard = Some(pid);
            (handles.child, handles.stdin, handles.stdout, stderr)
        }; // guard 释放，read loop 跑在后台 task

        // --prompt 模式不读 stdin；关 pipe 让 agent 拿 EOF（若它读），不传进 loop。
        drop(stdin);
        tokio::spawn(run_turn(
            child,
            stdout,
            stderr,
            self.tx.clone(),
            Arc::clone(&self.resume_key),
            Arc::clone(&self.current),
            self.idle_timeout,
        ));
        Ok(())
    }

    fn subscribe(&self) -> AgentEventReceiver {
        self.tx.subscribe()
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        // 无 turn 在跑 → 无可中断，no-op（非 error：用户连点中断不该炸）。
        let pid = {
            let guard = self.current.lock().await;
            match *guard {
                Some(pid) => pid,
                None => return Ok(()),
            }
        };
        kill(Pid::from_raw(pid as i32), Signal::SIGINT)
            .map_err(|err| CoreError::AgentProcess(format!("SIGINT failed: {}", err)))
    }

    fn current_resume_key(&self) -> Option<String> {
        self.resume_key.lock().expect("resume key poisoned").clone()
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    async fn close(self: Box<Self>) -> Result<(), CoreError> {
        self.alive.store(false, Ordering::Release);
        // SIGTERM 当前 turn 进程（若有）。read loop 的 `child.wait()` 会 reap 并
        // 发 `Exited`；`kill_on_drop` 是兜底。
        let pid = {
            let guard = self.current.lock().await;
            *guard
        };
        if let Some(pid) = pid {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        Ok(())
    }
}

// ============ per-turn read loop ============

/// 一个 turn 的全部后台工作：读 stdout（stream-json → 事件）、读 stderr（抓 resume
/// key 兜底）、idle-timeout 监视、进程退出 → 清 turn 槽 + 发 `Exited`。owns `Child`。
async fn run_turn(
    mut child: Child,
    mut stdout: BufReader<ChildStdout>,
    stderr: BufReader<ChildStderr>,
    tx: broadcast::Sender<AgentEvent>,
    resume_key: Arc<StdMutex<Option<String>>>,
    current: CurrentTurn,
    idle_timeout: Duration,
) {
    // stderr 只抓 resume key（兜底 result 事件没带 session_id 的情况），跑到 EOF。
    let stderr_task = tokio::spawn(scan_stderr_for_resume(stderr, Arc::clone(&resume_key)));

    let mut line = String::new();
    loop {
        line.clear();
        match tokio::time::timeout(idle_timeout, stdout.read_line(&mut line)).await {
            Ok(Ok(0)) => break, // stdout EOF → 本轮输出完
            Ok(Ok(_)) => match dispatch_ndjson_line(&line) {
                Ok(Some(event)) => emit_kimi_event(event, &tx, &resume_key).await,
                Ok(None) => {}
                Err(err) => {
                    let _ = tx.send(AgentEvent::Failed(CoreError::AgentProcess(format!(
                        "stream-json parse failed: {}",
                        err
                    ))));
                }
            },
            Ok(Err(err)) => {
                let _ = tx.send(AgentEvent::Failed(io_error(err)));
                break;
            }
            Err(_) => {
                // idle-timeout：dead-agent 安全网。kill + Failed，让 turn-runner 收尾。
                let _ = child.start_kill();
                let _ = tx.send(AgentEvent::Failed(CoreError::AgentProcess(format!(
                    "agent idle for {}s, killed",
                    idle_timeout.as_secs()
                ))));
                break;
            }
        }
    }

    let exit_code = match child.wait().await {
        Ok(status) => status.code(),
        Err(err) => {
            let _ = tx.send(AgentEvent::Failed(CoreError::AgentProcess(format!(
                "wait failed: {}",
                err
            ))));
            None
        }
    };

    // 进程已退，stderr reader 必然 EOF；abort 保险（不阻塞）。
    stderr_task.abort();

    // 清 turn 槽，让下一次 send 能占。先清后发 Exited，避免 Exited 到达前 host
    // 重发 send 撞上未清的槽（host 正常流程是等 Exited 后才 send，但防御一手）。
    {
        let mut guard = current.lock().await;
        *guard = None;
    }
    let _ = tx.send(AgentEvent::Exited(exit_code));
}

async fn emit_kimi_event(
    event: StreamJsonEvent,
    tx: &broadcast::Sender<AgentEvent>,
    resume_key: &Arc<StdMutex<Option<String>>>,
) {
    if let StreamJsonEvent::Assistant(raw) = &event {
        for normalized in kimi_assistant_events(raw) {
            let _ = tx.send(AgentEvent::Normalized(normalized));
        }
    }
    if let Some(AgentEvent::TurnEnd { resume_key: key }) = shared_agent_event(event) {
        // result 事件权威：覆盖 stderr 早先抓到的值（一般同值，无妨）。
        *resume_key.lock().expect("resume key poisoned") = key.clone();
        let _ = tx.send(AgentEvent::TurnEnd { resume_key: key });
    }
}

// ============ assistant message → NormalizedEvent ============

/// kimi stream-json assistant 事件的语义映射。结构与 claude 的 Anthropic 风格
/// content blocks 一致（`text` / `thinking` / `tool_use`）；若 kimi 格式将来分叉，
/// 改本函数即可。镜像 [`claude_code`] 的同名逻辑——刻意不抽进 adapter，避免
/// 触 TES-81（claude_code 非本任务范围）；后续可统一进 adapter.rs。
fn kimi_assistant_events(raw: &Value) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    collect_assistant_content(raw.get("message").unwrap_or(raw), &mut events);
    events
}

fn collect_assistant_content(value: &Value, events: &mut Vec<NormalizedEvent>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_assistant_content(item, events);
            }
        }
        Value::Object(object) => {
            match object.get("type").and_then(Value::as_str) {
                Some("text") => push_str_field(object, &["text"], NormalizedEvent::Text, events),
                Some("thinking") => push_str_field(
                    object,
                    &["thinking", "text"],
                    NormalizedEvent::Thinking,
                    events,
                ),
                Some("tool_use") => {
                    let name = object.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = object
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_default();
                    events.push(NormalizedEvent::ToolCall(format_tool_call(name, &input)));
                }
                _ => {}
            }
            if let Some(content) = object.get("content") {
                collect_assistant_content(content, events);
            }
        }
        _ => {}
    }
}

fn push_str_field(
    object: &serde_json::Map<String, Value>,
    fields: &[&str],
    build: fn(String) -> NormalizedEvent,
    events: &mut Vec<NormalizedEvent>,
) {
    if let Some(text) = fields
        .iter()
        .find_map(|field| object.get(*field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
    {
        events.push(build(text));
    }
}

fn format_tool_call(name: &str, input: &str) -> String {
    if input.is_empty() {
        name.to_owned()
    } else {
        format!("{} {}", name, input)
    }
}

// ============ stderr resume-key 兜底抓取 ============

async fn scan_stderr_for_resume(
    mut stderr: BufReader<ChildStderr>,
    resume_key: Arc<StdMutex<Option<String>>>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match stderr.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(key) = extract_resume_key_from_text(&line) {
                    *resume_key.lock().expect("resume key poisoned") = Some(key);
                }
            }
        }
    }
}

/// 从非结构化文本（stderr 行）抓 session/resume id。容忍 `session_id` /
/// `sessionId` / `resume_key` 等大小写与分隔变体，带可选引号。
static RESUME_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:session[_\s-]?id|resume[_\s-]?key)\s*[:=]\s*["']?([A-Za-z0-9][A-Za-z0-9_-]+)["']?"#)
        .expect("valid resume key regex")
});

fn extract_resume_key_from_text(text: &str) -> Option<String> {
    RESUME_KEY_RE.captures(text).map(|caps| caps[1].to_owned())
}

// ============ --print 支持探针 ============

/// 跑 `<cmd> <base args> --help`，看输出里是否出现 `--print`。spawn 失败 / 超时
/// → 按前向兼容返回 `false`（新版 kimi 弃用 `--print`，不加是对的）。
async fn probe_print_support(config: &AgentConfig) -> bool {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .arg("--help")
        .envs(&config.env_vars)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    match tokio::time::timeout(PRINT_PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => {
            String::from_utf8_lossy(&output.stdout).contains("--print")
                || String::from_utf8_lossy(&output.stderr).contains("--print")
        }
        Ok(Err(_)) | Err(_) => false,
    }
}

fn io_error(err: std::io::Error) -> CoreError {
    CoreError::Io(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_core::types::{AgentId, AgentKind};
    use std::collections::HashMap;
    use tokio::time::timeout;

    fn kimi_config(script: &str) -> AgentConfig {
        AgentConfig {
            id: AgentId("fake-kimi".into()),
            kind: AgentKind::KimiCode,
            command: "sh".into(),
            args: vec!["-c".into(), script.into(), "fake-kimi".into()],
            env_vars: HashMap::new(),
        }
    }

    fn temp_workspace() -> WorkspacePath {
        WorkspacePath(std::env::temp_dir())
    }

    /// `--print` 在 help 里出现 → 探针 true。turn 跑通且收到 result + Exited。
    fn script_print_supported() -> &'static str {
        r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print --prompt <text> --output-format stream-json'; exit 0;;
esac
printf '%s\n' '{"type":"result","session_id":"kimi-resume-1"}'"#
    }

    /// `--print` 不在 help 里 → 探针 false。若 argv 仍含 `--print`，脚本以 2 退出
    /// （证明 build_turn_args 没误加）。
    fn script_print_not_supported() -> &'static str {
        r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi (no --print flag)'; exit 0;;
  --print) printf '%s\n' 'ERROR: --print should not be passed' >&2; exit 2;;
esac
printf '%s\n' '{"type":"result","session_id":"kimi-no-print"}'"#
    }

    async fn recv(rx: &mut AgentEventReceiver) -> AgentEvent {
        timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event timeout")
            .expect("event")
    }

    #[tokio::test]
    async fn per_turn_spawn_resume_injection_and_exit() {
        // 初始 resume "seed" → argv 含 `--resume seed`。脚本回 result 覆盖为
        // "kimi-resume-1"，再退出 0。期望：TurnEnd + Exited(0)，且 resume_key 更新。
        let session = KimiSession::test_session(
            kimi_config(script_print_supported()),
            temp_workspace(),
            Some("seed".into()),
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();

        session.send("hello").await.expect("send");

        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::TurnEnd { resume_key: Some(ref k) } if k == "kimi-resume-1"
        ));
        assert!(matches!(recv(&mut rx).await, AgentEvent::Exited(Some(0))));
        assert_eq!(session.current_resume_key(), Some("kimi-resume-1".into()));
        assert!(session.alive());
        // close() 消费 self（Box<Self>）；alive=false 在内部置位，无法事后查。
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn print_probe_supported_path_adds_print_flag() {
        // 帮助含 `--print` → 探针 true → argv 加 `--print`。脚本不报错（无 `--print`
        // 分支命中），回 result。验证支持路径不炸。
        let session = KimiSession::test_session(
            kimi_config(script_print_supported()),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();
        session.send("go").await.expect("send");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::TurnEnd { resume_key: Some(ref k) } if k == "kimi-resume-1"
        ));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn print_probe_unsupported_path_omits_print_flag() {
        // 帮助无 `--print` → 探针 false → argv 不加 `--print`。脚本若见 `--print`
        // 则 exit 2（无 result）。期望收到 result（证明没误加 `--print`）。
        let session = KimiSession::test_session(
            kimi_config(script_print_not_supported()),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();
        session.send("go").await.expect("send");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::TurnEnd { resume_key: Some(ref k) } if k == "kimi-no-print"
        ));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn resume_key_captured_from_stderr_when_no_result_event() {
        // stdout 无 result，仅退出。stderr 抓到 session_id → resume_key 兜底。
        let script = r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print'; exit 0;;
esac
echo 'session_id: kimi-stderr-key' >&2
# 不发任何 stream-json，直接退出
exit 0"#;
        let session = KimiSession::test_session(
            kimi_config(script),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();
        session.send("go").await.expect("send");

        assert!(matches!(recv(&mut rx).await, AgentEvent::Exited(Some(0))));
        assert_eq!(session.current_resume_key(), Some("kimi-stderr-key".into()));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn cross_turn_channel_continuity_and_resume_chain() {
        // 两 turn 各起一进程，同一 subscribe() 收全。turn1 无 resume → result
        // "turn1-key"；turn2 用 `--resume turn1-key` → result "turn2-turn1-key"。
        let script = r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print'; exit 0;;
esac
# 找 --resume 的值
resume=""
prev=""
for a in "$@"; do
  case "$prev" in --resume) resume="$a";; esac
  prev="$a"
done
if [ -n "$resume" ]; then
  printf '%s\n' "{\"type\":\"result\",\"session_id\":\"turn2-${resume}\"}"
else
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}'
  printf '%s\n' '{"type":"result","session_id":"turn1-key"}'
fi"#;
        let session = KimiSession::test_session(
            kimi_config(script),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();

        session.send("first").await.expect("send 1");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::Normalized(NormalizedEvent::Text(ref t)) if t == "hi"
        ));
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::TurnEnd { resume_key: Some(ref k) } if k == "turn1-key"
        ));
        assert!(matches!(recv(&mut rx).await, AgentEvent::Exited(Some(0))));
        assert_eq!(session.current_resume_key(), Some("turn1-key".into()));

        // 等 turn 槽被清（Exited 已发，槽已清）。send 2 应能用 turn1-key 拼 resume。
        session.send("second").await.expect("send 2");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::TurnEnd { resume_key: Some(ref k) } if k == "turn2-turn1-key"
        ));
        assert!(matches!(recv(&mut rx).await, AgentEvent::Exited(Some(0))));
        assert_eq!(session.current_resume_key(), Some("turn2-turn1-key".into()));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn send_while_turn_in_progress_errors() {
        // 第一 turn 卡住不退（trap 忽略 SIGINT 之外的信号，循环 sleep）；
        // 第二次 send 撞上未清的 turn 槽 → Err。第一 turn 用 close 收尾。
        let script = r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print'; exit 0;;
esac
while true; do sleep 0.2; done"#;
        let session = KimiSession::test_session(
            kimi_config(script),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let _rx = session.subscribe();
        session.send("first").await.expect("send 1");
        // 立即再发：turn 槽还被占。
        let err = session.send("second").await;
        assert!(err.is_err(), "second send during active turn must error");
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn interrupt_kills_current_turn() {
        let script = r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print'; exit 0;;
esac
trap 'exit 130' INT
while true; do sleep 1; done"#;
        let session = KimiSession::test_session(
            kimi_config(script),
            temp_workspace(),
            None,
            DEFAULT_IDLE_TIMEOUT,
        );
        let mut rx = session.subscribe();
        session.send("go").await.expect("send");
        session.interrupt().await.expect("interrupt");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::Exited(Some(130)) | AgentEvent::Exited(None)
        ));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn idle_timeout_kills_stuck_agent() {
        // 脚本打印一个 stream-json 后长时间沉默 → idle-timeout 触发 Failed + Exited。
        let script = r#"case "$1" in
  --help) printf '%s\n' 'usage: kimi --print'; exit 0;;
esac
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}'
sleep 30"#;
        let session = KimiSession::test_session(
            kimi_config(script),
            temp_workspace(),
            None,
            Duration::from_millis(150),
        );
        let mut rx = session.subscribe();
        session.send("go").await.expect("send");
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::Normalized(NormalizedEvent::Text(ref t)) if t == "partial"
        ));
        assert!(matches!(
            recv(&mut rx).await,
            AgentEvent::Failed(CoreError::AgentProcess(_))
        ));
        assert!(matches!(recv(&mut rx).await, AgentEvent::Exited(_)));
        Box::new(session).close().await.expect("close");
    }

    #[test]
    fn resume_key_regex_matches_common_variants() {
        assert_eq!(
            extract_resume_key_from_text("session_id: abc123"),
            Some("abc123".into())
        );
        assert_eq!(
            extract_resume_key_from_text("sessionId=kimi-1"),
            Some("kimi-1".into())
        );
        assert_eq!(
            extract_resume_key_from_text("RESUME KEY: \"turn-9\""),
            Some("turn-9".into())
        );
        assert_eq!(extract_resume_key_from_text("no key here"), None);
    }

    #[test]
    fn build_turn_args_assembles_expected_flags() {
        let session = KimiSession::new(
            kimi_config("true"),
            temp_workspace(),
            Some("seed-key".into()),
        );
        let args = session.build_turn_args("hello world", true);
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "stream-json"));
        assert!(args.contains(&"--print".to_string()));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--resume" && w[1] == "seed-key"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--prompt" && w[1] == "hello world"));
    }

    #[test]
    fn build_turn_args_omits_resume_when_none() {
        let session = KimiSession::new(kimi_config("true"), temp_workspace(), None);
        let args = session.build_turn_args("hi", false);
        assert!(!args.iter().any(|a| a == "--resume"));
        assert!(!args.iter().any(|a| a == "--print"));
    }
}
