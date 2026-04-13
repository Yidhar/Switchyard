//! In-memory runtime state for TUI rendering.
//!
//! Updated via RuntimeEvent channel, not by polling the store.

use ratatui::text::Line;
use std::collections::{HashMap, HashSet, VecDeque};
use switchyard_provider_api::{ExecutionTelemetry, HostSurfaceKind, HostSurfaceProbe};
use switchyard_text::{prefix_bytes, prefix_chars, preview_chars};
use uuid::Uuid;

use crate::terminal_buffer::TerminalScreenBuffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyardJobSource {
    Live,
    Inferred,
    Store,
    Reconciled,
}

impl HyardJobSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Inferred => "inferred",
            Self::Store => "store",
            Self::Reconciled => "recovered",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Live => 4,
            Self::Reconciled => 3,
            Self::Store => 2,
            Self::Inferred => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyardJobSummary {
    pub job_id: String,
    pub provider: String,
    pub status: String,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub execution: Option<ExecutionTelemetry>,
    pub wait_timeout_count: u32,
    pub artifact_count: usize,
    pub result_ready: bool,
    pub error: Option<String>,
    pub updated_at: String,
    pub source: HyardJobSource,
}

impl HyardJobSummary {
    pub fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "queued" | "running" | "cancel_requested"
        )
    }

    pub fn is_authoritative(&self) -> bool {
        !matches!(self.source, HyardJobSource::Inferred)
    }

    pub fn short_job_id(&self) -> String {
        prefix_chars(&self.job_id, 8)
    }

    pub fn status_badge(&self) -> String {
        let label = match self.status.as_str() {
            "queued" => "排队中",
            "running" => "运行中",
            "cancel_requested" => "取消中",
            "completed" => "已完成",
            "failed" => "已失败",
            "cancelled" => "已取消",
            other => other,
        };
        if self.wait_timeout_count == 0 {
            label.to_string()
        } else {
            format!("{label}·w{}", self.wait_timeout_count)
        }
    }

    pub fn source_badge(&self) -> &'static str {
        match self.source {
            HyardJobSource::Live => "实时",
            HyardJobSource::Inferred => "待确认",
            HyardJobSource::Store => "缓存",
            HyardJobSource::Reconciled => "恢复",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostSurfaceState {
    pub kind: HostSurfaceKind,
    pub installed: bool,
    pub configured: bool,
    pub discoverable: bool,
    pub probed: bool,
    pub notes: Option<String>,
}

impl HostSurfaceState {
    pub fn new(kind: HostSurfaceKind) -> Self {
        Self {
            kind,
            installed: false,
            configured: false,
            discoverable: false,
            probed: false,
            notes: None,
        }
    }

    pub fn from_probe(probe: &HostSurfaceProbe) -> Self {
        Self {
            kind: probe.kind,
            installed: probe.installed,
            configured: probe.configured,
            discoverable: probe.discoverable,
            probed: true,
            notes: summarize_host_surface_probe(probe),
        }
    }

    pub fn label(&self) -> &'static str {
        host_surface_label(self.kind)
    }

    pub fn readiness(&self) -> HostSurfaceReadiness {
        if !self.probed {
            HostSurfaceReadiness::Unknown
        } else if self.installed && self.configured && self.discoverable {
            HostSurfaceReadiness::Ready
        } else if !self.installed {
            HostSurfaceReadiness::Unavailable
        } else {
            HostSurfaceReadiness::Partial
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSurfaceReadiness {
    Ready,
    Partial,
    Unavailable,
    Unknown,
}

impl HostSurfaceReadiness {
    pub fn label(&self) -> &'static str {
        match self {
            HostSurfaceReadiness::Ready => "就绪",
            HostSurfaceReadiness::Partial => "部分可用",
            HostSurfaceReadiness::Unavailable => "缺失",
            HostSurfaceReadiness::Unknown => "未知",
        }
    }
}

/// Current execution phase.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Preparing,
    ProbingPeers,
    CoreRunning,
    DelegateRequested,
    PeerRunning,
    Finalizing,
    /// Provider output done, finalize/archive/save in progress.
    Committing,
}

impl Phase {
    /// True while a provider is actively executing (show "running...").
    pub fn is_executing(&self) -> bool {
        matches!(
            self,
            Phase::Preparing
                | Phase::ProbingPeers
                | Phase::CoreRunning
                | Phase::PeerRunning
                | Phase::Finalizing
        )
    }

    /// True for any non-idle state (block input submission).
    pub fn is_busy(&self) -> bool {
        !matches!(self, Phase::Idle)
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Idle => write!(f, "空闲"),
            Phase::Preparing => write!(f, "准备中"),
            Phase::ProbingPeers => write!(f, "探测 peers"),
            Phase::CoreRunning => write!(f, "主代理执行中"),
            Phase::DelegateRequested => write!(f, "已请求委托"),
            Phase::PeerRunning => write!(f, "peer 执行中"),
            Phase::Finalizing => write!(f, "收尾中"),
            Phase::Committing => write!(f, "保存中"),
        }
    }
}

/// Live runtime state, updated from RuntimeEvent channel.
pub struct RuntimeState {
    pub phase: Phase,
    pub current_provider: String,
    pub current_peer: Option<String>,
    pub current_turn_id: Option<Uuid>,
    /// Active turn ids per provider, used for PTY resize back-propagation.
    pub provider_turn_ids: HashMap<String, Uuid>,
    /// Recent streaming text lines from the active provider.
    pub stream_lines: VecDeque<String>,
    /// Recent runtime event descriptions for the event pane.
    pub event_log: VecDeque<String>,
    /// Raw JSON lines from provider stdout (for raw stream pane).
    pub raw_json_lines: VecDeque<String>,
    /// Per-provider timeline lines used by the CLI-like message views.
    pub provider_view_lines: HashMap<String, VecDeque<String>>,
    /// Per-provider terminal transcript mirrored from raw subprocess transport.
    pub provider_terminal_lines: HashMap<String, VecDeque<String>>,
    /// ANSI-aware screen buffers for provider terminal mirror views.
    pub provider_screen_buffers: HashMap<String, TerminalScreenBuffer>,
    /// Latest transport label for provider terminal output (pty/pipe).
    pub provider_terminal_transport: HashMap<String, String>,
    /// Stable first-seen order for provider views in the TUI.
    pub provider_view_order: Vec<String>,
    /// Core provider host-surface awareness, updated from real probe results.
    pub core_host_surface: HostSurfaceState,
    /// Active peer host-surface awareness, when delegation is in flight.
    pub peer_host_surface: Option<HostSurfaceState>,
    pub core_execution: Option<ExecutionTelemetry>,
    pub peer_execution: Option<ExecutionTelemetry>,
    /// Latest probed peer readiness counts.
    pub peer_ready_count: usize,
    pub peer_total_count: usize,
    pub peer_probe_done: bool,
    /// Most recent HYARD-related event text.
    pub latest_hyard_event: Option<String>,
    /// Persisted HYARD async jobs recovered from disk.
    pub persisted_hyard_jobs: Vec<HyardJobSummary>,
    /// Ephemeral HYARD jobs seen during this runtime.
    ///
    /// This may include authoritative bridge observations (`Live`) and
    /// provisional runtime hints before bridge/store confirmation (`Inferred`).
    pub live_hyard_jobs: Vec<HyardJobSummary>,
    /// Merged HYARD async jobs, preferring authoritative observation first,
    /// persisted fallback second, and inferred placeholders last.
    pub hyard_jobs: Vec<HyardJobSummary>,
    /// Authoritative active jobs only (Live/Store/Reconciled).
    pub active_hyard_job_count: usize,
    /// Authoritative active jobs currently in wait-timeout continuation state.
    pub waiting_hyard_job_count: usize,
    /// Provisional active jobs inferred from runtime events but not yet
    /// confirmed by bridge/store snapshots.
    pub inferred_hyard_job_count: usize,
    /// Whether the UI needs a redraw.
    pub dirty: bool,
    /// Elapsed time tracking.
    pub started_at: Option<std::time::Instant>,
}

const MAX_STREAM_LINES: usize = 100;
const MAX_EVENT_LOG: usize = 50;
const MAX_RAW_JSON: usize = 200;
const MAX_PROVIDER_VIEW_LINES: usize = 200;
const MAX_PROVIDER_TERMINAL_LINES: usize = 400;
const MAX_ENTRY_LEN: usize = 512;

impl RuntimeState {
    pub fn new(provider: &str) -> Self {
        Self {
            phase: Phase::Idle,
            current_provider: provider.to_string(),
            current_peer: None,
            current_turn_id: None,
            provider_turn_ids: HashMap::new(),
            stream_lines: VecDeque::new(),
            event_log: VecDeque::new(),
            raw_json_lines: VecDeque::new(),
            provider_view_lines: HashMap::new(),
            provider_terminal_lines: HashMap::new(),
            provider_screen_buffers: HashMap::new(),
            provider_terminal_transport: HashMap::new(),
            provider_view_order: Vec::new(),
            core_host_surface: HostSurfaceState::new(HostSurfaceKind::Unknown),
            peer_host_surface: None,
            core_execution: None,
            peer_execution: None,
            peer_ready_count: 0,
            peer_total_count: 0,
            peer_probe_done: false,
            latest_hyard_event: None,
            persisted_hyard_jobs: Vec::new(),
            live_hyard_jobs: Vec::new(),
            hyard_jobs: Vec::new(),
            active_hyard_job_count: 0,
            waiting_hyard_job_count: 0,
            inferred_hyard_job_count: 0,
            dirty: false,
            started_at: None,
        }
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.started_at.map(|t| t.elapsed())
    }

    pub fn elapsed_display(&self) -> String {
        match self.elapsed() {
            Some(d) => format!("{:.1}s", d.as_secs_f64()),
            None => String::new(),
        }
    }

    pub fn push_event(&mut self, desc: String) {
        let event = desc.clone();
        push_bounded(&mut self.event_log, event, MAX_EVENT_LOG);
        if is_hyard_event_text(&desc) {
            self.latest_hyard_event = Some(desc);
        }
        self.dirty = true;
    }

    pub fn push_stream_line(&mut self, line: String) {
        push_bounded(&mut self.stream_lines, line, MAX_STREAM_LINES);
        self.dirty = true;
    }

    pub fn push_raw_json(&mut self, line: String) {
        push_bounded(&mut self.raw_json_lines, line, MAX_RAW_JSON);
        self.dirty = true;
    }

    pub fn provider_view_ids(&self) -> Vec<String> {
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();
        let mut push_provider = |provider: &str| {
            let trimmed = provider.trim();
            if trimmed.is_empty() {
                return;
            }
            let provider = trimmed.to_string();
            if seen.insert(provider.clone()) {
                ordered.push(provider);
            }
        };

        push_provider(&self.current_provider);
        if let Some(peer) = &self.current_peer {
            push_provider(peer);
        }
        for provider in &self.provider_view_order {
            push_provider(provider);
        }
        for job in self.hyard_jobs.iter().filter(|job| job.is_active()) {
            push_provider(&job.provider);
        }

        ordered
    }

    pub fn provider_view_entries(&self, provider: &str) -> Vec<String> {
        self.provider_view_lines
            .get(provider)
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn provider_terminal_entries(&self, provider: &str) -> Vec<String> {
        self.provider_terminal_lines
            .get(provider)
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn provider_screen_entries(&self, provider: &str, max_lines: usize) -> Vec<String> {
        self.provider_screen_buffers
            .get(provider)
            .map(|screen| screen.visible_lines(max_lines))
            .unwrap_or_default()
    }

    pub fn provider_screen_rendered_lines(
        &self,
        provider: &str,
        max_lines: usize,
    ) -> Vec<Line<'static>> {
        self.provider_screen_buffers
            .get(provider)
            .map(|screen| screen.rendered_lines(max_lines))
            .unwrap_or_default()
    }

    pub fn provider_terminal_transport(&self, provider: &str) -> Option<&str> {
        self.provider_terminal_transport
            .get(provider)
            .map(|value| value.as_str())
    }

    pub fn provider_has_activity(&self, provider: &str) -> bool {
        self.provider_view_lines
            .get(provider)
            .is_some_and(|lines| !lines.is_empty())
            || self
                .provider_screen_buffers
                .get(provider)
                .is_some_and(|screen| !screen.is_empty())
            || self
                .provider_terminal_lines
                .get(provider)
                .is_some_and(|lines| !lines.is_empty())
            || self
                .hyard_jobs
                .iter()
                .any(|job| job.provider == provider && job.is_active())
    }

    pub fn set_core_host_surface_probe(&mut self, probe: &HostSurfaceProbe) {
        self.core_host_surface = HostSurfaceState::from_probe(probe);
        self.dirty = true;
    }

    pub fn set_peer_host_surface_probe(&mut self, probe: Option<&HostSurfaceProbe>) {
        self.peer_host_surface = probe.map(HostSurfaceState::from_probe);
        self.dirty = true;
    }

    pub fn set_peer_probe_summary(&mut self, ready: usize, total: usize, done: bool) {
        self.peer_ready_count = ready;
        self.peer_total_count = total;
        self.peer_probe_done = done;
        self.dirty = true;
    }

    pub fn set_hyard_jobs(&mut self, jobs: Vec<HyardJobSummary>) {
        self.persisted_hyard_jobs = jobs;
        self.rebuild_hyard_jobs(true);
    }

    pub fn primary_hyard_job(&self) -> Option<&HyardJobSummary> {
        self.hyard_jobs
            .iter()
            .find(|job| job.is_active() && job.is_authoritative())
            .or_else(|| self.hyard_jobs.iter().find(|job| job.is_authoritative()))
            .or_else(|| self.hyard_jobs.iter().find(|job| job.is_active()))
            .or_else(|| self.hyard_jobs.first())
    }

    fn upsert_live_hyard_job(&mut self, job: HyardJobSummary) {
        if let Some(existing) = self.live_hyard_jobs.iter().position(|existing| {
            existing.job_id == job.job_id
                || (existing.provider == job.provider && existing.job_id.starts_with("live-"))
        }) {
            self.live_hyard_jobs[existing] = job;
        } else {
            self.live_hyard_jobs.push(job);
        }
        self.rebuild_hyard_jobs(true);
    }

    fn update_live_hyard_job<F>(&mut self, provider: &str, update: F)
    where
        F: FnOnce(Option<&HyardJobSummary>) -> HyardJobSummary,
    {
        let existing = self
            .live_hyard_jobs
            .iter()
            .find(|job| job.provider == provider && !job.is_authoritative())
            .cloned();
        self.upsert_live_hyard_job(update(existing.as_ref()));
    }

    fn clear_live_hyard_jobs(&mut self) {
        if !self.live_hyard_jobs.is_empty() {
            self.live_hyard_jobs.clear();
            self.rebuild_hyard_jobs(true);
        }
    }

    fn rebuild_hyard_jobs(&mut self, mark_dirty: bool) {
        let mut by_id: HashMap<String, HyardJobSummary> = HashMap::new();
        for job in &self.persisted_hyard_jobs {
            by_id.insert(job.job_id.clone(), job.clone());
        }
        for job in &self.live_hyard_jobs {
            match by_id.get(&job.job_id) {
                Some(existing) if existing.updated_at > job.updated_at => {}
                _ => {
                    by_id.insert(job.job_id.clone(), job.clone());
                }
            }
        }

        let mut merged: Vec<HyardJobSummary> = by_id.into_values().collect();
        merged.sort_by(|left, right| {
            right
                .is_active()
                .cmp(&left.is_active())
                .then_with(|| right.source.rank().cmp(&left.source.rank()))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.provider.cmp(&right.provider))
        });

        let active_count = merged
            .iter()
            .filter(|job| job.is_active() && job.is_authoritative())
            .count();
        let waiting_count = merged
            .iter()
            .filter(|job| job.is_active() && job.is_authoritative() && job.wait_timeout_count > 0)
            .count();
        let inferred_count = merged
            .iter()
            .filter(|job| job.is_active() && !job.is_authoritative())
            .count();
        let changed = self.hyard_jobs != merged
            || self.active_hyard_job_count != active_count
            || self.waiting_hyard_job_count != waiting_count
            || self.inferred_hyard_job_count != inferred_count;

        self.hyard_jobs = merged;
        self.active_hyard_job_count = active_count;
        self.waiting_hyard_job_count = waiting_count;
        self.inferred_hyard_job_count = inferred_count;
        if changed && mark_dirty {
            self.dirty = true;
        }
    }

    pub fn active_host_surface(&self) -> &HostSurfaceState {
        self.current_peer
            .as_ref()
            .and(self.peer_host_surface.as_ref())
            .unwrap_or(&self.core_host_surface)
    }

    pub fn clear_peer_host_surface(&mut self) {
        self.peer_host_surface = None;
        self.dirty = true;
    }

    pub fn active_execution(&self) -> Option<&ExecutionTelemetry> {
        self.current_peer
            .as_ref()
            .and(self.peer_execution.as_ref())
            .or(self.core_execution.as_ref())
    }

    pub fn active_turn_ids(&self) -> Vec<Uuid> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        if let Some(turn_id) = self.current_turn_id
            && seen.insert(turn_id)
        {
            ids.push(turn_id);
        }
        for turn_id in self.provider_turn_ids.values().copied() {
            if seen.insert(turn_id) {
                ids.push(turn_id);
            }
        }
        ids
    }

    pub fn resize_active_provider_screens(&mut self, rows: u16, cols: u16) {
        for provider in self.provider_turn_ids.keys().cloned().collect::<Vec<_>>() {
            self.provider_screen_buffers
                .entry(provider)
                .or_default()
                .resize(usize::from(rows), usize::from(cols));
        }

        if let Some(execution) = self.core_execution.as_mut() {
            execution.terminal_rows = Some(rows);
            execution.terminal_cols = Some(cols);
        }
        if let Some(execution) = self.peer_execution.as_mut() {
            execution.terminal_rows = Some(rows);
            execution.terminal_cols = Some(cols);
        }
        self.dirty = true;
    }

    fn reset_provider_views(&mut self, provider: &str) {
        self.provider_view_lines.clear();
        self.provider_terminal_lines.clear();
        self.provider_screen_buffers.clear();
        self.provider_terminal_transport.clear();
        self.provider_view_order.clear();
        self.ensure_provider_view(provider);
    }

    fn ensure_provider_view(&mut self, provider: &str) {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            return;
        }
        if !self
            .provider_view_order
            .iter()
            .any(|existing| existing == trimmed)
        {
            self.provider_view_order.push(trimmed.to_string());
        }
        self.provider_view_lines
            .entry(trimmed.to_string())
            .or_default();
        self.provider_terminal_lines
            .entry(trimmed.to_string())
            .or_default();
        self.provider_screen_buffers
            .entry(trimmed.to_string())
            .or_default();
    }

    fn clear_provider_view(&mut self, provider: &str) {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            return;
        }
        self.ensure_provider_view(trimmed);
        self.provider_view_lines
            .insert(trimmed.to_string(), VecDeque::new());
        self.provider_terminal_lines
            .insert(trimmed.to_string(), VecDeque::new());
        self.provider_screen_buffers
            .insert(trimmed.to_string(), TerminalScreenBuffer::default());
        self.provider_terminal_transport.remove(trimmed);
    }

    fn push_provider_view_line(&mut self, provider: &str, line: String) {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            return;
        }

        self.ensure_provider_view(trimmed);
        let lines = self
            .provider_view_lines
            .entry(trimmed.to_string())
            .or_default();
        if lines.back().is_some_and(|existing| existing == &line) {
            return;
        }
        push_bounded(lines, line, MAX_PROVIDER_VIEW_LINES);
        self.dirty = true;
    }

    fn push_provider_terminal_line(
        &mut self,
        provider: &str,
        line: String,
        transport: Option<String>,
    ) {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            return;
        }

        self.ensure_provider_view(trimmed);
        if let Some(transport) = transport.filter(|value| !value.trim().is_empty()) {
            self.provider_terminal_transport
                .insert(trimmed.to_string(), transport);
        }
        let lines = self
            .provider_terminal_lines
            .entry(trimmed.to_string())
            .or_default();
        push_bounded(lines, line.clone(), MAX_PROVIDER_TERMINAL_LINES);
        let rows = self
            .core_execution
            .as_ref()
            .filter(|_| self.current_provider == trimmed)
            .and_then(|value| value.terminal_rows)
            .or_else(|| {
                self.peer_execution
                    .as_ref()
                    .filter(|_| self.current_peer.as_deref() == Some(trimmed))
                    .and_then(|value| value.terminal_rows)
            })
            .map(usize::from)
            .unwrap_or(40);
        let cols = self
            .core_execution
            .as_ref()
            .filter(|_| self.current_provider == trimmed)
            .and_then(|value| value.terminal_cols)
            .or_else(|| {
                self.peer_execution
                    .as_ref()
                    .filter(|_| self.current_peer.as_deref() == Some(trimmed))
                    .and_then(|value| value.terminal_cols)
            })
            .map(usize::from)
            .unwrap_or(120);
        let screen = self
            .provider_screen_buffers
            .entry(trimmed.to_string())
            .or_insert_with(|| TerminalScreenBuffer::new(rows, cols));
        screen.apply_text(&line);
        if let Some(transport) = self.provider_terminal_transport.get(trimmed).cloned() {
            if self.current_provider == trimmed
                && let Some(execution) = self.core_execution.as_mut()
            {
                execution.io_transport = Some(transport.clone());
            }
            if self.current_peer.as_deref() == Some(trimmed)
                && let Some(execution) = self.peer_execution.as_mut()
            {
                execution.io_transport = Some(transport);
            }
        }
        self.dirty = true;
    }

    fn resize_provider_screen_from_execution(
        &mut self,
        provider: &str,
        execution: &ExecutionTelemetry,
    ) {
        let rows = execution.terminal_rows.map(usize::from).unwrap_or(40);
        let cols = execution.terminal_cols.map(usize::from).unwrap_or(120);
        self.ensure_provider_view(provider);
        self.provider_screen_buffers
            .entry(provider.to_string())
            .or_insert_with(|| TerminalScreenBuffer::new(rows, cols))
            .resize(rows, cols);
    }

    fn push_execution_lines(&mut self, provider: &str, execution: &ExecutionTelemetry) {
        self.push_provider_view_line(
            provider,
            format!(
                "[exec] 原始命令: {}",
                truncate(&execution.original_command, 160)
            ),
        );
        self.push_provider_view_line(
            provider,
            format!(
                "[exec] 解析命令: {}",
                truncate(&execution.resolved_command, 160)
            ),
        );
        self.push_provider_view_line(
            provider,
            format!(
                "[exec] 实际执行: {}",
                truncate(&execution.actual_display, 180)
            ),
        );
        self.push_provider_view_line(
            provider,
            format!(
                "[exec] npm改写: {}",
                if execution.used_npm_wrapper_rewrite {
                    "是"
                } else {
                    "否"
                }
            ),
        );
        if let Some(node_path) = execution.node_path.as_deref() {
            self.push_provider_view_line(
                provider,
                format!("[exec] node 路径: {}", truncate(node_path, 160)),
            );
        }
        if let Some(js_entry) = execution.js_entry.as_deref() {
            self.push_provider_view_line(
                provider,
                format!("[exec] js 入口: {}", truncate(js_entry, 160)),
            );
        }
    }

    /// Apply a RuntimeEvent to update state.
    pub fn apply(&mut self, event: &switchyard_core::RuntimeEvent) {
        use switchyard_core::RuntimeEvent;
        match event {
            RuntimeEvent::CoreTurnStarted { turn_id, provider } => {
                self.clear_live_hyard_jobs();
                self.phase = Phase::CoreRunning;
                self.current_provider = provider.clone();
                self.current_turn_id = Some(*turn_id);
                self.provider_turn_ids.clear();
                self.provider_turn_ids.insert(provider.clone(), *turn_id);
                self.current_peer = None;
                self.peer_execution = None;
                self.core_execution = None;
                self.started_at = Some(std::time::Instant::now());
                self.stream_lines.clear();
                self.reset_provider_views(provider);
                self.push_provider_view_line(
                    provider,
                    format!(
                        "[system] 已开始处理 turn {}",
                        prefix_chars(&turn_id.to_string(), 8)
                    ),
                );
                self.push_event(format!("[core/{provider}] 已开始处理"));
            }
            RuntimeEvent::CoreExecutionTelemetry { execution, .. } => {
                self.core_execution = Some(execution.clone());
                self.resize_provider_screen_from_execution(
                    &self.current_provider.clone(),
                    execution,
                );
                self.push_execution_lines(&self.current_provider.clone(), execution);
                self.push_event(format!("[exec/core] {}", format_execution_brief(execution)));
            }
            RuntimeEvent::CoreItemUpdated { provider, text, .. } => {
                // Streaming text → stream pane + raw pane.
                self.push_stream_line(text.clone());
                self.push_raw_json(format!("[core/{provider}] {text}"));
                self.push_provider_view_line(provider, text.clone());
                if text.trim_start().starts_with("[hyard]") {
                    self.push_event(text.clone());
                }
            }
            RuntimeEvent::CoreTerminalOutput {
                provider,
                text,
                transport,
                ..
            } => {
                self.push_provider_terminal_line(provider, text.clone(), transport.clone());
            }
            RuntimeEvent::CoreOutputCompleted { provider, .. } => {
                self.phase = Phase::Committing;
                self.push_provider_view_line(provider, "[system] 输出完成，正在收尾".to_string());
                self.push_event(format!("[core/{provider}] 输出完成，正在收尾"));
            }
            RuntimeEvent::DelegateRequested {
                core_turn_id,
                peer,
                role,
                task_summary,
                ..
            } => {
                self.phase = Phase::DelegateRequested;
                self.current_peer = Some(peer.clone());
                self.peer_execution = None;
                self.clear_provider_view(peer);
                self.push_provider_view_line(
                    &self.current_provider.clone(),
                    format!(
                        "[hyard] 已委托给 {peer} ({role})：{}",
                        truncate(task_summary, 120)
                    ),
                );
                self.push_provider_view_line(
                    peer,
                    format!(
                        "[hyard] 收到委托：角色={role} | {}",
                        truncate(task_summary, 160)
                    ),
                );
                self.update_live_hyard_job(peer, |existing| HyardJobSummary {
                    job_id: existing
                        .map(|job| job.job_id.clone())
                        .unwrap_or_else(|| format!("live-{core_turn_id}")),
                    provider: peer.clone(),
                    status: "queued".to_string(),
                    last_event: Some("delegate_requested".to_string()),
                    last_output_preview: Some(truncate(task_summary, 80)),
                    execution: None,
                    wait_timeout_count: 0,
                    artifact_count: 0,
                    result_ready: false,
                    error: None,
                    updated_at: format!("live:{core_turn_id}"),
                    source: HyardJobSource::Inferred,
                });
                self.push_event(format!(
                    "[hyard] 已请求委托 -> {peer} ({role})：{}",
                    truncate(task_summary, 50)
                ));
            }
            RuntimeEvent::PeerTurnStarted { provider, turn_id } => {
                self.phase = Phase::PeerRunning;
                self.provider_turn_ids.insert(provider.clone(), *turn_id);
                self.ensure_provider_view(provider);
                self.push_provider_view_line(provider, "[system] 已开始执行".to_string());
                self.update_live_hyard_job(provider, |existing| {
                    let mut job = existing.cloned().unwrap_or_else(|| HyardJobSummary {
                        job_id: format!("live-peer-{provider}"),
                        provider: provider.clone(),
                        status: "running".to_string(),
                        last_event: None,
                        last_output_preview: None,
                        execution: None,
                        wait_timeout_count: 0,
                        artifact_count: 0,
                        result_ready: false,
                        error: None,
                        updated_at: format!("live:{provider}"),
                        source: HyardJobSource::Inferred,
                    });
                    job.status = "running".to_string();
                    job.last_event = Some("peer_turn_started".to_string());
                    job.updated_at = format!("live:{provider}:started");
                    job
                });
                self.push_event(format!("[peer/{provider}] 已开始执行"));
            }
            RuntimeEvent::PeerExecutionTelemetry {
                provider,
                execution,
                ..
            } => {
                self.peer_execution = Some(execution.clone());
                self.resize_provider_screen_from_execution(provider, execution);
                self.push_execution_lines(provider, execution);
                self.update_live_hyard_job(provider, |existing| {
                    let mut job = existing.cloned().unwrap_or_else(|| HyardJobSummary {
                        job_id: format!("live-peer-{provider}"),
                        provider: provider.clone(),
                        status: "running".to_string(),
                        last_event: None,
                        last_output_preview: None,
                        execution: None,
                        wait_timeout_count: 0,
                        artifact_count: 0,
                        result_ready: false,
                        error: None,
                        updated_at: format!("live:{provider}"),
                        source: HyardJobSource::Inferred,
                    });
                    job.status = "running".to_string();
                    job.execution = Some(execution.clone());
                    job.last_event = Some("execution_resolved".to_string());
                    job.updated_at = format!("live:{provider}:exec");
                    job
                });
                self.push_event(format!("[exec/peer] {}", format_execution_brief(execution)));
            }
            RuntimeEvent::PeerItemUpdated { provider, text, .. } => {
                // Streaming text → stream pane + raw pane.
                self.push_stream_line(text.clone());
                self.push_raw_json(format!("[peer/{provider}] {text}"));
                self.push_provider_view_line(provider, text.clone());
                self.update_live_hyard_job(provider, |existing| {
                    let mut job = existing.cloned().unwrap_or_else(|| HyardJobSummary {
                        job_id: format!("live-peer-{provider}"),
                        provider: provider.clone(),
                        status: "running".to_string(),
                        last_event: None,
                        last_output_preview: None,
                        execution: None,
                        wait_timeout_count: 0,
                        artifact_count: 0,
                        result_ready: false,
                        error: None,
                        updated_at: format!("live:{provider}"),
                        source: HyardJobSource::Inferred,
                    });
                    job.status = "running".to_string();
                    job.last_event = Some("item_updated".to_string());
                    job.last_output_preview = Some(truncate(text, 80));
                    job.updated_at = format!("live:{provider}:item");
                    job
                });
                if text.trim_start().starts_with("[hyard]") {
                    self.push_event(text.clone());
                }
            }
            RuntimeEvent::PeerTerminalOutput {
                provider,
                text,
                transport,
                ..
            } => {
                self.push_provider_terminal_line(provider, text.clone(), transport.clone());
            }
            RuntimeEvent::PeerOutputCompleted { provider, .. } => {
                self.provider_turn_ids.remove(provider);
                self.push_provider_view_line(provider, "[system] 输出完成".to_string());
                self.update_live_hyard_job(provider, |existing| {
                    let mut job = existing.cloned().unwrap_or_else(|| HyardJobSummary {
                        job_id: format!("live-peer-{provider}"),
                        provider: provider.clone(),
                        status: "running".to_string(),
                        last_event: None,
                        last_output_preview: None,
                        execution: None,
                        wait_timeout_count: 0,
                        artifact_count: 0,
                        result_ready: false,
                        error: None,
                        updated_at: format!("live:{provider}"),
                        source: HyardJobSource::Inferred,
                    });
                    job.last_event = Some("output_completed".to_string());
                    job.updated_at = format!("live:{provider}:output_completed");
                    job
                });
                self.push_event(format!("[peer/{provider}] 输出完成"));
            }
            RuntimeEvent::DelegateCompleted {
                peer,
                status,
                summary,
                ..
            } => {
                self.push_provider_view_line(peer, format!("[result] 委托结束 -> {status}"));
                if let Some(summary) = summary.as_deref() {
                    self.push_provider_view_line(
                        peer,
                        format!("[summary] {}", truncate(summary, 180)),
                    );
                }
                self.update_live_hyard_job(peer, |existing| {
                    let mut job = existing.cloned().unwrap_or_else(|| HyardJobSummary {
                        job_id: format!("live-peer-{peer}"),
                        provider: peer.clone(),
                        status: "queued".to_string(),
                        last_event: None,
                        last_output_preview: None,
                        execution: None,
                        wait_timeout_count: 0,
                        artifact_count: 0,
                        result_ready: false,
                        error: None,
                        updated_at: format!("live:{peer}"),
                        source: HyardJobSource::Inferred,
                    });
                    job.status = match status.as_str() {
                        "success" => "completed".to_string(),
                        "cancelled" => "cancelled".to_string(),
                        "timeout" => "failed".to_string(),
                        "failed" => "failed".to_string(),
                        other => other.to_string(),
                    };
                    job.last_event = Some(format!("delegate_completed:{status}"));
                    if status == "timeout" {
                        job.error = Some("delegate timeout".to_string());
                    } else if status == "failed" && job.error.is_none() {
                        job.error = Some("delegate failed".to_string());
                    }
                    job.result_ready = status == "success";
                    job.updated_at = format!("live:{peer}:delegate_completed");
                    job
                });
                self.push_event(format!("[hyard] 委托 {peer} -> {status}"));
            }
            RuntimeEvent::HyardJobObserved {
                source_provider,
                observed_at,
                job,
            } => {
                self.ensure_provider_view(&job.provider);
                self.push_provider_view_line(
                    &job.provider,
                    format!("[hyard] 任务状态 -> {} ({})", job.status, source_provider),
                );
                if let Some(preview) = job.last_output_preview.as_deref() {
                    self.push_provider_view_line(
                        &job.provider,
                        format!("[preview] {}", truncate(preview, 180)),
                    );
                }
                if let Some(execution) = job.execution.as_ref() {
                    self.push_execution_lines(&job.provider, execution);
                }
                self.upsert_live_hyard_job(HyardJobSummary {
                    job_id: job.job_id.clone(),
                    provider: job.provider.clone(),
                    status: job.status.clone(),
                    last_event: job.last_event.clone(),
                    last_output_preview: job.last_output_preview.clone(),
                    execution: job.execution.clone(),
                    wait_timeout_count: job.wait_timeout_count,
                    artifact_count: job.artifact_count,
                    result_ready: job.result_ready,
                    error: job.error.clone(),
                    updated_at: observed_at.clone(),
                    source: HyardJobSource::Live,
                });
                self.push_event(format!(
                    "[hyard] {} 任务状态 -> {} ({})",
                    job.provider, job.status, source_provider
                ));
            }
            RuntimeEvent::FinalizationStarted { provider, turn_id } => {
                self.phase = Phase::Finalizing;
                self.current_provider = provider.clone();
                self.current_turn_id = Some(*turn_id);
                self.provider_turn_ids.insert(provider.clone(), *turn_id);
                self.current_peer = None;
                self.peer_execution = None;
                self.push_provider_view_line(provider, "[system] 已开始收尾".to_string());
                self.push_event(format!("[core/{provider}] 已开始收尾"));
            }
            RuntimeEvent::TurnCompleted {
                provider, turn_id, ..
            } => {
                self.phase = Phase::Idle;
                self.provider_turn_ids.remove(provider);
                if self.current_turn_id == Some(*turn_id) {
                    self.current_turn_id = None;
                }
                self.current_peer = None;
                self.peer_execution = None;
                self.started_at = None;
                self.push_provider_view_line(provider, "[result] 已完成".to_string());
                self.push_event(format!("[core/{provider}] 已完成"));
            }
            RuntimeEvent::TurnFailed {
                provider,
                turn_id,
                error,
            } => {
                self.phase = Phase::Idle;
                self.provider_turn_ids.remove(provider);
                if self.current_turn_id == Some(*turn_id) {
                    self.current_turn_id = None;
                }
                self.current_peer = None;
                self.peer_execution = None;
                self.started_at = None;
                self.push_provider_view_line(
                    provider,
                    format!("[result] 失败：{}", truncate(error, 160)),
                );
                self.push_event(format!("[core/{provider}] 失败：{}", truncate(error, 60)));
            }
        }
        self.dirty = true;
    }
}

pub fn host_surface_label(kind: HostSurfaceKind) -> &'static str {
    match kind {
        HostSurfaceKind::NativeSlash => "原生斜杠命令",
        HostSurfaceKind::NativeCustomCommand => "原生命令",
        HostSurfaceKind::Skill => "skill",
        HostSurfaceKind::Plugin => "plugin",
        HostSurfaceKind::ShellFallback => "shell 回退",
        HostSurfaceKind::Unknown => "未知",
    }
}

pub fn is_hyard_event_text(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[hyard]")
        || trimmed.starts_with("[hyard/")
        || trimmed.contains(" host surface")
}

fn summarize_host_surface_probe(probe: &HostSurfaceProbe) -> Option<String> {
    let mut parts = Vec::new();

    if probe.is_ready() {
        parts.push("ready".to_string());
    } else {
        if !probe.installed {
            parts.push("not installed".to_string());
        } else if !probe.configured {
            parts.push("needs config".to_string());
        }
        if !probe.discoverable {
            parts.push("not discoverable".to_string());
        }
    }

    for note in probe.notes.iter().take(2) {
        let compact = compact_host_surface_note(note);
        if !compact.is_empty() && !parts.iter().any(|existing| existing == &compact) {
            parts.push(compact);
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn format_execution_brief(execution: &ExecutionTelemetry) -> String {
    let base = if execution.used_npm_wrapper_rewrite {
        format!(
            "{} -> {} (npm→node)",
            truncate_path_hint(&execution.resolved_command, 28),
            truncate_path_hint(
                execution
                    .js_entry
                    .as_deref()
                    .unwrap_or(&execution.actual_command),
                32
            )
        )
    } else {
        format!(
            "{} -> {}",
            truncate_path_hint(&execution.original_command, 18),
            truncate_path_hint(&execution.actual_command, 32)
        )
    };

    match execution.io_transport.as_deref() {
        Some(transport) => format!("[{}] {base}", transport.to_uppercase()),
        None => base,
    }
}

fn truncate_path_hint(path: &str, max_chars: usize) -> String {
    let value = compact_path_hint(path);
    truncate_path(&value, max_chars)
}

fn compact_path_hint(path: &str) -> String {
    let value = path.trim();
    if value.contains('\\') || value.contains('/') {
        value
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(value)
            .to_string()
    } else {
        value.to_string()
    }
}

fn truncate_path(path: &str, max_chars: usize) -> String {
    preview_chars(path.trim(), max_chars, "...")
}

fn compact_host_surface_note(note: &str) -> String {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    trimmed
        .split_once(": ")
        .map(|(head, _)| head.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    preview_chars(s, max, "...")
}

/// Push a string into a bounded VecDeque, truncating entries that exceed MAX_ENTRY_LEN.
fn push_bounded(deque: &mut VecDeque<String>, entry: String, max_len: usize) {
    if deque.len() >= max_len {
        deque.pop_front();
    }
    if entry.len() > MAX_ENTRY_LEN {
        deque.push_back(format!("{}...", prefix_bytes(&entry, MAX_ENTRY_LEN)));
    } else {
        deque.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_core::RuntimeEvent;
    use switchyard_provider_api::{
        ExecutionTelemetry, HostSurfaceKind, HostSurfaceProbe, HyardJobObservation,
    };
    use uuid::Uuid;

    fn new_state() -> RuntimeState {
        RuntimeState::new("test-provider")
    }

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
    fn truncate_is_utf8_safe_for_multibyte_text() {
        let text = "这是一次通过 hyard/Switchyard 发起的最小连通性测试，请只回复三项内容。";
        let truncated = truncate(text, 20);

        assert!(truncated.starts_with("这是一次通过"));
        assert!(truncated.ends_with("..."));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn set_hyard_jobs_updates_counts_and_primary_job() {
        let mut state = new_state();
        state.set_hyard_jobs(vec![
            HyardJobSummary {
                job_id: "job-1".to_string(),
                provider: "claude".to_string(),
                status: "completed".to_string(),
                last_event: None,
                last_output_preview: None,
                execution: None,
                wait_timeout_count: 0,
                artifact_count: 0,
                result_ready: true,
                error: None,
                updated_at: "2026-04-04T12:00:00Z".to_string(),
                source: HyardJobSource::Store,
            },
            HyardJobSummary {
                job_id: "job-2".to_string(),
                provider: "codex".to_string(),
                status: "running".to_string(),
                last_event: Some("item_updated:codex".to_string()),
                last_output_preview: Some("working".to_string()),
                execution: None,
                wait_timeout_count: 1,
                artifact_count: 0,
                result_ready: false,
                error: None,
                updated_at: "2026-04-04T12:01:00Z".to_string(),
                source: HyardJobSource::Store,
            },
        ]);

        assert_eq!(state.active_hyard_job_count, 1);
        assert_eq!(state.waiting_hyard_job_count, 1);
        assert_eq!(state.inferred_hyard_job_count, 0);
        assert_eq!(
            state.primary_hyard_job().map(|job| job.job_id.as_str()),
            Some("job-2")
        );
    }

    #[test]
    fn hyard_job_observed_creates_live_job_and_beats_persisted_fallback() {
        let mut state = new_state();
        state.set_hyard_jobs(vec![HyardJobSummary {
            job_id: "persisted-job".to_string(),
            provider: "claude".to_string(),
            status: "running".to_string(),
            last_event: Some("worker_booting".to_string()),
            last_output_preview: None,
            execution: None,
            wait_timeout_count: 1,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }]);

        state.apply(&RuntimeEvent::HyardJobObserved {
            source_provider: "codex".to_string(),
            observed_at: "2026-04-04T12:05:00Z".to_string(),
            job: HyardJobObservation {
                job_id: "real-job".to_string(),
                provider: "gemini".to_string(),
                status: "running".to_string(),
                bridge_status: "wait_timeout".to_string(),
                last_event: Some("item_updated:gemini".to_string()),
                last_output_preview: Some("researching".to_string()),
                execution: None,
                wait_timeout_count: 2,
                artifact_count: 0,
                result_ready: false,
                error: None,
            },
        });

        assert_eq!(state.live_hyard_jobs.len(), 1);
        assert_eq!(state.active_hyard_job_count, 2);
        assert_eq!(state.waiting_hyard_job_count, 2);
        assert_eq!(state.inferred_hyard_job_count, 0);
        assert_eq!(
            state.primary_hyard_job().map(|job| job.job_id.as_str()),
            Some("real-job")
        );
        assert_eq!(
            state.primary_hyard_job().map(|job| job.source),
            Some(HyardJobSource::Live)
        );
    }

    #[test]
    fn host_surface_state_from_probe_uses_canonical_kind_and_compact_notes() {
        let probe = HostSurfaceProbe {
            kind: HostSurfaceKind::Skill,
            installed: true,
            configured: false,
            discoverable: false,
            notes: vec![
                "missing Codex AGENTS file: C:\\Users\\demo\\.codex\\AGENTS.md".to_string(),
                "codex plugins feature is disabled in this install; keep HYARD on skill/shell fallback.".to_string(),
            ],
        };

        let state = HostSurfaceState::from_probe(&probe);

        assert_eq!(state.kind, HostSurfaceKind::Skill);
        assert!(state.probed);
        assert!(state.installed);
        assert!(!state.configured);
        assert!(!state.discoverable);
        assert_eq!(state.readiness(), HostSurfaceReadiness::Partial);
        assert_eq!(
            state.notes.as_deref(),
            Some(
                "needs config; not discoverable; missing Codex AGENTS file; codex plugins feature is disabled in this install; keep HYARD on skill/shell fallback."
            )
        );
        assert_eq!(state.label(), "skill");
    }

    #[test]
    fn active_host_surface_prefers_peer_when_present() {
        let mut state = new_state();
        let core = HostSurfaceProbe::ready(HostSurfaceKind::Skill);
        let peer = HostSurfaceProbe::ready(HostSurfaceKind::NativeSlash);

        state.set_core_host_surface_probe(&core);
        assert_eq!(state.active_host_surface().kind, HostSurfaceKind::Skill);

        state.current_peer = Some("claude".to_string());
        state.set_peer_host_surface_probe(Some(&peer));
        assert_eq!(
            state.active_host_surface().kind,
            HostSurfaceKind::NativeSlash
        );

        state.current_peer = None;
        assert_eq!(state.active_host_surface().kind, HostSurfaceKind::Skill);
    }

    #[test]
    fn host_surface_readiness_distinguishes_unknown_ready_partial_and_unavailable() {
        let state = HostSurfaceState::new(HostSurfaceKind::Unknown);
        assert_eq!(state.readiness(), HostSurfaceReadiness::Unknown);

        let ready = HostSurfaceState::from_probe(&HostSurfaceProbe::ready(HostSurfaceKind::Skill));
        assert_eq!(ready.readiness(), HostSurfaceReadiness::Ready);

        let partial = HostSurfaceState::from_probe(&HostSurfaceProbe {
            kind: HostSurfaceKind::Plugin,
            installed: true,
            configured: false,
            discoverable: true,
            notes: vec!["needs config".to_string()],
        });
        assert_eq!(partial.readiness(), HostSurfaceReadiness::Partial);

        let missing = HostSurfaceState::from_probe(&HostSurfaceProbe::unavailable(vec![
            "not installed".to_string(),
        ]));
        assert_eq!(missing.readiness(), HostSurfaceReadiness::Unavailable);
        assert_eq!(missing.readiness().label(), "缺失");
    }

    #[test]
    fn push_event_only_updates_latest_hyard_for_hyard_events() {
        let mut state = new_state();

        state.push_event("[core/codex] turn started".to_string());
        assert!(state.latest_hyard_event.is_none());

        state.push_event("[hyard] delegate -> claude (reviewer): review".to_string());
        assert_eq!(
            state.latest_hyard_event.as_deref(),
            Some("[hyard] delegate -> claude (reviewer): review")
        );

        state.push_event("[core/codex] output complete".to_string());
        assert_eq!(
            state.latest_hyard_event.as_deref(),
            Some("[hyard] delegate -> claude (reviewer): review")
        );
    }

    #[test]
    fn set_peer_probe_summary_updates_counts_and_done_flag() {
        let mut state = new_state();
        state.set_peer_probe_summary(2, 3, true);

        assert_eq!(state.peer_ready_count, 2);
        assert_eq!(state.peer_total_count, 3);
        assert!(state.peer_probe_done);
    }

    // ── CoreTurnStarted ──

    #[test]
    fn core_turn_started_sets_phase_and_provider() {
        let mut state = new_state();
        let turn_id = Uuid::now_v7();

        state.apply(&RuntimeEvent::CoreTurnStarted {
            turn_id,
            provider: "claude".to_string(),
        });

        assert_eq!(state.phase, Phase::CoreRunning);
        assert_eq!(state.current_provider, "claude");
        assert_eq!(state.current_turn_id, Some(turn_id));
        assert!(state.current_peer.is_none());
        assert!(state.started_at.is_some());
        assert!(state.dirty);
    }

    #[test]
    fn core_turn_started_clears_stream_lines() {
        let mut state = new_state();
        state.push_stream_line("old line".to_string());
        assert!(!state.stream_lines.is_empty());

        state.apply(&RuntimeEvent::CoreTurnStarted {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
        });

        assert!(state.stream_lines.is_empty());
    }

    #[test]
    fn execution_telemetry_updates_active_execution_and_log() {
        let mut state = new_state();
        let execution = sample_execution();

        state.apply(&RuntimeEvent::CoreTurnStarted {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
        });
        state.apply(&RuntimeEvent::CoreExecutionTelemetry {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
            execution: execution.clone(),
        });

        assert_eq!(state.core_execution.as_ref(), Some(&execution));
        assert_eq!(state.active_execution(), Some(&execution));
        assert!(
            state
                .event_log
                .back()
                .is_some_and(|line| line.contains("[exec/core]"))
        );

        state.apply(&RuntimeEvent::DelegateRequested {
            core_turn_id: Uuid::now_v7(),
            peer: "gemini".to_string(),
            role: "researcher".to_string(),
            task_summary: "inspect".to_string(),
        });
        state.apply(&RuntimeEvent::PeerExecutionTelemetry {
            turn_id: Uuid::now_v7(),
            provider: "gemini".to_string(),
            execution: execution.clone(),
        });

        assert_eq!(state.peer_execution.as_ref(), Some(&execution));
        assert_eq!(state.active_execution(), Some(&execution));

        state.apply(&RuntimeEvent::FinalizationStarted {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
        });

        assert!(state.peer_execution.is_none());
        assert_eq!(state.active_execution(), Some(&execution));
    }

    // ── CoreItemUpdated ──

    #[test]
    fn core_item_updated_pushes_stream_and_raw_not_event_log() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::CoreItemUpdated {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
            text: "hello world".to_string(),
        });

        assert_eq!(state.stream_lines.len(), 1);
        assert_eq!(state.stream_lines[0], "hello world");
        assert!(!state.raw_json_lines.is_empty());
        // Streaming items no longer pollute the event log
        assert!(state.event_log.is_empty());
    }

    #[test]
    fn terminal_output_populates_provider_terminal_transcript() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::CoreTerminalOutput {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
            text: "Applying patch...".to_string(),
            transport: Some("pty".to_string()),
        });

        assert_eq!(
            state.provider_terminal_entries("codex"),
            vec!["Applying patch...".to_string()]
        );
        assert_eq!(state.provider_terminal_transport("codex"), Some("pty"));
        assert!(state.provider_has_activity("codex"));
    }

    // ── DelegateRequested ──

    #[test]
    fn delegate_requested_sets_phase_and_peer() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::DelegateRequested {
            core_turn_id: Uuid::now_v7(),
            peer: "gemini".to_string(),
            role: "reviewer".to_string(),
            task_summary: "Review code".to_string(),
        });

        assert_eq!(state.phase, Phase::DelegateRequested);
        assert_eq!(state.current_peer, Some("gemini".to_string()));
        assert_eq!(state.active_hyard_job_count, 0);
        assert_eq!(state.inferred_hyard_job_count, 1);
        assert_eq!(
            state.primary_hyard_job().map(|job| job.source),
            Some(HyardJobSource::Inferred)
        );
        assert!(
            state
                .event_log
                .back()
                .is_some_and(|line| line.starts_with("[hyard] 已请求委托 -> gemini"))
        );
    }

    #[test]
    fn authoritative_hyard_job_beats_inferred_placeholder() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::DelegateRequested {
            core_turn_id: Uuid::now_v7(),
            peer: "claude".to_string(),
            role: "researcher".to_string(),
            task_summary: "investigate".to_string(),
        });
        state.set_hyard_jobs(vec![HyardJobSummary {
            job_id: "job-real".to_string(),
            provider: "claude".to_string(),
            status: "running".to_string(),
            last_event: Some("item_updated:claude".to_string()),
            last_output_preview: Some("working".to_string()),
            execution: None,
            wait_timeout_count: 1,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }]);

        assert_eq!(state.active_hyard_job_count, 1);
        assert_eq!(state.inferred_hyard_job_count, 1);
        assert_eq!(
            state.primary_hyard_job().map(|job| job.job_id.as_str()),
            Some("job-real")
        );
        assert_eq!(
            state.primary_hyard_job().map(|job| job.source),
            Some(HyardJobSource::Store)
        );
    }

    #[test]
    fn peer_runtime_hints_do_not_overwrite_authoritative_live_job() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::HyardJobObserved {
            source_provider: "codex".to_string(),
            observed_at: "2026-04-04T12:05:00Z".to_string(),
            job: HyardJobObservation {
                job_id: "real-job".to_string(),
                provider: "gemini".to_string(),
                status: "running".to_string(),
                bridge_status: "wait_timeout".to_string(),
                last_event: Some("item_updated:gemini".to_string()),
                last_output_preview: Some("authoritative".to_string()),
                execution: None,
                wait_timeout_count: 2,
                artifact_count: 0,
                result_ready: false,
                error: None,
            },
        });

        state.apply(&RuntimeEvent::PeerItemUpdated {
            turn_id: Uuid::now_v7(),
            provider: "gemini".to_string(),
            text: "inferred text".to_string(),
        });

        assert_eq!(state.active_hyard_job_count, 1);
        assert_eq!(state.inferred_hyard_job_count, 1);
        assert_eq!(
            state.primary_hyard_job().map(|job| job.job_id.as_str()),
            Some("real-job")
        );
        assert_eq!(
            state
                .primary_hyard_job()
                .and_then(|job| job.last_output_preview.as_deref()),
            Some("authoritative")
        );
        assert_eq!(
            state.primary_hyard_job().map(|job| job.source),
            Some(HyardJobSource::Live)
        );
    }

    // ── PeerTurnStarted ──

    #[test]
    fn peer_turn_started_sets_phase() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::PeerTurnStarted {
            turn_id: Uuid::now_v7(),
            provider: "gemini".to_string(),
        });

        assert_eq!(state.phase, Phase::PeerRunning);
    }

    // ── PeerItemUpdated ──

    #[test]
    fn peer_item_updated_pushes_stream_and_raw_not_event_log() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::PeerItemUpdated {
            turn_id: Uuid::now_v7(),
            provider: "gemini".to_string(),
            text: "peer output".to_string(),
        });

        assert!(state.stream_lines.iter().any(|l| l == "peer output"));
        assert!(
            state
                .raw_json_lines
                .iter()
                .any(|l| l.contains("peer/gemini"))
        );
        assert!(state.event_log.is_empty());
    }

    // ── DelegateCompleted ──

    #[test]
    fn delegate_completed_logs_event() {
        let mut state = new_state();
        state.apply(&RuntimeEvent::DelegateCompleted {
            core_turn_id: Uuid::now_v7(),
            peer: "gemini".to_string(),
            status: "success".to_string(),
            summary: Some("all good".to_string()),
        });

        assert!(
            state
                .event_log
                .iter()
                .any(|e| e.contains("委托") && e.contains("gemini"))
        );
    }

    // ── CoreOutputCompleted ──

    #[test]
    fn core_output_completed_sets_committing() {
        let mut state = new_state();
        state.phase = Phase::CoreRunning;

        state.apply(&RuntimeEvent::CoreOutputCompleted {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
        });

        assert_eq!(state.phase, Phase::Committing);
        assert!(state.event_log.iter().any(|e| e.contains("输出完成")));
    }

    // ── PeerOutputCompleted ──

    #[test]
    fn peer_output_completed_logs_event() {
        let mut state = new_state();
        state.phase = Phase::PeerRunning;

        state.apply(&RuntimeEvent::PeerOutputCompleted {
            turn_id: Uuid::now_v7(),
            provider: "gemini".to_string(),
        });

        assert!(state.event_log.iter().any(|e| e.contains("输出完成")));
    }

    // ── Phase helpers ──

    #[test]
    fn phase_is_executing_and_is_busy() {
        assert!(!Phase::Idle.is_busy());
        assert!(!Phase::Idle.is_executing());

        assert!(Phase::CoreRunning.is_executing());
        assert!(Phase::CoreRunning.is_busy());

        assert!(!Phase::Committing.is_executing());
        assert!(Phase::Committing.is_busy());
    }

    // ── FinalizationStarted ──

    #[test]
    fn finalization_started_sets_phase_and_clears_peer() {
        let mut state = new_state();
        state.current_peer = Some("gemini".to_string());

        state.apply(&RuntimeEvent::FinalizationStarted {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
        });

        assert_eq!(state.phase, Phase::Finalizing);
        assert!(state.current_peer.is_none());
    }

    // ── TurnCompleted ──

    #[test]
    fn turn_completed_resets_to_idle() {
        let mut state = new_state();
        state.phase = Phase::CoreRunning;
        state.started_at = Some(std::time::Instant::now());
        state.current_peer = Some("gemini".to_string());

        state.apply(&RuntimeEvent::TurnCompleted {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
            response: Some("done".to_string()),
        });

        assert_eq!(state.phase, Phase::Idle);
        assert!(state.current_peer.is_none());
        assert!(state.started_at.is_none());
    }

    // ── TurnFailed ──

    #[test]
    fn turn_failed_resets_to_idle_with_error() {
        let mut state = new_state();
        state.phase = Phase::CoreRunning;
        state.started_at = Some(std::time::Instant::now());

        state.apply(&RuntimeEvent::TurnFailed {
            turn_id: Uuid::now_v7(),
            provider: "claude".to_string(),
            error: "crash".to_string(),
        });

        assert_eq!(state.phase, Phase::Idle);
        assert!(state.started_at.is_none());
        assert!(state.event_log.iter().any(|e| e.contains("失败")));
    }

    // ── Full lifecycle ──

    #[test]
    fn full_delegate_lifecycle_phase_transitions() {
        let mut state = new_state();
        let core_turn = Uuid::now_v7();
        let peer_turn = Uuid::now_v7();

        // 1. Core starts
        state.apply(&RuntimeEvent::CoreTurnStarted {
            turn_id: core_turn,
            provider: "claude".to_string(),
        });
        assert_eq!(state.phase, Phase::CoreRunning);

        // 2. Core produces output
        state.apply(&RuntimeEvent::CoreItemUpdated {
            turn_id: core_turn,
            provider: "claude".to_string(),
            text: "thinking...".to_string(),
        });
        assert_eq!(state.phase, Phase::CoreRunning);

        // 3. Core output finishes
        state.apply(&RuntimeEvent::CoreOutputCompleted {
            turn_id: core_turn,
            provider: "claude".to_string(),
        });
        assert_eq!(state.phase, Phase::Committing);

        // 4. Core turn fully committed
        state.apply(&RuntimeEvent::TurnCompleted {
            turn_id: core_turn,
            provider: "claude".to_string(),
            response: Some("delegate block".to_string()),
        });
        assert_eq!(state.phase, Phase::Idle);

        // 5. Delegate requested
        state.apply(&RuntimeEvent::DelegateRequested {
            core_turn_id: core_turn,
            peer: "gemini".to_string(),
            role: "reviewer".to_string(),
            task_summary: "review".to_string(),
        });
        assert_eq!(state.phase, Phase::DelegateRequested);
        assert_eq!(state.current_peer.as_deref(), Some("gemini"));

        // 6. Peer starts
        state.apply(&RuntimeEvent::PeerTurnStarted {
            turn_id: peer_turn,
            provider: "gemini".to_string(),
        });
        assert_eq!(state.phase, Phase::PeerRunning);

        // 7. Peer produces output
        state.apply(&RuntimeEvent::PeerItemUpdated {
            turn_id: peer_turn,
            provider: "gemini".to_string(),
            text: "looks good".to_string(),
        });
        assert_eq!(state.phase, Phase::PeerRunning);

        // 8. Delegate completed
        state.apply(&RuntimeEvent::DelegateCompleted {
            core_turn_id: core_turn,
            peer: "gemini".to_string(),
            status: "success".to_string(),
            summary: Some("all good".to_string()),
        });

        // 9. Finalization starts
        let final_turn = Uuid::now_v7();
        state.apply(&RuntimeEvent::FinalizationStarted {
            turn_id: final_turn,
            provider: "claude".to_string(),
        });
        assert_eq!(state.phase, Phase::Finalizing);
        assert!(state.current_peer.is_none());

        // 10. Finalization output done
        state.apply(&RuntimeEvent::CoreOutputCompleted {
            turn_id: final_turn,
            provider: "claude".to_string(),
        });
        assert_eq!(state.phase, Phase::Committing);

        // 11. Final turn committed
        state.apply(&RuntimeEvent::TurnCompleted {
            turn_id: final_turn,
            provider: "claude".to_string(),
            response: Some("final answer".to_string()),
        });
        assert_eq!(state.phase, Phase::Idle);
    }

    // ── push_bounded ──

    #[test]
    fn push_bounded_truncates_long_entries() {
        let mut deque = VecDeque::new();
        let long_entry = "x".repeat(MAX_ENTRY_LEN + 100);

        push_bounded(&mut deque, long_entry, 10);

        assert_eq!(deque.len(), 1);
        assert!(deque[0].ends_with("..."));
        assert!(deque[0].len() <= MAX_ENTRY_LEN + 4); // +4 for "..."
    }

    #[test]
    fn push_bounded_evicts_oldest_when_full() {
        let mut deque = VecDeque::new();
        for i in 0..5 {
            push_bounded(&mut deque, format!("entry-{i}"), 3);
        }

        assert_eq!(deque.len(), 3);
        assert_eq!(deque[0], "entry-2");
        assert_eq!(deque[1], "entry-3");
        assert_eq!(deque[2], "entry-4");
    }

    // ── elapsed_display ──

    #[test]
    fn elapsed_display_empty_when_not_started() {
        let state = new_state();
        assert!(state.elapsed_display().is_empty());
    }

    #[test]
    fn elapsed_display_shows_seconds() {
        let mut state = new_state();
        state.started_at = Some(std::time::Instant::now());
        let display = state.elapsed_display();
        assert!(display.ends_with('s'), "should end with 's': {display}");
    }
}
