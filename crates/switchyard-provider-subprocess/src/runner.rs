//! Generic subprocess runner for CLI-based providers.
//!
//! Handles: spawn, stdin write, concurrent stdout/stderr drain, timeout/kill,
//! and cooperative cancellation via CancellationToken.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use switchyard_provider_api::ExecutionTelemetry;

use crate::resolve::{find_on_path, is_windows_batch_wrapper, resolve_npm_entry};

/// Configuration for a subprocess invocation.
pub struct SubprocessConfig<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub stdin_data: Option<&'a str>,
    pub timeout_secs: u64,
    pub cwd: Option<&'a std::path::Path>,
    pub pty_registry_key: Option<Uuid>,
    /// Whether this invocation prefers PTY transport when compatible.
    ///
    /// Structured/headless JSON modes should keep this disabled because many
    /// official CLIs switch into interactive TTY rendering under PTY and stop
    /// emitting machine-readable protocol lines.
    pub prefer_pty: bool,
    pub env: Option<&'a std::collections::HashMap<String, String>>,
}

/// Result of a completed subprocess execution.
#[derive(Debug)]
pub struct SubprocessOutput {
    pub stdout: String,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct SubprocessInvocationPlan {
    pub command: String,
    pub args: Vec<String>,
    pub execution: ExecutionTelemetry,
}

#[derive(Debug, Clone)]
pub struct StreamingOutputLine {
    pub text: String,
    pub transport: String,
}

type SharedMasterPty = Arc<Mutex<Box<dyn MasterPty + Send>>>;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Prevent helper CLIs spawned by the GUI process from flashing their own
/// console windows on Windows.
///
/// The packaged Tauri app is a `windows_subsystem = "windows"` binary, but
/// child console-subsystem executables such as `node.exe`, `git.exe`, or
/// `taskkill.exe` can still allocate a visible console unless we opt out on
/// every non-interactive spawn.
pub fn suppress_windows_console_for_tokio_command(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Std-process variant of [`suppress_windows_console_for_tokio_command`].
pub fn suppress_windows_console_for_std_command(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

fn log_debug_stdio(direction: &str, command: &str, line: &str) {
    if std::env::var("SWITCHYARD_DEBUG_STDIO").unwrap_or_default() == "1" {
        let path = if std::path::Path::new(".switchyard").is_dir() {
            std::path::PathBuf::from(".switchyard").join("debug_stdio.log")
        } else {
            std::path::PathBuf::from("debug_stdio.log")
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let log_line = format!(
                "[{}] [{}] {}: {}\n",
                timestamp,
                command,
                direction,
                line.trim_end_matches(['\r', '\n'])
            );
            let _ = file.write_all(log_line.as_bytes());
        }
    }
}

fn pty_registry() -> &'static Mutex<HashMap<Uuid, SharedMasterPty>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Uuid, SharedMasterPty>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_turn_pty(turn_id: Uuid, master: SharedMasterPty) {
    if let Ok(mut registry) = pty_registry().lock() {
        registry.insert(turn_id, master);
    }
}

fn unregister_turn_pty(turn_id: Uuid) {
    if let Ok(mut registry) = pty_registry().lock() {
        registry.remove(&turn_id);
    }
}

pub fn resize_registered_pty(turn_id: Uuid, rows: u16, cols: u16) -> Result<bool, String> {
    let master = {
        let registry = pty_registry()
            .lock()
            .map_err(|_| "pty registry poisoned".to_string())?;
        registry.get(&turn_id).cloned()
    };

    let Some(master) = master else {
        return Ok(false);
    };

    let size = PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    };
    let master = master
        .lock()
        .map_err(|_| "pty handle poisoned".to_string())?;
    master
        .resize(size)
        .map_err(|error| format!("pty resize: {error}"))?;
    Ok(true)
}

fn take_terminal_segments(pending: &mut Vec<u8>) -> Vec<String> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for index in 0..pending.len() {
        if matches!(pending[index], b'\n' | b'\r') {
            let segment = String::from_utf8_lossy(&pending[start..=index]).into_owned();
            segments.push(segment);
            start = index + 1;
        }
    }

    if start > 0 {
        pending.drain(0..start);
    }

    segments
}

fn flush_terminal_tail(pending: &mut Vec<u8>) -> Option<String> {
    if pending.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&std::mem::take(pending)).into_owned())
    }
}

/// Errors from subprocess execution.
#[derive(Debug, thiserror::Error)]
pub enum SubprocessError {
    #[error("command not found: {0}")]
    NotFound(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("stdin write failed: {0}")]
    StdinFailed(String),
    #[error("timed out after {0}s")]
    Timeout(u64),
    #[error("cancelled")]
    Cancelled,
    #[error("process failed: {0}")]
    ProcessFailed(String),
}

/// Run a subprocess, collect all output, return when done.
pub async fn run_subprocess(
    config: &SubprocessConfig<'_>,
) -> Result<SubprocessOutput, SubprocessError> {
    let (child, stderr_task) = spawn_and_setup(config).await?;
    collect_output(child, stderr_task, config.timeout_secs).await
}

/// Run a subprocess, streaming each stdout line through an mpsc channel.
///
/// Completes when the child process exits (not just stdout EOF).
/// If `cancel` is signalled, the child process is killed immediately.
pub async fn run_subprocess_streaming(
    config: &SubprocessConfig<'_>,
    line_tx: &tokio::sync::mpsc::Sender<StreamingOutputLine>,
    cancel: CancellationToken,
) -> Result<SubprocessOutput, SubprocessError> {
    run_subprocess_streaming_until(
        config,
        line_tx,
        cancel,
        None,
        tokio::time::Duration::from_millis(0),
    )
    .await
}

/// Run a subprocess, streaming stdout lines, but allow a provider protocol
/// signal to end the turn before the child naturally exits.
///
/// This is needed for CLIs such as Codex, which may emit a structured
/// `turn.completed` event well before the process exits. When
/// `logical_done` fires, this runner gives the child a short grace period to
/// exit naturally, then kills it and treats the subprocess as successful.
pub async fn run_subprocess_streaming_until(
    config: &SubprocessConfig<'_>,
    line_tx: &tokio::sync::mpsc::Sender<StreamingOutputLine>,
    cancel: CancellationToken,
    logical_done: Option<CancellationToken>,
    logical_done_grace: tokio::time::Duration,
) -> Result<SubprocessOutput, SubprocessError> {
    if should_use_pty(config)
        && let Some(output) = try_run_subprocess_streaming_until_pty(
            config,
            line_tx,
            cancel.clone(),
            logical_done.clone(),
            logical_done_grace,
        )
        .await?
    {
        return Ok(output);
    }

    run_subprocess_streaming_until_pipe(config, line_tx, cancel, logical_done, logical_done_grace)
        .await
}

fn should_use_pty(config: &SubprocessConfig<'_>) -> bool {
    config.prefer_pty && !looks_like_structured_headless_mode(config.args)
}

fn looks_like_structured_headless_mode(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--json" | "stream-json" | "--output-format" | "--verbose"
        )
    })
}

async fn run_subprocess_streaming_until_pipe(
    config: &SubprocessConfig<'_>,
    line_tx: &tokio::sync::mpsc::Sender<StreamingOutputLine>,
    cancel: CancellationToken,
    logical_done: Option<CancellationToken>,
    logical_done_grace: tokio::time::Duration,
) -> Result<SubprocessOutput, SubprocessError> {
    let (mut child, mut stderr_task) = spawn_and_setup(config).await?;

    let timeout = tokio::time::Duration::from_secs(config.timeout_secs);

    // Move stdout into a background reader task so we can wait on
    // child.wait() (process exit) as the primary completion signal.
    // This avoids hanging when a process outputs its response but
    // keeps stdout open during cleanup.
    let stdout_handle = child.stdout.take();
    let line_tx_clone = line_tx.clone();
    let cmd_name = config.command.to_string();
    let mut stdout_task = tokio::spawn(async move {
        let mut full_bytes = Vec::new();
        if let Some(stdout) = stdout_handle {
            let mut reader = stdout;
            let mut buf = [0u8; 4096];
            let mut pending = Vec::new();
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(read) => {
                        full_bytes.extend_from_slice(&buf[..read]);
                        pending.extend_from_slice(&buf[..read]);
                        for segment in take_terminal_segments(&mut pending) {
                            log_debug_stdio("STDOUT", &cmd_name, &segment);
                            line_tx_clone
                                .send(StreamingOutputLine {
                                    text: segment,
                                    transport: "pipe".to_string(),
                                })
                                .await
                                .ok();
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(segment) = flush_terminal_tail(&mut pending) {
                log_debug_stdio("STDOUT", &cmd_name, &segment);
                line_tx_clone
                    .send(StreamingOutputLine {
                        text: segment,
                        transport: "pipe".to_string(),
                    })
                    .await
                    .ok();
            }
        }
        String::from_utf8_lossy(&full_bytes).into_owned()
    });

    async fn terminate_child(child: &mut tokio::process::Child) {
        let pid = child.id();
        child.kill().await.ok();

        let waited = tokio::time::timeout(tokio::time::Duration::from_secs(2), child.wait()).await;
        if waited.is_ok() {
            return;
        }

        #[cfg(windows)]
        if let Some(pid) = pid {
            let mut taskkill = tokio::process::Command::new("taskkill");
            suppress_windows_console_for_tokio_command(&mut taskkill);
            let _ = taskkill
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            let _ = tokio::time::timeout(tokio::time::Duration::from_secs(2), child.wait()).await;
        }
    }

    async fn complete_after_logical_done(
        child: &mut tokio::process::Child,
        grace: tokio::time::Duration,
    ) -> Result<Option<i32>, SubprocessError> {
        if !grace.is_zero() {
            match tokio::time::timeout(grace, child.wait()).await {
                Ok(Ok(_status)) => return Ok(Some(0)),
                Ok(Err(e)) => {
                    return Err(SubprocessError::ProcessFailed(format!(
                        "wait after logical completion: {e}"
                    )));
                }
                Err(_) => {}
            }
        }

        terminate_child(child).await;
        Ok(Some(0))
    }

    let logical_done_wait = async {
        if let Some(token) = logical_done {
            token.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    // Wait for: process exit, timeout, user cancellation, or logical completion.
    let exit_result = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(s) => Ok(s.code()),
                Err(e) => Err(SubprocessError::ProcessFailed(format!("wait: {e}"))),
            }
        }
        _ = tokio::time::sleep(timeout), if config.timeout_secs > 0 => {
            terminate_child(&mut child).await;
            Err(SubprocessError::Timeout(config.timeout_secs))
        }
        _ = cancel.cancelled() => {
            terminate_child(&mut child).await;
            Err(SubprocessError::Cancelled)
        }
        _ = logical_done_wait => {
            complete_after_logical_done(&mut child, logical_done_grace).await
        }
    };

    // Give the stdout reader a moment to flush remaining buffered lines.
    let full_stdout =
        match tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut stdout_task).await {
            Ok(Ok(s)) => s,
            Ok(Err(_)) => String::new(),
            Err(_) => {
                stdout_task.abort();
                String::new()
            }
        };

    let stderr =
        match tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut stderr_task).await {
            Ok(Ok(s)) => s,
            Ok(Err(_)) => None,
            Err(_) => {
                stderr_task.abort();
                None
            }
        };

    match exit_result {
        Ok(exit_code) => Ok(SubprocessOutput {
            stdout: full_stdout,
            stderr,
            exit_code,
        }),
        Err(e) => Err(e),
    }
}

const DEFAULT_PTY_ROWS: u16 = 40;
const DEFAULT_PTY_COLS: u16 = 120;

async fn try_run_subprocess_streaming_until_pty(
    config: &SubprocessConfig<'_>,
    line_tx: &tokio::sync::mpsc::Sender<StreamingOutputLine>,
    cancel: CancellationToken,
    logical_done: Option<CancellationToken>,
    logical_done_grace: tokio::time::Duration,
) -> Result<Option<SubprocessOutput>, SubprocessError> {
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: DEFAULT_PTY_ROWS,
        cols: DEFAULT_PTY_COLS,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(_) => return Ok(None),
    };

    let plan = build_subprocess_invocation_plan(config.command, config.command, config.args);

    let mut cmd = CommandBuilder::new(&plan.command);
    cmd.args(&plan.args);
    if let Some(cwd) = config.cwd {
        cmd.cwd(cwd);
    }
    if let Some(envs) = config.env {
        for (k, v) in envs {
            cmd.env(k, v);
        }
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(child) => child,
        Err(_) => return Ok(None),
    };
    drop(pair.slave);

    let master = Arc::new(Mutex::new(pair.master));

    if let Some(data) = config.stdin_data {
        log_debug_stdio("STDIN", config.command, data);
        let mut writer = master
            .lock()
            .map_err(|_| SubprocessError::ProcessFailed("pty master poisoned".to_string()))?
            .take_writer()
            .map_err(|e| SubprocessError::StdinFailed(e.to_string()))?;
        writer
            .write_all(data.as_bytes())
            .map_err(|e| SubprocessError::StdinFailed(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| SubprocessError::StdinFailed(e.to_string()))?;
        drop(writer);
    }

    let reader = master
        .lock()
        .map_err(|_| SubprocessError::ProcessFailed("pty master poisoned".to_string()))?
        .try_clone_reader()
        .map_err(|e| SubprocessError::ProcessFailed(format!("pty reader: {e}")))?;
    let pty_registry_key = config.pty_registry_key;
    if let Some(turn_id) = pty_registry_key {
        register_turn_pty(turn_id, Arc::clone(&master));
    }

    let line_tx_clone = line_tx.clone();
    let (stdout_done_tx, stdout_done_rx) = std_mpsc::channel::<String>();
    let cmd_name = config.command.to_string();
    thread::spawn(move || {
        let mut reader = reader;
        let mut full_bytes = Vec::new();
        let mut pending = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => {
                    full_bytes.extend_from_slice(&buf[..read]);
                    pending.extend_from_slice(&buf[..read]);
                    for segment in take_terminal_segments(&mut pending) {
                        log_debug_stdio("STDOUT", &cmd_name, &segment);
                        let _ = line_tx_clone.blocking_send(StreamingOutputLine {
                            text: segment,
                            transport: "pty".to_string(),
                        });
                    }
                }
                Err(_) => break,
            }
        }
        if let Some(segment) = flush_terminal_tail(&mut pending) {
            log_debug_stdio("STDOUT", &cmd_name, &segment);
            let _ = line_tx_clone.blocking_send(StreamingOutputLine {
                text: segment,
                transport: "pty".to_string(),
            });
        }
        let _ = stdout_done_tx.send(String::from_utf8_lossy(&full_bytes).into_owned());
    });

    async fn terminate_pty_child(
        child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    ) -> Result<(), SubprocessError> {
        let pid = child.process_id();
        child
            .kill()
            .map_err(|e| SubprocessError::ProcessFailed(format!("pty kill: {e}")))?;

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(SubprocessError::ProcessFailed(format!(
                        "pty try_wait after kill: {e}"
                    )));
                }
            }
        }

        #[cfg(windows)]
        if let Some(pid) = pid {
            let mut taskkill = tokio::process::Command::new("taskkill");
            suppress_windows_console_for_tokio_command(&mut taskkill);
            let _ = taskkill
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
        }

        Ok(())
    }

    async fn wait_pty_exit_code(
        child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    ) -> Result<Option<i32>, SubprocessError> {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Ok(Some(i32::try_from(status.exit_code()).unwrap_or(i32::MAX)));
                }
                Ok(None) => tokio::time::sleep(tokio::time::Duration::from_millis(50)).await,
                Err(e) => return Err(SubprocessError::ProcessFailed(format!("pty wait: {e}"))),
            }
        }
    }

    async fn complete_after_logical_done_pty(
        child: &mut Box<dyn portable_pty::Child + Send + Sync>,
        grace: tokio::time::Duration,
    ) -> Result<Option<i32>, SubprocessError> {
        if !grace.is_zero() {
            let deadline = tokio::time::Instant::now() + grace;
            while tokio::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_status)) => return Ok(Some(0)),
                    Ok(None) => tokio::time::sleep(tokio::time::Duration::from_millis(50)).await,
                    Err(e) => {
                        return Err(SubprocessError::ProcessFailed(format!(
                            "pty wait after logical completion: {e}"
                        )));
                    }
                }
            }
        }

        terminate_pty_child(child).await?;
        Ok(Some(0))
    }

    let timeout = tokio::time::Duration::from_secs(config.timeout_secs);
    let logical_done_wait = async {
        if let Some(token) = logical_done {
            token.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    let exit_result = tokio::select! {
        result = wait_pty_exit_code(&mut child) => result,
        _ = tokio::time::sleep(timeout), if config.timeout_secs > 0 => {
            terminate_pty_child(&mut child).await?;
            Err(SubprocessError::Timeout(config.timeout_secs))
        }
        _ = cancel.cancelled() => {
            terminate_pty_child(&mut child).await?;
            Err(SubprocessError::Cancelled)
        }
        _ = logical_done_wait => {
            complete_after_logical_done_pty(&mut child, logical_done_grace).await
        }
    };

    let full_stdout = tokio::task::spawn_blocking(move || {
        stdout_done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();

    let output = match exit_result {
        Ok(exit_code) => Ok(Some(SubprocessOutput {
            stdout: full_stdout,
            stderr: None,
            exit_code,
        })),
        Err(e) => Err(e),
    };

    if let Some(turn_id) = pty_registry_key {
        unregister_turn_pty(turn_id);
    }

    output
}

// ── Internal helpers ──

/// Spawn the process, write stdin, start stderr drain task.
async fn spawn_and_setup(
    config: &SubprocessConfig<'_>,
) -> Result<
    (
        tokio::process::Child,
        tokio::task::JoinHandle<Option<String>>,
    ),
    SubprocessError,
> {
    let plan = build_subprocess_invocation_plan(config.command, config.command, config.args);
    let command = plan.command.as_str();
    let args = plan.args.as_slice();

    let mut cmd = tokio::process::Command::new(command);
    suppress_windows_console_for_tokio_command(&mut cmd);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    if let Some(envs) = config.env {
        cmd.envs(envs);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SubprocessError::NotFound(command.to_string())
        } else {
            SubprocessError::SpawnFailed(format!("{command}: {e}"))
        }
    })?;

    // Write stdin then close
    if let Some(data) = config.stdin_data {
        log_debug_stdio("STDIN", config.command, data);
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data.as_bytes())
                .await
                .map_err(|e| SubprocessError::StdinFailed(e.to_string()))?;
            stdin.shutdown().await.ok();
        }
    } else {
        drop(child.stdin.take());
    }

    // Drain stderr concurrently to prevent pipe buffer deadlock
    let stderr_task = {
        let handle = child.stderr.take();
        let cmd_name = config.command.to_string();
        tokio::spawn(async move {
            if let Some(mut h) = handle {
                let mut buf = String::new();
                tokio::io::AsyncReadExt::read_to_string(&mut h, &mut buf)
                    .await
                    .ok();
                if buf.is_empty() {
                    None
                } else {
                    log_debug_stdio("STDERR", &cmd_name, &buf);
                    Some(buf)
                }
            } else {
                None
            }
        })
    };

    Ok((child, stderr_task))
}

pub fn build_subprocess_invocation_plan(
    original_command: &str,
    resolved_command: &str,
    args: &[String],
) -> SubprocessInvocationPlan {
    let resolved = resolve_subprocess_invocation(resolved_command, args);
    let actual_display = if resolved.used_npm_wrapper_rewrite {
        match resolved.js_entry.as_deref() {
            Some(js_entry) => format!("{} {}", resolved.command, js_entry),
            None => resolved.command.clone(),
        }
    } else {
        resolved.command.clone()
    };

    SubprocessInvocationPlan {
        command: resolved.command.clone(),
        args: resolved.args.clone(),
        execution: ExecutionTelemetry {
            original_command: original_command.to_string(),
            resolved_command: resolved_command.to_string(),
            actual_command: resolved.command,
            actual_display,
            io_transport: None,
            used_npm_wrapper_rewrite: resolved.used_npm_wrapper_rewrite,
            js_entry: resolved.js_entry,
            node_path: resolved.node_path,
            terminal_rows: Some(DEFAULT_PTY_ROWS),
            terminal_cols: Some(DEFAULT_PTY_COLS),
        },
    }
}

#[derive(Debug, Clone)]
struct ResolvedInvocation {
    command: String,
    args: Vec<String>,
    used_npm_wrapper_rewrite: bool,
    js_entry: Option<String>,
    node_path: Option<String>,
}

fn resolve_subprocess_invocation(command: &str, args: &[String]) -> ResolvedInvocation {
    if let Some((node_path, js_entry, rewritten_args)) =
        rewrite_windows_npm_wrapper_with_node(command, args, find_on_path("node"))
    {
        return ResolvedInvocation {
            command: node_path.clone(),
            args: rewritten_args,
            used_npm_wrapper_rewrite: true,
            js_entry: Some(js_entry),
            node_path: Some(node_path),
        };
    }

    ResolvedInvocation {
        command: command.to_string(),
        args: args.to_vec(),
        used_npm_wrapper_rewrite: false,
        js_entry: None,
        node_path: None,
    }
}

fn rewrite_windows_npm_wrapper_with_node(
    command: &str,
    args: &[String],
    node_command: Option<String>,
) -> Option<(String, String, Vec<String>)> {
    if !cfg!(windows) || !is_windows_batch_wrapper(command) {
        return None;
    }

    let js_entry = resolve_npm_entry(command)?;
    let node = node_command?;

    let mut rewritten_args = Vec::with_capacity(args.len() + 1);
    rewritten_args.push(js_entry.to_string_lossy().to_string());
    rewritten_args.extend(args.iter().cloned());

    Some((node, js_entry.to_string_lossy().to_string(), rewritten_args))
}

/// Read all stdout (non-streaming), then finish.
async fn collect_output(
    mut child: tokio::process::Child,
    stderr_task: tokio::task::JoinHandle<Option<String>>,
    timeout_secs: u64,
) -> Result<SubprocessOutput, SubprocessError> {
    if timeout_secs == 0 {
        let full_stdout = read_child_stdout(&mut child).await;
        let stderr = stderr_task.await.ok().flatten();
        let status = child
            .wait()
            .await
            .map_err(|e| SubprocessError::ProcessFailed(format!("wait failed: {e}")))?;
        return Ok(SubprocessOutput {
            stdout: full_stdout,
            stderr,
            exit_code: status.code(),
        });
    }

    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    match tokio::time::timeout(timeout, read_child_stdout(&mut child)).await {
        Ok(full_stdout) => {
            let stderr = stderr_task.await.ok().flatten();
            let status = child
                .wait()
                .await
                .map_err(|e| SubprocessError::ProcessFailed(format!("wait failed: {e}")))?;
            Ok(SubprocessOutput {
                stdout: full_stdout,
                stderr,
                exit_code: status.code(),
            })
        }
        Err(_) => {
            child.kill().await.ok();
            stderr_task.abort();
            Err(SubprocessError::Timeout(timeout_secs))
        }
    }
}

async fn read_child_stdout(child: &mut tokio::process::Child) -> String {
    let mut full_stdout = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let mut reader = stdout;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(read) => full_stdout.extend_from_slice(&buf[..read]),
                Err(_) => break,
            }
        }
    }
    String::from_utf8_lossy(&full_stdout).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_test_binary_list_command() -> (String, Vec<String>) {
        (
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            vec!["--list".to_string()],
        )
    }

    #[tokio::test]
    async fn run_subprocess_timeout_zero_waits_for_process() {
        let (command, args) = current_test_binary_list_command();
        let config = SubprocessConfig {
            command: &command,
            args: &args,
            stdin_data: None,
            timeout_secs: 0,
            cwd: None,
            pty_registry_key: None,
            prefer_pty: false,
            env: None,
        };

        let output = run_subprocess(&config).await.unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(
            output
                .stdout
                .contains("run_subprocess_timeout_zero_waits_for_process")
        );
    }

    #[tokio::test]
    async fn run_subprocess_streaming_timeout_zero_waits_for_process() {
        let (command, args) = current_test_binary_list_command();
        let config = SubprocessConfig {
            command: &command,
            args: &args,
            stdin_data: None,
            timeout_secs: 0,
            cwd: None,
            pty_registry_key: None,
            prefer_pty: false,
            env: None,
        };
        let (line_tx, mut line_rx) = tokio::sync::mpsc::channel(32);

        let output = run_subprocess_streaming(&config, &line_tx, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(
            output
                .stdout
                .contains("run_subprocess_streaming_timeout_zero_waits_for_process")
        );
        let mut streamed = String::new();
        while let Ok(line) = line_rx.try_recv() {
            streamed.push_str(&line.text);
        }
        assert!(streamed.contains("run_subprocess_streaming_timeout_zero_waits_for_process"));
    }

    #[test]
    fn rewrite_windows_npm_wrapper_expands_to_node_and_js_entry() {
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("codex.cmd");
        let shell = dir.path().join("codex");
        let js = dir
            .path()
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin");
        std::fs::create_dir_all(&js).unwrap();
        std::fs::write(js.join("codex.js"), "console.log('ok')").unwrap();
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();
        std::fs::write(
            &shell,
            "#!/bin/sh\nexec node  \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\n",
        )
        .unwrap();

        let args = vec!["exec".to_string(), "--json".to_string(), "-".to_string()];
        let rewritten = rewrite_windows_npm_wrapper_with_node(
            &wrapper.to_string_lossy(),
            &args,
            Some(r"C:\Program Files\nodejs\node.exe".to_string()),
        )
        .unwrap();

        assert_eq!(rewritten.0, r"C:\Program Files\nodejs\node.exe");
        assert!(
            rewritten
                .1
                .replace('/', "\\")
                .ends_with(r"node_modules\@openai\codex\bin\codex.js")
        );
        assert_eq!(&rewritten.2[1..], args.as_slice());
    }

    #[test]
    fn rewrite_windows_npm_wrapper_returns_none_without_node() {
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("gemini.cmd");
        let shell = dir.path().join("gemini");
        let js = dir
            .path()
            .join("node_modules")
            .join("@google")
            .join("gemini-cli")
            .join("dist");
        std::fs::create_dir_all(&js).unwrap();
        std::fs::write(js.join("index.js"), "console.log('ok')").unwrap();
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();
        std::fs::write(
            &shell,
            "#!/bin/sh\nexec node  \"$basedir/node_modules/@google/gemini-cli/dist/index.js\" \"$@\"\n",
        )
        .unwrap();

        let args = vec!["-v".to_string()];
        assert!(
            rewrite_windows_npm_wrapper_with_node(&wrapper.to_string_lossy(), &args, None)
                .is_none()
        );
    }

    #[test]
    fn subprocess_plan_records_wrapper_rewrite_telemetry() {
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("gemini.cmd");
        let shell = dir.path().join("gemini");
        let js = dir
            .path()
            .join("node_modules")
            .join("@google")
            .join("gemini-cli")
            .join("dist");
        std::fs::create_dir_all(&js).unwrap();
        std::fs::write(js.join("index.js"), "console.log('ok')").unwrap();
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();
        std::fs::write(
            &shell,
            "#!/bin/sh\nexec node  \"$basedir/node_modules/@google/gemini-cli/dist/index.js\" \"$@\"\n",
        )
        .unwrap();

        let args = vec![
            "-p".to_string(),
            "".to_string(),
            "-o".to_string(),
            "stream-json".to_string(),
        ];
        let plan = build_subprocess_invocation_plan("gemini", &wrapper.to_string_lossy(), &args);

        assert_eq!(plan.execution.original_command, "gemini");
        assert_eq!(plan.execution.resolved_command, wrapper.to_string_lossy());
        if cfg!(windows) {
            assert!(plan.execution.used_npm_wrapper_rewrite);
            assert!(plan.execution.node_path.is_some());
            assert!(plan.execution.js_entry.is_some());
        } else {
            assert!(!plan.execution.used_npm_wrapper_rewrite);
        }
    }

    #[test]
    fn structured_modes_disable_pty_even_when_preferred() {
        let args = vec!["exec".to_string(), "--json".to_string(), "-".to_string()];
        let config = SubprocessConfig {
            command: "codex",
            args: &args,
            stdin_data: None,
            timeout_secs: 30,
            cwd: None,
            pty_registry_key: None,
            prefer_pty: true,
            env: None,
        };

        assert!(!should_use_pty(&config));
    }

    #[test]
    fn unstructured_invocation_can_still_prefer_pty() {
        let args = vec!["bash".to_string()];
        let config = SubprocessConfig {
            command: "cmd",
            args: &args,
            stdin_data: None,
            timeout_secs: 30,
            cwd: None,
            pty_registry_key: None,
            prefer_pty: true,
            env: None,
        };

        assert!(should_use_pty(&config));
    }
}
