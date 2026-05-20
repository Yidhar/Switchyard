use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use switchyard_text::{prefix_chars, preview_chars, preview_collapsed};
use uuid::Uuid;

use crate::ExecutionTelemetry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOutput {
    pub line: String,
    pub stream: Option<String>,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HyardJobObservation {
    pub job_id: String,
    pub provider: String,
    /// Normalized live status: wait_timeout is mapped to the underlying job_status.
    pub status: String,
    /// Raw bridge status from the HYARD envelope (e.g. wait_timeout, completed).
    pub bridge_status: String,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub execution: Option<ExecutionTelemetry>,
    pub wait_timeout_count: u32,
    pub artifact_count: usize,
    pub result_ready: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEvent {
    pub event_id: Uuid,
    pub turn_id: Uuid,
    pub event_type: EventType,
    pub provider: String,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

impl ProviderEvent {
    pub fn new(
        turn_id: Uuid,
        event_type: EventType,
        provider: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: Uuid::now_v7(),
            turn_id,
            event_type,
            provider: provider.into(),
            timestamp: Utc::now(),
            payload,
        }
    }

    pub fn text_message(
        turn_id: Uuid,
        provider: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(
            turn_id,
            EventType::ItemUpdated,
            provider,
            serde_json::json!({ "item_type": "agent_message", "text": text.into() }),
        )
    }

    pub fn turn_started(turn_id: Uuid, provider: impl Into<String>) -> Self {
        Self::new(
            turn_id,
            EventType::TurnStarted,
            provider,
            serde_json::json!({}),
        )
    }

    pub fn turn_completed(turn_id: Uuid, provider: impl Into<String>) -> Self {
        Self::new(
            turn_id,
            EventType::TurnCompleted,
            provider,
            serde_json::json!({}),
        )
    }

    pub fn turn_failed(
        turn_id: Uuid,
        provider: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self::new(
            turn_id,
            EventType::TurnFailed,
            provider,
            serde_json::json!({ "error": error.into() }),
        )
    }

    pub fn execution_telemetry(
        turn_id: Uuid,
        provider: impl Into<String>,
        execution: &ExecutionTelemetry,
    ) -> Self {
        Self::new(
            turn_id,
            EventType::ItemUpdated,
            provider,
            serde_json::json!({
                "item_type": "execution_telemetry",
                "execution": execution,
            }),
        )
    }

    pub fn terminal_output(
        turn_id: Uuid,
        provider: impl Into<String>,
        line: impl Into<String>,
        stream: Option<&str>,
        transport: Option<&str>,
    ) -> Self {
        Self::new(
            turn_id,
            EventType::ItemUpdated,
            provider,
            serde_json::json!({
                "item_type": "terminal_output",
                "line": line.into(),
                "stream": stream,
                "transport": transport,
            }),
        )
    }

    /// Extract a human-readable display text from the event payload.
    ///
    /// Supports all three provider payload shapes:
    /// - `payload["text"]` — Switchyard text_message (shared)
    /// - `payload["item"]["text"]` — Codex `item.completed` events
    /// - `payload["result"]` — Claude `result` events
    /// - `payload["message"]["content"][*]["text"]` — Claude `assistant` events
    /// - `payload["content"]` — Gemini `message` events
    pub fn display_text(&self) -> Option<String> {
        extract_display_text(&self.payload)
    }

    /// Extract a human-readable text when available, otherwise return a compact
    /// activity summary so UIs can still show that the provider is doing work.
    pub fn display_text_or_summary(&self) -> Option<String> {
        extract_display_text(&self.payload).or_else(|| extract_activity_summary(&self.payload))
    }

    /// Get a compact one-line summary suitable for a raw stream pane.
    pub fn raw_line(&self) -> String {
        self.payload.to_string()
    }
}

/// Extract a human-readable display text from a provider event payload.
///
/// Tries multiple paths used by different providers:
/// 1. `payload["text"]` — Switchyard text_message
/// 2. `payload["item"]["text"]` — Codex item.completed
/// 3. `payload["result"]` — Claude result event
/// 4. `payload["message"]["content"][*]["text"]` — Claude assistant event
/// 5. `payload["content"]` — Gemini message event
pub fn extract_display_text(payload: &serde_json::Value) -> Option<String> {
    // Path 1: shared text_message format
    if let Some(t) = payload.get("text").and_then(|v| v.as_str())
        && !t.is_empty()
    {
        return Some(t.to_string());
    }
    // Path 2: Codex item.completed → item.text
    if let Some(t) = payload
        .get("item")
        .and_then(|i| i.get("text"))
        .and_then(|v| v.as_str())
        && !t.is_empty()
    {
        return Some(t.to_string());
    }
    // Path 3: Claude result
    if let Some(t) = payload.get("result").and_then(|v| v.as_str())
        && !t.is_empty()
    {
        return Some(t.to_string());
    }
    // Path 4: Claude assistant message content blocks
    if let Some(blocks) = payload
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(t) = block.get("text").and_then(|v| v.as_str())
                && !t.is_empty()
            {
                return Some(t.to_string());
            }
        }
    }
    // Path 5: Gemini content
    if let Some(t) = payload.get("content").and_then(|v| v.as_str())
        && !t.is_empty()
    {
        return Some(t.to_string());
    }
    // Path 6: Delta text (Codex item.delta or Claude content_block_delta)
    if let Some(delta) = payload.get("delta") {
        if let Some(t) = delta.get("text").and_then(|v| v.as_str())
            && !t.is_empty()
        {
            return Some(t.to_string());
        }
        if let Some(t) = delta.get("delta").and_then(|d2| d2.get("text")).and_then(|v| v.as_str())
            && !t.is_empty()
        {
            return Some(t.to_string());
        }
    }
    None
}

pub fn extract_execution_telemetry(payload: &serde_json::Value) -> Option<ExecutionTelemetry> {
    if payload.get("item_type").and_then(|t| t.as_str()) != Some("execution_telemetry") {
        return None;
    }

    serde_json::from_value(payload.get("execution")?.clone()).ok()
}

pub fn extract_terminal_output(payload: &serde_json::Value) -> Option<TerminalOutput> {
    if payload.get("item_type").and_then(|t| t.as_str()) != Some("terminal_output") {
        return None;
    }

    Some(TerminalOutput {
        line: payload
            .get("line")
            .and_then(|value| value.as_str())?
            .to_string(),
        stream: payload
            .get("stream")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        transport: payload
            .get("transport")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
    })
}

pub fn extract_hyard_job_observation(payload: &serde_json::Value) -> Option<HyardJobObservation> {
    let item = payload.get("item")?;
    if item.get("type").and_then(|t| t.as_str()) != Some("command_execution") {
        return None;
    }

    let command = item
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    if !is_hyard_host_command(command) {
        return None;
    }

    let aggregated_output = item
        .get("aggregated_output")
        .and_then(|o| o.as_str())
        .unwrap_or_default()
        .trim();
    if aggregated_output.is_empty() {
        return None;
    }

    let json = serde_json::from_str::<serde_json::Value>(aggregated_output).ok()?;
    let job_id = json.get("job_id").and_then(|value| value.as_str())?;
    let bridge_status = json.get("status").and_then(|value| value.as_str())?;
    let normalized_status = if bridge_status == "wait_timeout" {
        json.get("job_status")
            .and_then(|value| value.as_str())
            .unwrap_or("running")
    } else {
        bridge_status
    };

    Some(HyardJobObservation {
        job_id: job_id.to_string(),
        provider: json
            .get("provider")
            .and_then(|value| value.as_str())
            .unwrap_or("peer")
            .to_string(),
        status: normalized_status.to_string(),
        bridge_status: bridge_status.to_string(),
        last_event: json
            .get("last_event")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        last_output_preview: json
            .get("last_output_preview")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
        execution: json
            .get("execution")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
        wait_timeout_count: json
            .get("wait_timeout_count")
            .and_then(|value| value.as_u64())
            .unwrap_or_default() as u32,
        artifact_count: json
            .get("artifact_count")
            .and_then(|value| value.as_u64())
            .unwrap_or_default() as usize,
        result_ready: json
            .get("result_ready")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        error: json
            .get("error")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
    })
}

/// Extract a compact activity marker for provider payloads that do not yet
/// contain user-facing text.
pub fn extract_activity_summary(payload: &serde_json::Value) -> Option<String> {
    if let Some(summary) = extract_command_execution_summary(payload) {
        return Some(summary);
    }

    if let Some(msg_type) = payload.get("type").and_then(|t| t.as_str()) {
        match msg_type {
            "thread.started" => return Some("[会话] 已启动".to_string()),
            "turn.started" => return Some("[回合] 已开始".to_string()),
            "system" => {
                if payload.get("subtype").and_then(|t| t.as_str()) == Some("init") {
                    return Some("[系统] 已初始化环境".to_string());
                }
                return Some("[系统] 状态已更新".to_string());
            }
            "assistant" => return Some("[助手] 正在输出回复".to_string()),
            "result" => {
                if payload.get("subtype").and_then(|t| t.as_str()) == Some("success") {
                    return Some("[结果] 已返回".to_string());
                }
                return Some("[结果] 状态已更新".to_string());
            }
            "rate_limit_event" => return Some("[限额] 已更新".to_string()),
            _ => {}
        }
    }

    if let Some(item_type) = payload
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(|t| t.as_str())
    {
        return Some(match item_type {
            "reasoning" => "[思考] 正在整理推理".to_string(),
            "tool_call" => "[工具] 正在调用工具".to_string(),
            "file_change" => "[文件] 正在修改".to_string(),
            "diff_ready" => "[Diff] 已生成差异".to_string(),
            "todo_list" => "[待办] 已更新".to_string(),
            "delegate_request" => "[委托] 已生成请求".to_string(),
            "delegate_result" => "[委托] 已返回结果".to_string(),
            "error" => payload
                .get("item")
                .and_then(|item| item.get("message"))
                .and_then(|m| m.as_str())
                .map(|message| format!("[错误] {}", preview(message, 80)))
                .unwrap_or_else(|| "[错误]".to_string()),
            _ => {
                if let Some(msg_type) = payload.get("type").and_then(|t| t.as_str()) {
                    format!("[{msg_type}:{item_type}]")
                } else {
                    format!("[item:{item_type}]")
                }
            }
        });
    }

    if let Some(msg_type) = payload.get("type").and_then(|t| t.as_str()) {
        if let Some(role) = payload.get("role").and_then(|r| r.as_str()) {
            return Some(format!("[{msg_type}:{role}]"));
        }
        return Some(format!("[{msg_type}]"));
    }

    if let Some(item_type) = payload.get("item_type").and_then(|t| t.as_str()) {
        if item_type == "terminal_output" {
            return None;
        }
        if item_type == "execution_telemetry" {
            return Some(
                extract_execution_telemetry(payload)
                    .map(summarize_execution_telemetry)
                    .unwrap_or_else(|| "[执行] 已解析命令".to_string()),
            );
        }
        return Some(format!("[{item_type}]"));
    }

    if let Some(error) = payload.get("error").and_then(|e| e.as_str()) {
        let preview = preview_chars(error, 80, "…");
        return Some(format!("[error] {preview}"));
    }

    None
}

fn summarize_execution_telemetry(execution: ExecutionTelemetry) -> String {
    let base = if execution.used_npm_wrapper_rewrite {
        format!(
            "[执行] npm wrapper 已改写：{} -> {}",
            preview_path_leaf(&execution.resolved_command),
            preview_path_leaf(
                execution
                    .js_entry
                    .as_deref()
                    .unwrap_or(&execution.actual_command)
            )
        )
    } else if execution.original_command != execution.actual_command {
        format!(
            "[执行] 命令已解析：{} -> {}",
            preview_path_leaf(&execution.original_command),
            preview_path_leaf(&execution.actual_command)
        )
    } else {
        format!(
            "[执行] 使用命令：{}",
            preview_path_leaf(&execution.actual_command)
        )
    };

    match execution.io_transport.as_deref() {
        Some(transport) => format!("[{}] {base}", transport.to_uppercase()),
        None => base,
    }
}

fn preview_path_leaf(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn extract_command_execution_summary(payload: &serde_json::Value) -> Option<String> {
    let item = payload.get("item")?;
    if item.get("type").and_then(|t| t.as_str()) != Some("command_execution") {
        return None;
    }

    let command = item
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    let status = item
        .get("status")
        .and_then(|s| s.as_str())
        .or_else(|| {
            payload
                .get("type")
                .and_then(|t| t.as_str())
                .and_then(|t| (t == "item.started").then_some("in_progress"))
        })
        .unwrap_or("unknown");
    let exit_code = item.get("exit_code").and_then(|c| c.as_i64());
    let aggregated_output = item
        .get("aggregated_output")
        .and_then(|o| o.as_str())
        .unwrap_or_default()
        .trim();

    if is_hyard_host_command(command) {
        return Some(summarize_hyard_host_command(
            command,
            status,
            exit_code,
            aggregated_output,
        ));
    }

    let command_preview = preview_command(command);
    Some(match status {
        "in_progress" => format!("[命令] 开始执行：{command_preview}"),
        "completed" if exit_code == Some(0) => format!("[命令] 执行完成：{command_preview}"),
        "completed" | "failed" => format!(
            "[命令] 执行失败(exit={})：{command_preview}",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "?".to_string())
        ),
        _ => format!("[命令] 状态更新：{command_preview}"),
    })
}

fn is_hyard_host_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("switchyard.exe host ") || lower.contains("switchyard host ")
}

fn summarize_hyard_host_command(
    command: &str,
    status: &str,
    exit_code: Option<i64>,
    aggregated_output: &str,
) -> String {
    let action = detect_hyard_host_action(command);
    let target_provider = extract_host_provider_arg(command);

    if status == "in_progress" {
        return match action {
            Some("list") => "[hyard] 正在检查可用代理".to_string(),
            Some("help") => "[hyard] 正在读取 HYARD 命令帮助".to_string(),
            Some("delegate") => format!(
                "[hyard] 正在发起委托 -> {}",
                target_provider
                    .as_deref()
                    .map(display_provider_name)
                    .unwrap_or_else(|| "peer".to_string())
            ),
            Some("status") => "[hyard] 正在查询任务状态".to_string(),
            Some("await") => "[hyard] 正在继续等待任务完成".to_string(),
            Some("result") => "[hyard] 正在读取任务结果".to_string(),
            Some("cancel") => "[hyard] 正在请求取消任务".to_string(),
            _ => "[hyard] 正在执行 host 命令".to_string(),
        };
    }

    if !aggregated_output.is_empty()
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(aggregated_output)
    {
        if let Some(peers) = json.get("peers").and_then(|p| p.as_array()) {
            let total = peers.len();
            let ready = peers
                .iter()
                .filter(|peer| peer.get("available").and_then(|v| v.as_bool()) == Some(true))
                .count();
            return format!("[hyard] 已更新可用代理：{ready}/{total} 可用");
        }

        if json.get("commands").is_some() && json.get("protocol").is_some() {
            return "[hyard] 已读取命令帮助与协议".to_string();
        }

        if let Some(job_status) = summarize_hyard_job_json(&json, action) {
            return job_status;
        }
    }

    match status {
        "completed" if exit_code == Some(0) => "[hyard] host 命令执行完成".to_string(),
        "completed" | "failed" => format!(
            "[hyard] host 命令执行失败(exit={})",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "?".to_string())
        ),
        _ => "[hyard] host 命令状态已更新".to_string(),
    }
}

fn summarize_hyard_job_json(json: &serde_json::Value, action: Option<&str>) -> Option<String> {
    let status = json.get("status").and_then(|s| s.as_str())?;
    let provider = json
        .get("provider")
        .and_then(|p| p.as_str())
        .map(display_provider_name)
        .unwrap_or_else(|| "peer".to_string());
    let short_job = json
        .get("job_id")
        .and_then(|id| id.as_str())
        .map(short_job_id)
        .unwrap_or_else(|| "????".to_string());
    let error = json
        .get("error")
        .and_then(|e| e.as_str())
        .map(|message| preview(message, 80));
    let action_label = match action {
        Some("delegate") => "任务",
        Some("await") => "任务",
        Some("result") => "结果",
        Some("status") => "状态",
        Some("cancel") => "取消请求",
        _ => "任务",
    };

    Some(match status {
        "wait_timeout" => {
            let runtime_status = json
                .get("job_status")
                .and_then(|s| s.as_str())
                .unwrap_or("running");
            format!(
                "[hyard] {provider} {action_label}仍在后台运行 (job {short_job} / {runtime_status})；本次等待超时，可继续 status/result/await，并先处理其他工作"
            )
        }
        "completed" => format!("[hyard] {provider} {action_label}已完成 (job {short_job})"),
        "running" | "queued" | "cancel_requested" => {
            format!("[hyard] {provider} {action_label}状态：{status} (job {short_job})")
        }
        "failed" => format!(
            "[hyard] {provider} {action_label}失败 (job {short_job})：{}",
            error.unwrap_or_else(|| "未知错误".to_string())
        ),
        "cancelled" => format!("[hyard] {provider} {action_label}已取消 (job {short_job})"),
        _ => format!("[hyard] {provider} {action_label}状态：{status} (job {short_job})"),
    })
}

fn detect_hyard_host_action(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    for action in [
        "delegate", "list", "help", "status", "await", "result", "cancel",
    ] {
        let needle = format!(" host {action}");
        if lower.contains(&needle) {
            return Some(action);
        }
    }
    None
}

fn extract_host_provider_arg(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let mut idx = 0;
    while idx < parts.len() {
        if parts[idx].eq_ignore_ascii_case("--provider") {
            return parts.get(idx + 1).map(|value| trim_matching_quotes(value));
        }
        idx += 1;
    }
    None
}

fn trim_matching_quotes(value: &str) -> String {
    value.trim_matches(|ch| ch == '"' || ch == '\'').to_string()
}

fn display_provider_name(provider: &str) -> String {
    match provider.to_ascii_lowercase().as_str() {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        "gemini" => "Gemini".to_string(),
        other => other.to_string(),
    }
}

fn short_job_id(job_id: &str) -> String {
    prefix_chars(job_id, 8)
}

fn preview_command(command: &str) -> String {
    preview(command, 72)
}

fn preview(text: &str, max_chars: usize) -> String {
    preview_collapsed(text, max_chars, "…")
}

/// Provider event types. Identical to switchyard-session::EventType so that
/// serialized forms are wire-compatible. Conversion in switchyard-core is a
/// trivial variant-to-variant match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventType {
    ThreadStarted,
    TurnStarted,
    ItemStarted,
    ItemUpdated,
    ItemCompleted,
    ArtifactReady,
    DelegateRequested,
    DelegateCompleted,
    TurnCompleted,
    TurnFailed,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThreadStarted => write!(f, "thread_started"),
            Self::TurnStarted => write!(f, "turn_started"),
            Self::ItemStarted => write!(f, "item_started"),
            Self::ItemUpdated => write!(f, "item_updated"),
            Self::ItemCompleted => write!(f, "item_completed"),
            Self::ArtifactReady => write!(f, "artifact_ready"),
            Self::DelegateRequested => write!(f, "delegate_requested"),
            Self::DelegateCompleted => write!(f, "delegate_completed"),
            Self::TurnCompleted => write!(f, "turn_completed"),
            Self::TurnFailed => write!(f, "turn_failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemType {
    AgentMessage,
    Reasoning,
    CommandExecution,
    FileChange,
    DiffReady,
    ToolCall,
    TodoList,
    DelegateRequest,
    DelegateResult,
    Error,
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentMessage => write!(f, "agent_message"),
            Self::Reasoning => write!(f, "reasoning"),
            Self::CommandExecution => write!(f, "command_execution"),
            Self::FileChange => write!(f, "file_change"),
            Self::DiffReady => write!(f, "diff_ready"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::TodoList => write!(f, "todo_list"),
            Self::DelegateRequest => write!(f, "delegate_request"),
            Self::DelegateResult => write!(f, "delegate_result"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_execution() -> ExecutionTelemetry {
        ExecutionTelemetry {
            original_command: "gemini".to_string(),
            resolved_command: r"C:\Users\demo\AppData\Roaming\npm\gemini.cmd".to_string(),
            actual_command: r"C:\Program Files\nodejs\node.exe".to_string(),
            actual_display: r#"C:\Program Files\nodejs\node.exe C:\Users\demo\AppData\Roaming\npm\node_modules\@google\gemini-cli\dist\index.js"#.to_string(),
            io_transport: Some("pty".to_string()),
            used_npm_wrapper_rewrite: true,
            js_entry: Some(
                r"C:\Users\demo\AppData\Roaming\npm\node_modules\@google\gemini-cli\dist\index.js"
                    .to_string(),
            ),
            node_path: Some(r"C:\Program Files\nodejs\node.exe".to_string()),
            terminal_rows: Some(40),
            terminal_cols: Some(120),
        }
    }

    #[test]
    fn extract_activity_summary_prefers_item_type_detail() {
        let payload = serde_json::json!({
            "type": "item.completed",
            "item": { "type": "tool_call" }
        });

        assert_eq!(
            extract_activity_summary(&payload).as_deref(),
            Some("[工具] 正在调用工具")
        );
    }

    #[test]
    fn display_text_or_summary_falls_back_when_no_text_exists() {
        let event = ProviderEvent::new(
            Uuid::nil(),
            EventType::ItemUpdated,
            "claude",
            serde_json::json!({
                "type": "assistant",
                "role": "assistant"
            }),
        );

        assert_eq!(
            event.display_text_or_summary().as_deref(),
            Some("[助手] 正在输出回复")
        );
    }

    #[test]
    fn command_execution_summary_includes_command_preview() {
        let payload = serde_json::json!({
            "type": "item.started",
            "item": {
                "type": "command_execution",
                "status": "in_progress",
                "command": "\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"Get-Content foo\""
            }
        });

        assert_eq!(
            extract_activity_summary(&payload).as_deref(),
            Some(
                "[命令] 开始执行：\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"Get-Content foo\""
            )
        );
    }

    #[test]
    fn hyard_wait_timeout_summary_is_human_readable() {
        let payload = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "status": "completed",
                "command": "\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"E:\\\\Switchyard\\\\target\\\\debug\\\\switchyard.exe host delegate --provider claude --task test --wait-sec 45\"",
                "exit_code": 0,
                "aggregated_output": "{\"job_id\":\"019d5709-f2b1-7002-8643-67a616f32d71\",\"status\":\"wait_timeout\",\"job_status\":\"running\",\"provider\":\"claude\",\"wait_timeout_count\":1}"
            }
        });

        let summary = extract_activity_summary(&payload).unwrap();
        assert!(summary.contains("[hyard]"));
        assert!(summary.contains("Claude"));
        assert!(summary.contains("等待超时"));
        assert!(summary.contains("019d5709"));
        assert!(summary.contains("先处理其他工作"));
    }

    #[test]
    fn hyard_failed_job_summary_shows_error() {
        let payload = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "status": "completed",
                "command": "\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"E:\\\\Switchyard\\\\target\\\\debug\\\\switchyard.exe host delegate --provider claude --task test --wait-sec 45\"",
                "exit_code": 0,
                "aggregated_output": "{\"job_id\":\"019d57eb-903f-73b3-98d2-9b924442fe6a\",\"status\":\"failed\",\"provider\":\"claude\",\"error\":\"provider execution failed: spawn failed: claude: access denied\"}"
            }
        });

        let summary = extract_activity_summary(&payload).unwrap();
        assert!(summary.contains("任务失败"));
        assert!(summary.contains("Claude"));
        assert!(summary.contains("access denied"));
    }

    #[test]
    fn extract_hyard_job_observation_normalizes_wait_timeout_to_running() {
        let payload = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "status": "completed",
                "command": "\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"E:\\\\Switchyard\\\\target\\\\debug\\\\switchyard.exe host delegate --provider claude --task test --wait-sec 45\"",
                "exit_code": 0,
                "aggregated_output": "{\"job_id\":\"019d5709-f2b1-7002-8643-67a616f32d71\",\"status\":\"wait_timeout\",\"job_status\":\"running\",\"provider\":\"claude\",\"wait_timeout_count\":1,\"last_event\":\"worker_booting\"}"
            }
        });

        let observation = extract_hyard_job_observation(&payload).expect("hyard job observation");
        assert_eq!(observation.job_id, "019d5709-f2b1-7002-8643-67a616f32d71");
        assert_eq!(observation.provider, "claude");
        assert_eq!(observation.bridge_status, "wait_timeout");
        assert_eq!(observation.status, "running");
        assert_eq!(observation.wait_timeout_count, 1);
        assert_eq!(observation.last_event.as_deref(), Some("worker_booting"));
    }

    #[test]
    fn extract_hyard_job_observation_preserves_failed_execution_details() {
        let payload = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "status": "completed",
                "command": "\"C:\\\\Program Files\\\\PowerShell\\\\7\\\\pwsh.exe\" -Command \"E:\\\\Switchyard\\\\target\\\\debug\\\\switchyard.exe host result --job-id 019d57eb-903f-73b3-98d2-9b924442fe6a\"",
                "exit_code": 0,
                "aggregated_output": "{\"job_id\":\"019d57eb-903f-73b3-98d2-9b924442fe6a\",\"status\":\"failed\",\"provider\":\"claude\",\"artifact_count\":2,\"result_ready\":false,\"execution\":{\"original_command\":\"claude\",\"resolved_command\":\"C:\\\\Users\\\\demo\\\\claude.exe\",\"actual_command\":\"C:\\\\Users\\\\demo\\\\claude.exe\",\"actual_display\":\"C:\\\\Users\\\\demo\\\\claude.exe\"},\"error\":\"provider execution failed: spawn failed: claude: access denied\"}"
            }
        });

        let observation = extract_hyard_job_observation(&payload).expect("hyard job observation");
        assert_eq!(observation.status, "failed");
        assert_eq!(observation.bridge_status, "failed");
        assert_eq!(observation.artifact_count, 2);
        assert!(!observation.result_ready);
        assert_eq!(
            observation.error.as_deref(),
            Some("provider execution failed: spawn failed: claude: access denied")
        );
        assert_eq!(
            observation
                .execution
                .as_ref()
                .map(|execution| execution.original_command.as_str()),
            Some("claude")
        );
    }

    #[test]
    fn execution_telemetry_round_trips_and_summarizes() {
        let execution = sample_execution();
        let event = ProviderEvent::execution_telemetry(Uuid::nil(), "gemini", &execution);

        assert_eq!(
            extract_execution_telemetry(&event.payload),
            Some(execution.clone())
        );

        let summary = event.display_text_or_summary().unwrap_or_default();
        assert!(summary.contains("[执行]"));
        assert!(summary.contains("npm wrapper 已改写"));
        assert!(summary.contains("gemini.cmd"));
        assert!(summary.contains("index.js"));
    }

    #[test]
    fn terminal_output_round_trips_without_display_summary() {
        let event = ProviderEvent::terminal_output(
            Uuid::nil(),
            "codex",
            "Working on patch",
            Some("merged"),
            Some("pty"),
        );

        assert_eq!(
            extract_terminal_output(&event.payload),
            Some(TerminalOutput {
                line: "Working on patch".to_string(),
                stream: Some("merged".to_string()),
                transport: Some("pty".to_string()),
            })
        );
        assert_eq!(event.display_text_or_summary(), None);
    }
}
