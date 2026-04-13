//! TUI application: event-driven loop with non-blocking turn execution.
//!
//! Turn execution is a pinned future polled alongside keyboard, heartbeat,
//! and runtime-event branches in a tokio::select! loop. The UI is never
//! blocked by a running turn.

use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use switchyard_core::{
    ProviderRegistry, RuntimeEvent, build_peer_catalog, build_peer_catalog_probed,
    run_routed_turn_observable,
};
use switchyard_provider_api::{HostSurfaceProbe, PeerCatalog};
use switchyard_provider_subprocess::resize_registered_pty;
use switchyard_session::Session;
use switchyard_store::{SessionRepository, StoreBackend, StoreHandle};
use switchyard_text::{prefix_chars, preview_chars};

use crate::hyard_jobs::read_hyard_job_summaries;
use crate::state::{
    HostSurfaceReadiness, HostSurfaceState, HyardJobSource, HyardJobSummary, Phase, RuntimeState,
    is_hyard_event_text,
};

struct TurnEntry {
    turn_id: Uuid,
    user_message: String,
    response: Option<String>,
    status: String,
    delegated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Input,
    Transcript,
    Sidebar,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Input => Self::Transcript,
            Self::Transcript => Self::Sidebar,
            Self::Sidebar => Self::Input,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Input => Self::Sidebar,
            Self::Transcript => Self::Input,
            Self::Sidebar => Self::Transcript,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Input => "输入框",
            Self::Transcript => "主消息区",
            Self::Sidebar => "右侧面板",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightPane {
    Events,
    RawStream,
    Artifacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MessageView {
    Overview,
    Provider(String),
    Hyard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ProviderPaneMode {
    Screen,
    Raw,
    Timeline,
}

impl ProviderPaneMode {
    fn all() -> [Self; 3] {
        [Self::Screen, Self::Raw, Self::Timeline]
    }

    fn index(self) -> usize {
        match self {
            Self::Screen => 0,
            Self::Raw => 1,
            Self::Timeline => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Screen => "屏幕",
            Self::Raw => "原始",
            Self::Timeline => "时间线",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Raw => "raw",
            Self::Timeline => "timeline",
        }
    }

    fn from_key(code: KeyCode) -> Option<Self> {
        match code {
            KeyCode::Char('s') | KeyCode::Char('S') => Some(Self::Screen),
            KeyCode::Char('r') | KeyCode::Char('R') => Some(Self::Raw),
            KeyCode::Char('t') | KeyCode::Char('T') => Some(Self::Timeline),
            _ => None,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Screen => Self::Raw,
            Self::Raw => Self::Timeline,
            Self::Timeline => Self::Screen,
        }
    }
}

impl RightPane {
    fn index(self) -> usize {
        match self {
            Self::Events => 0,
            Self::RawStream => 1,
            Self::Artifacts => 2,
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Events => "事件",
            Self::RawStream => "原始流",
            Self::Artifacts => "工件",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScrollState {
    offset: u16,
    follow_latest: bool,
    max_scroll: u16,
    has_unseen: bool,
}

impl ScrollState {
    const fn new() -> Self {
        Self {
            offset: 0,
            follow_latest: true,
            max_scroll: 0,
            has_unseen: false,
        }
    }

    fn sync(&mut self, max_scroll: u16) {
        if !self.follow_latest && max_scroll > self.max_scroll {
            self.has_unseen = true;
        }
        self.max_scroll = max_scroll;
        if self.follow_latest {
            self.offset = max_scroll;
            self.has_unseen = false;
        } else {
            self.offset = self.offset.min(max_scroll);
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let max_scroll = self.max_scroll as i32;
        let current = if self.follow_latest {
            max_scroll
        } else {
            self.offset.min(self.max_scroll) as i32
        };
        let next = (current + delta).clamp(0, max_scroll) as u16;
        self.offset = next;
        self.follow_latest = next >= self.max_scroll;
        if self.follow_latest {
            self.has_unseen = false;
        }
    }

    fn scroll_to_top(&mut self) {
        self.follow_latest = false;
        self.offset = 0;
    }

    fn scroll_to_latest(&mut self) {
        self.follow_latest = true;
        self.offset = self.max_scroll;
        self.has_unseen = false;
    }
}

const DEFAULT_SCROLL_STATE: ScrollState = ScrollState::new();

pub struct App {
    input: String,
    cursor: usize,
    turns: Vec<TurnEntry>,
    message_scrolls: HashMap<MessageView, ScrollState>,
    provider_mode_scrolls: HashMap<(String, ProviderPaneMode), ScrollState>,
    right_pane_scrolls: [ScrollState; 3],
    status: String,
    quit: bool,
    focus: Focus,
    right_pane: RightPane,
    message_view: MessageView,
    provider_view_modes: HashMap<String, ProviderPaneMode>,
    provider_name: String,
    store_backend: StoreBackend,
    store_path: PathBuf,
    job_dir: PathBuf,
    runtime: RuntimeState,
    /// Last submitted message, for retry.
    last_message: Option<String>,
    /// Collected artifact entries for the artifact panel.
    artifact_entries: Vec<ArtifactDisplayEntry>,
    /// Cached session id prefix for status display (avoids borrowing session during running).
    session_id_prefix: String,
    /// CancellationToken for the active turn (None when idle).
    active_cancel: Option<CancellationToken>,
    /// Clickable tab hitboxes from the latest frame.
    message_tab_hitboxes: Vec<(Rect, MessageView)>,
    /// Clickable per-provider mode tabs from the latest frame.
    provider_mode_hitboxes: Vec<(Rect, ProviderPaneMode)>,
    /// Latest message panel area.
    message_panel_area: Rect,
    /// Latest right sidebar column area.
    sidebar_area: Rect,
    /// Latest input box area.
    input_area: Rect,
    /// Latest left turn list area.
    turn_list_area: Rect,
    /// TUI resize needs to be pushed into active PTYs after the next draw.
    pending_terminal_resize: bool,
}

struct ArtifactDisplayEntry {
    turn_label: String,
    items: Vec<String>,
}

impl App {
    pub fn new(provider_name: String, session_dir: PathBuf) -> Self {
        let job_dir = session_dir
            .parent()
            .map(|parent| parent.join("jobs"))
            .unwrap_or_else(|| PathBuf::from(".switchyard").join("jobs"));
        Self::with_paths(provider_name, session_dir, job_dir)
    }

    pub fn with_paths(provider_name: String, session_dir: PathBuf, job_dir: PathBuf) -> Self {
        Self::with_store(provider_name, StoreBackend::Jsonl, session_dir, job_dir)
    }

    pub fn with_store(
        provider_name: String,
        store_backend: StoreBackend,
        store_path: PathBuf,
        job_dir: PathBuf,
    ) -> Self {
        let runtime = RuntimeState::new(&provider_name);
        Self {
            input: String::new(),
            cursor: 0,
            turns: Vec::new(),
            message_scrolls: HashMap::new(),
            provider_mode_scrolls: HashMap::new(),
            right_pane_scrolls: [ScrollState::new(); 3],
            status: format!("provider: {provider_name} | Enter 发送 | Ctrl-C 退出"),
            quit: false,
            focus: Focus::Input,
            right_pane: RightPane::Events,
            message_view: MessageView::Overview,
            provider_view_modes: HashMap::new(),
            provider_name,
            store_backend,
            store_path,
            job_dir,
            runtime,
            last_message: None,
            artifact_entries: Vec::new(),
            session_id_prefix: String::new(),
            active_cancel: None,
            message_tab_hitboxes: Vec::new(),
            provider_mode_hitboxes: Vec::new(),
            message_panel_area: Rect::new(0, 0, 0, 0),
            sidebar_area: Rect::new(0, 0, 0, 0),
            input_area: Rect::new(0, 0, 0, 0),
            turn_list_area: Rect::new(0, 0, 0, 0),
            pending_terminal_resize: false,
        }
    }

    pub async fn run(
        &mut self,
        registry: &ProviderRegistry,
        config: &switchyard_config::SwitchyardConfig,
    ) -> anyhow::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
        )?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let mut terminal = ratatui::Terminal::new(backend)?;

        let mut store = StoreHandle::open(self.store_backend, self.store_path.clone())?;
        let mut session = Session::new(self.provider_name.clone());
        store.save_session(&session)?;
        self.session_id_prefix = prefix_chars(&session.session_id.to_string(), 8);
        self.refresh_hyard_jobs();
        self.update_idle_status();

        let result = self
            .event_loop(&mut terminal, registry, config, &mut store, &mut session)
            .await;

        crossterm::terminal::disable_raw_mode()?;
        crossterm::execute!(
            terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
        )?;
        terminal.show_cursor()?;
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
        registry: &ProviderRegistry,
        config: &switchyard_config::SwitchyardConfig,
        store: &mut StoreHandle,
        session: &mut Session,
    ) -> anyhow::Result<()> {
        let (runtime_tx, mut runtime_rx) = mpsc::channel::<RuntimeEvent>(64);
        let mut status_tick = tokio::time::interval(tokio::time::Duration::from_millis(100));
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut hyard_job_tick = tokio::time::interval(tokio::time::Duration::from_millis(500));
        hyard_job_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut kbd_tick = tokio::time::interval(tokio::time::Duration::from_millis(16));
        kbd_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending_submit: Option<String> = None;

        // Start with a static catalog (instant, no subprocess calls).
        // Background probe upgrades it to real availability data.
        let provider_name_for_probe = self.provider_name.clone();
        let mut peer_catalog = build_peer_catalog(&provider_name_for_probe, registry);
        self.sync_peer_catalog_state(&peer_catalog, false);
        let probe_fut =
            build_peer_catalog_probed(&provider_name_for_probe, registry, &config.providers);
        let core_probe_fut =
            probe_core_host_surface(&provider_name_for_probe, registry, &config.providers);
        tokio::pin!(probe_fut);
        tokio::pin!(core_probe_fut);
        let mut probe_done = false;
        let mut core_probe_done = false;
        self.runtime
            .push_event("[hyard] probing peers in background".to_string());
        self.runtime
            .push_event("[hyard] probing core host surface in background".to_string());
        self.runtime.dirty = true;

        loop {
            if self.runtime.dirty {
                terminal.draw(|f| self.draw(f))?;
                self.runtime.dirty = false;
            }
            self.sync_pending_terminal_resize();

            if self.quit {
                if let Some(cancel) = self.active_cancel.take() {
                    cancel.cancel();
                }
                return Ok(());
            }

            // ── Submit: enter running phase ──
            if let Some(msg) = pending_submit.take() {
                self.prepare_turn(&msg);
                self.runtime.phase = Phase::Preparing;
                self.runtime.started_at = Some(std::time::Instant::now());
                self.runtime.push_event("[tui] preparing turn".to_string());
                terminal.draw(|f| self.draw(f))?;
                self.sync_pending_terminal_resize();

                // If probe hasn't finished yet, poll it now with UI alive
                if !probe_done {
                    self.runtime.phase = Phase::ProbingPeers;
                    self.runtime.push_event("[hyard] probing peers".to_string());
                    terminal.draw(|f| self.draw(f))?;
                    self.sync_pending_terminal_resize();

                    loop {
                        tokio::select! {
                            result = &mut probe_fut => {
                                peer_catalog = result;
                                probe_done = true;
                                self.sync_peer_catalog_state(&peer_catalog, true);
                                self.runtime.push_event(format!(
                                    "[hyard] peer probe complete: {}/{} ready",
                                    self.runtime.peer_ready_count,
                                    self.runtime.peer_total_count
                                ));
                                self.runtime.dirty = true;
                                break;
                            }
                            result = &mut core_probe_fut, if !core_probe_done => {
                                core_probe_done = true;
                                self.apply_core_host_surface_probe(result);
                            }
                            _ = status_tick.tick() => {
                                self.update_running_status();
                                self.runtime.dirty = true;
                            }
                            _ = hyard_job_tick.tick() => {
                                self.refresh_hyard_jobs();
                            }
                            _ = kbd_tick.tick() => {
                                while event::poll(std::time::Duration::ZERO)? {
                                    match event::read()? {
                                        Event::Key(key) => {
                                            if key.kind != KeyEventKind::Press { continue; }
                                            self.handle_key_running(key.code, key.modifiers);
                                            self.runtime.dirty = true;
                                        }
                                        Event::Mouse(mouse) => {
                                            self.handle_mouse(mouse);
                                            self.runtime.dirty = true;
                                        }
                                        Event::Resize(_, _) => {
                                            self.pending_terminal_resize = true;
                                            self.runtime.dirty = true;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        if self.runtime.dirty {
                            terminal.draw(|f| self.draw(f))?;
                            self.runtime.dirty = false;
                        }
                        self.sync_pending_terminal_resize();
                        if self.quit {
                            return Ok(());
                        }
                    }
                }

                let cancel = CancellationToken::new();
                self.active_cancel = Some(cancel.clone());

                let provider = registry.create(
                    &self.provider_name,
                    config.providers.get(&self.provider_name),
                );
                let artifact_dir =
                    config.artifact_dir(&std::env::current_dir().unwrap_or_default());

                match provider {
                    Some(p) => {
                        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let resolve_peer =
                            |name: &str| registry.create(name, config.providers.get(name));
                        let turn_fut = run_routed_turn_observable(
                            store,
                            session,
                            p.as_ref(),
                            &peer_catalog,
                            &resolve_peer,
                            msg,
                            cwd,
                            Some(&artifact_dir),
                            Some(&runtime_tx),
                            cancel,
                        );
                        tokio::pin!(turn_fut);

                        // Running phase: turn future + UI events
                        loop {
                            if self.runtime.dirty {
                                terminal.draw(|f| self.draw(f))?;
                                self.runtime.dirty = false;
                            }
                            self.sync_pending_terminal_resize();
                            if self.quit {
                                if let Some(c) = self.active_cancel.take() {
                                    c.cancel();
                                }
                                return Ok(());
                            }

                            tokio::select! {
                                result = &mut turn_fut => {
                                    self.active_cancel = None;
                                    if let Err(e) = result {
                                        self.handle_turn_error(&e.to_string());
                                    }
                                    self.update_idle_status();
                                    self.runtime.dirty = true;
                                    break;
                                }
                                Some(evt) = runtime_rx.recv() => {
                                    self.handle_runtime_event(evt, &peer_catalog);
                                    // Batch drain: process up to 32 queued events in one pass
                                    for _ in 0..32 {
                                        match runtime_rx.try_recv() {
                                            Ok(e) => self.handle_runtime_event(e, &peer_catalog),
                                            Err(_) => break,
                                        }
                                    }
                                }
                                result = &mut core_probe_fut, if !core_probe_done => {
                                    core_probe_done = true;
                                    self.apply_core_host_surface_probe(result);
                                }
                                _ = status_tick.tick() => {
                                    self.update_running_status();
                                    self.runtime.dirty = true;
                                }
                                _ = hyard_job_tick.tick() => {
                                    self.refresh_hyard_jobs();
                                }
                                _ = kbd_tick.tick() => {
                                    while event::poll(std::time::Duration::ZERO)? {
                                        match event::read()? {
                                            Event::Key(key) => {
                                                if key.kind != KeyEventKind::Press { continue; }
                                                self.handle_key_running(key.code, key.modifiers);
                                                self.runtime.dirty = true;
                                            }
                                            Event::Mouse(mouse) => {
                                                self.handle_mouse(mouse);
                                                self.runtime.dirty = true;
                                            }
                                            Event::Resize(_, _) => {
                                                self.pending_terminal_resize = true;
                                                self.runtime.dirty = true;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        self.active_cancel = None;
                        self.handle_turn_error(&format!(
                            "provider '{}' not registered",
                            self.provider_name
                        ));
                    }
                }

                while let Ok(evt) = runtime_rx.try_recv() {
                    self.handle_runtime_event(evt, &peer_catalog);
                }
                continue;
            }

            // ── Idle: keyboard + background probe + events ──
            tokio::select! {
                result = &mut probe_fut, if !probe_done => {
                    peer_catalog = result;
                    probe_done = true;
                    self.sync_peer_catalog_state(&peer_catalog, true);
                    self.runtime.push_event(format!(
                        "[hyard] peer probe complete: {}/{} ready",
                        self.runtime.peer_ready_count,
                        self.runtime.peer_total_count
                    ));
                    self.runtime.dirty = true;
                }
                result = &mut core_probe_fut, if !core_probe_done => {
                    core_probe_done = true;
                    self.apply_core_host_surface_probe(result);
                }
                Some(evt) = runtime_rx.recv() => {
                    self.handle_runtime_event(evt, &peer_catalog);
                    for _ in 0..32 {
                        match runtime_rx.try_recv() {
                            Ok(e) => self.handle_runtime_event(e, &peer_catalog),
                            Err(_) => break,
                        }
                    }
                }
                _ = status_tick.tick() => {
                    if self.runtime.phase.is_busy() {
                        self.update_running_status();
                        self.runtime.dirty = true;
                    }
                }
                _ = hyard_job_tick.tick() => {
                    self.refresh_hyard_jobs();
                }
                _ = kbd_tick.tick() => {
                    while event::poll(std::time::Duration::ZERO)? {
                        match event::read()? {
                            Event::Key(key) => {
                                if key.kind != KeyEventKind::Press { continue; }
                                self.handle_key_idle(key.code, key.modifiers, &mut pending_submit);
                                self.runtime.dirty = true;
                            }
                            Event::Mouse(mouse) => {
                                self.handle_mouse(mouse);
                                self.runtime.dirty = true;
                            }
                            Event::Resize(_, _) => {
                                self.pending_terminal_resize = true;
                                self.runtime.dirty = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Prepare UI state for a new turn (before the future is created).
    fn prepare_turn(&mut self, message: &str) {
        self.last_message = Some(message.to_string());
        self.turns.push(TurnEntry {
            turn_id: Uuid::nil(),
            user_message: message.to_string(),
            response: None,
            status: "pending".to_string(),
            delegated: false,
        });
        self.scroll_to_latest();
        self.runtime.dirty = true;
    }

    /// Handle key events in the idle phase (full input editing).
    fn handle_key_idle(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        pending_submit: &mut Option<String>,
    ) {
        if self.focus != Focus::Input && self.try_switch_message_view_by_key(code) {
            return;
        }
        if self.focus != Focus::Input && self.try_switch_provider_mode_by_key(code) {
            return;
        }

        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            (KeyCode::BackTab, _) => {
                let next = self.focus.prev();
                self.set_focus(next);
            }
            (KeyCode::Tab, _) => {
                let next = self.focus.next();
                self.set_focus(next);
            }
            (KeyCode::F(2), _) => {
                self.advance_right_pane();
            }
            _ if self.focus == Focus::Input => match code {
                KeyCode::Char('r') if self.input.is_empty() => {
                    if let Some(ref msg) = self.last_message {
                        *pending_submit = Some(msg.clone());
                    }
                }
                KeyCode::Enter => {
                    if !self.input.is_empty() {
                        *pending_submit = Some(self.input.clone());
                        self.input.clear();
                        self.cursor = 0;
                    }
                }
                KeyCode::Char(c) => {
                    let bp = char_to_byte(&self.input, self.cursor);
                    self.input.insert(bp, c);
                    self.cursor += 1;
                }
                KeyCode::Backspace => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        let bp = char_to_byte(&self.input, self.cursor);
                        self.input.remove(bp);
                    }
                }
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => {
                    if self.cursor < self.input.chars().count() {
                        self.cursor += 1;
                    }
                }
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.input.chars().count(),
                _ => {}
            },
            _ if self.focus == Focus::Transcript => match code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_message_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_message_by(1),
                KeyCode::PageUp => self.scroll_message_by(-10),
                KeyCode::PageDown => self.scroll_message_by(10),
                KeyCode::Home => self.scroll_to_top(),
                KeyCode::End => self.scroll_to_latest(),
                _ => {}
            },
            _ if self.focus == Focus::Sidebar => match code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_right_pane_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_right_pane_by(1),
                KeyCode::PageUp => self.scroll_right_pane_by(-10),
                KeyCode::PageDown => self.scroll_right_pane_by(10),
                KeyCode::Home => self.scroll_right_pane_to_top(),
                KeyCode::End => self.scroll_right_pane_to_latest(),
                _ => {}
            },
            _ => {}
        }
    }

    /// Handle key events during running phase (limited: Esc cancel, Ctrl-C quit, pane toggle, scroll).
    fn handle_key_running(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if self.focus != Focus::Input && self.try_switch_message_view_by_key(code) {
            return;
        }
        if self.focus != Focus::Input && self.try_switch_provider_mode_by_key(code) {
            return;
        }

        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            (KeyCode::Esc, _) => {
                if let Some(cancel) = self.active_cancel.take() {
                    cancel.cancel();
                    // Immediately reset UI — don't wait for turn_fut to drain.
                    // The running select loop will still finish turn_fut in background.
                    self.runtime.phase = Phase::Idle;
                    self.runtime.started_at = None;
                    self.runtime.current_peer = None;
                    self.runtime.push_event("[user] cancelled".to_string());
                    if let Some(last) = self.turns.last_mut()
                        && last.status == "pending"
                    {
                        last.status = "cancelled".to_string();
                        last.response = Some("(cancelled by user)".to_string());
                    }
                    self.update_idle_status();
                }
            }
            (KeyCode::BackTab, _) => {
                let next = self.focus.prev();
                self.set_focus(next);
            }
            (KeyCode::Tab, _) => {
                let next = self.focus.next();
                self.set_focus(next);
            }
            (KeyCode::F(2), _) => {
                self.advance_right_pane();
            }
            _ if self.focus == Focus::Transcript => match code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_message_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_message_by(1),
                KeyCode::PageUp => self.scroll_message_by(-10),
                KeyCode::PageDown => self.scroll_message_by(10),
                KeyCode::Home => self.scroll_to_top(),
                KeyCode::End => self.scroll_to_latest(),
                _ => {}
            },
            _ if self.focus == Focus::Sidebar => match code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_right_pane_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_right_pane_by(1),
                KeyCode::PageUp => self.scroll_right_pane_by(-10),
                KeyCode::PageDown => self.scroll_right_pane_by(10),
                KeyCode::Home => self.scroll_right_pane_to_top(),
                KeyCode::End => self.scroll_right_pane_to_latest(),
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let column = mouse.column;
        let row = mouse.row;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(view) = self
                    .message_tab_hitboxes
                    .iter()
                    .find(|(rect, _)| rect_contains(*rect, column, row))
                    .map(|(_, view)| view.clone())
                {
                    self.set_focus(Focus::Transcript);
                    self.set_message_view(view);
                    return;
                }

                if let Some(mode) = self
                    .provider_mode_hitboxes
                    .iter()
                    .find(|(rect, _)| rect_contains(*rect, column, row))
                    .map(|(_, mode)| *mode)
                    && let MessageView::Provider(provider) = self.message_view.clone()
                {
                    self.set_focus(Focus::Transcript);
                    self.set_provider_mode(&provider, mode);
                    return;
                }

                if rect_contains(self.input_area, column, row) {
                    self.set_focus(Focus::Input);
                } else if rect_contains(self.sidebar_area, column, row) {
                    self.set_focus(Focus::Sidebar);
                } else if rect_contains(self.message_panel_area, column, row)
                    || rect_contains(self.turn_list_area, column, row)
                {
                    self.set_focus(Focus::Transcript);
                }
            }
            MouseEventKind::ScrollUp => {
                if rect_contains(self.sidebar_area, column, row) {
                    self.set_focus(Focus::Sidebar);
                    self.scroll_right_pane_by(-3);
                } else {
                    if rect_contains(self.message_panel_area, column, row)
                        || rect_contains(self.turn_list_area, column, row)
                    {
                        self.set_focus(Focus::Transcript);
                    }
                    self.scroll_message_by(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if rect_contains(self.sidebar_area, column, row) {
                    self.set_focus(Focus::Sidebar);
                    self.scroll_right_pane_by(3);
                } else {
                    if rect_contains(self.message_panel_area, column, row)
                        || rect_contains(self.turn_list_area, column, row)
                    {
                        self.set_focus(Focus::Transcript);
                    }
                    self.scroll_message_by(3);
                }
            }
            _ => {}
        }
    }

    fn handle_runtime_event(&mut self, evt: RuntimeEvent, peer_catalog: &PeerCatalog) {
        if matches!(
            evt,
            RuntimeEvent::CoreTurnStarted { .. }
                | RuntimeEvent::PeerTurnStarted { .. }
                | RuntimeEvent::FinalizationStarted { .. }
        ) {
            self.pending_terminal_resize = true;
        }

        self.runtime.apply(&evt);
        self.sync_peer_host_surface(peer_catalog);

        match &evt {
            RuntimeEvent::TurnCompleted {
                turn_id,
                response,
                provider,
            } => {
                if let Some(last) = self.turns.last_mut() {
                    last.turn_id = *turn_id;
                    last.status = "completed".to_string();
                    last.response = response.clone();
                    last.delegated = false;
                }
                let msg: String = self
                    .turns
                    .last()
                    .map(|t| prefix_chars(&t.user_message, 20))
                    .unwrap_or_default();
                push_artifact(
                    &mut self.artifact_entries,
                    ArtifactDisplayEntry {
                        turn_label: format!("[{provider}] {msg}"),
                        items: vec!["stdout.txt".to_string(), "events.jsonl".to_string()],
                    },
                );
                self.update_idle_status();
            }
            RuntimeEvent::TurnFailed { turn_id, error, .. } => {
                if let Some(last) = self.turns.last_mut() {
                    last.turn_id = *turn_id;
                    last.status = "failed".to_string();
                    last.response = Some(format!("Error: {error}"));
                }
                self.status = format!("turn failed: {error}");
            }
            RuntimeEvent::DelegateCompleted {
                peer,
                status,
                summary,
                ..
            } => {
                if let Some(last) = self.turns.last_mut() {
                    last.delegated = true;
                }
                let sum_preview: String = summary
                    .as_deref()
                    .unwrap_or("(none)")
                    .chars()
                    .take(30)
                    .collect();
                push_artifact(
                    &mut self.artifact_entries,
                    ArtifactDisplayEntry {
                        turn_label: format!("[peer/{peer}] {status}"),
                        items: vec![
                            format!("delegate_result: {sum_preview}"),
                            "peer_stdout.txt".to_string(),
                        ],
                    },
                );
            }
            _ => {}
        }

        if !matches!(
            evt,
            RuntimeEvent::TurnCompleted { .. } | RuntimeEvent::TurnFailed { .. }
        ) && self.runtime.phase.is_busy()
        {
            self.update_running_status();
        } else if matches!(evt, RuntimeEvent::HyardJobObserved { .. }) {
            self.update_idle_status();
        }
    }

    fn handle_turn_error(&mut self, error: &str) {
        if let Some(last) = self.turns.last_mut()
            && last.status == "pending"
        {
            last.status = "failed".to_string();
            last.response = Some(format!("错误：{error}"));
        }
        self.status = format!("回合失败：{error}");
        self.runtime.phase = Phase::Idle;
        self.runtime.started_at = None;
        self.runtime.dirty = true;
    }

    fn update_idle_status(&mut self) {
        let retry_hint = if self.last_message.is_some() {
            " | r:重试"
        } else {
            ""
        };
        let view_hint = self.view_switch_hint();
        let hyard_suffix = hyard_status_suffix(
            self.runtime.active_hyard_job_count,
            self.runtime.waiting_hyard_job_count,
            self.runtime.inferred_hyard_job_count,
        );
        self.status = format!(
            "session:{} | {}{} | focus:{} | {} | Tab:切焦点 | Enter:发送{} | Esc:取消 | F2:右侧面板 | Ctrl-C:退出",
            self.session_id_prefix,
            self.provider_name,
            hyard_suffix,
            self.focus.label(),
            view_hint,
            retry_hint,
        );
    }

    fn update_running_status(&mut self) {
        let elapsed = self.runtime.elapsed_display();
        let provider = &self.runtime.current_provider;
        let peer = self.runtime.current_peer.as_deref();
        let view_hint = self.view_switch_hint();
        let hyard_suffix = hyard_status_suffix(
            self.runtime.active_hyard_job_count,
            self.runtime.waiting_hyard_job_count,
            self.runtime.inferred_hyard_job_count,
        );
        self.status = match &self.runtime.phase {
            Phase::Preparing => format!(
                "准备回合 [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                self.focus.label(),
            ),
            Phase::ProbingPeers => {
                format!(
                    "探测 peers [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                    self.focus.label(),
                )
            }
            Phase::CoreRunning => format!(
                "主代理执行中 ({provider}) [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                self.focus.label(),
            ),
            Phase::DelegateRequested => {
                format!(
                    "已请求委托 -> {} [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                    peer.unwrap_or("?"),
                    self.focus.label(),
                )
            }
            Phase::PeerRunning => {
                format!(
                    "peer 执行中 -> {} [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                    peer.unwrap_or("?"),
                    self.focus.label(),
                )
            }
            Phase::Finalizing => format!(
                "正在收尾 ({provider}) [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                self.focus.label(),
            ),
            Phase::Committing => format!(
                "正在保存结果 [{elapsed}]{hyard_suffix} | focus:{} | {view_hint}",
                self.focus.label(),
            ),
            Phase::Idle => String::new(), // shouldn't reach here
        };
    }

    fn sync_pending_terminal_resize(&mut self) {
        if !self.pending_terminal_resize {
            return;
        }

        self.pending_terminal_resize = false;
        let rows = self.message_panel_area.height.saturating_sub(2);
        let cols = self.message_panel_area.width.saturating_sub(2);
        if rows == 0 || cols == 0 {
            return;
        }

        self.runtime.resize_active_provider_screens(rows, cols);
        for turn_id in self.runtime.active_turn_ids() {
            let _ = resize_registered_pty(turn_id, rows, cols);
        }
    }

    // ── Drawing ──

    fn draw(&mut self, f: &mut Frame) {
        self.ensure_visible_message_view();
        let size = f.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(size);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(22),
                Constraint::Min(30),
                Constraint::Length(32),
            ])
            .split(outer[1]);

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(12),
                Constraint::Min(4),
                Constraint::Length(6),
            ])
            .split(body[2]);

        self.turn_list_area = body[0];
        self.message_panel_area = body[1];
        self.sidebar_area = body[2];
        self.input_area = outer[3];

        self.draw_message_tabs(f, outer[0]);
        self.draw_turn_list(f, body[0]);
        self.draw_message_panel(f, body[1]);
        self.draw_runtime_status(f, right[0]);
        match self.right_pane {
            RightPane::Events => self.draw_event_log(f, right[1]),
            RightPane::RawStream => self.draw_raw_stream(f, right[1]),
            RightPane::Artifacts => self.draw_artifact_detail(f, right[1]),
        }
        self.draw_artifacts(f, right[2]);
        self.draw_hint_bar(f, outer[2]);
        self.draw_input(f, outer[3]);
        self.draw_status(f, outer[4]);
    }

    fn draw_message_tabs(&mut self, f: &mut Frame, area: Rect) {
        let views = self.visible_message_views();
        let titles: Vec<Line> = views
            .iter()
            .enumerate()
            .map(|(index, view)| Line::from(self.message_view_label(index, view)))
            .collect();
        let selected = views
            .iter()
            .position(|view| view == &self.message_view)
            .unwrap_or(0);
        let accent = self.message_view_accent_color(&self.message_view);
        self.message_tab_hitboxes = compute_tab_hitboxes(area, &views, &titles);

        let tabs = Tabs::new(titles)
            .select(selected)
            .block(
                Block::default()
                    .title(" Views ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .divider(Span::raw(" | "));

        f.render_widget(tabs, area);
    }

    fn draw_message_panel(&mut self, f: &mut Frame, area: Rect) {
        match self.message_view.clone() {
            MessageView::Overview => {
                self.provider_mode_hitboxes.clear();
                self.message_panel_area = area;
                self.draw_transcript(f, area);
            }
            MessageView::Provider(provider) => self.draw_provider_message_panel(f, area, &provider),
            view => {
                self.provider_mode_hitboxes.clear();
                self.message_panel_area = area;
                self.draw_message_stream(f, area, &view);
            }
        }
    }

    fn draw_provider_message_panel(&mut self, f: &mut Frame, area: Rect, provider: &str) {
        self.provider_mode_hitboxes.clear();
        if area.height <= 4 {
            self.message_panel_area = area;
            self.draw_provider_secondary_content(
                f,
                area,
                provider,
                self.current_provider_mode(provider),
            );
            return;
        }

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(area);
        self.message_panel_area = sections[1];
        self.draw_provider_mode_tabs(f, sections[0], provider);
        self.draw_provider_secondary_content(
            f,
            sections[1],
            provider,
            self.current_provider_mode(provider),
        );
    }

    fn draw_provider_mode_tabs(&mut self, f: &mut Frame, area: Rect, provider: &str) {
        let current_mode = self.current_provider_mode(provider);
        let titles = ProviderPaneMode::all()
            .into_iter()
            .map(|mode| Line::from(format!(" {} ", mode.label())))
            .collect::<Vec<_>>();
        self.provider_mode_hitboxes = compute_provider_mode_hitboxes(area, &titles);

        let tabs = Tabs::new(titles)
            .select(current_mode.index())
            .block(
                Block::default()
                    .title(" 模式 ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(
                Style::default()
                    .fg(self
                        .message_view_accent_color(&MessageView::Provider(provider.to_string())))
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::raw(" | "));

        f.render_widget(tabs, area);
    }

    fn draw_provider_secondary_content(
        &mut self,
        f: &mut Frame,
        area: Rect,
        provider: &str,
        mode: ProviderPaneMode,
    ) {
        let border_color = if self.focus == Focus::Transcript {
            self.message_view_accent_color(&MessageView::Provider(provider.to_string()))
        } else {
            Color::DarkGray
        };

        let lines: Vec<Line<'static>> = match mode {
            ProviderPaneMode::Screen => {
                let lines = self.runtime.provider_screen_rendered_lines(provider, 400);
                if lines.is_empty() {
                    vec![Line::from(Span::styled(
                        empty_provider_mode_text(provider, mode),
                        Style::default().fg(Color::DarkGray),
                    ))]
                } else {
                    lines
                }
            }
            ProviderPaneMode::Raw => {
                let entries = collect_provider_mode_entries(provider, mode, &self.runtime);
                if entries.is_empty() {
                    vec![Line::from(Span::styled(
                        empty_provider_mode_text(provider, mode),
                        Style::default().fg(Color::DarkGray),
                    ))]
                } else {
                    entries
                        .into_iter()
                        .map(|entry| {
                            Line::from(Span::styled(entry, Style::default().fg(Color::Green)))
                        })
                        .collect()
                }
            }
            ProviderPaneMode::Timeline => {
                let entries = collect_provider_mode_entries(provider, mode, &self.runtime);
                if entries.is_empty() {
                    vec![Line::from(Span::styled(
                        empty_provider_mode_text(provider, mode),
                        Style::default().fg(Color::DarkGray),
                    ))]
                } else {
                    entries
                        .into_iter()
                        .map(|entry| {
                            Line::from(Span::styled(
                                format!(" {entry}"),
                                Style::default().fg(self.message_view_accent_color(
                                    &MessageView::Provider(provider.to_string()),
                                )),
                            ))
                        })
                        .collect()
                }
            }
        };

        let scroll = self.sync_current_message_scroll(max_scroll_for_lines(&lines, area));
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(
                        self.message_view_panel_title(&MessageView::Provider(provider.to_string())),
                    )
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_turn_list(&self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let marker = match t.status.as_str() {
                    "completed" if t.delegated => "D",
                    "completed" => "+",
                    "failed" => "x",
                    "pending" => "~",
                    "cancelled" => "C",
                    _ => " ",
                };
                let msg = prefix_chars(&t.user_message, 12);
                ListItem::new(format!("{marker} #{} {msg}", i + 1))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" Turns ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(list, area);
    }

    fn draw_transcript(&mut self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for (i, turn) in self.turns.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("#{} You: ", i + 1),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(turn.user_message.clone()),
            ]));

            match &turn.response {
                Some(resp) => {
                    let label = if turn.delegated {
                        format!("   [core/{}] (via delegate): ", self.provider_name)
                    } else {
                        format!("   [core/{}]: ", self.provider_name)
                    };
                    lines.push(Line::from(Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for line in resp.lines() {
                        lines.push(Line::from(format!("   {line}")));
                    }
                }
                None if turn.status == "pending" => {
                    if !self.runtime.stream_lines.is_empty() {
                        let active = if self.runtime.phase == Phase::PeerRunning {
                            format!(
                                "[peer/{}]",
                                self.runtime.current_peer.as_deref().unwrap_or("?")
                            )
                        } else {
                            format!("[core/{}]", self.runtime.current_provider)
                        };
                        let progress_label = if self.runtime.phase.is_executing() {
                            "streaming..."
                        } else if self.runtime.phase.is_busy() {
                            "finishing..."
                        } else {
                            "pending..."
                        };
                        lines.push(Line::from(Span::styled(
                            format!("   {active} ({progress_label}): "),
                            Style::default().fg(Color::Yellow),
                        )));
                        for sl in self.runtime.stream_lines.iter().rev().take(5).rev() {
                            let preview = prefix_chars(sl, 60);
                            lines.push(Line::from(Span::styled(
                                format!("   {preview}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                    } else {
                        lines.push(Line::from(Span::styled(
                            "   (running...)",
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                }
                None => {}
            }
            lines.push(Line::from(""));
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Type a message and press Enter.",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let border_color = if self.focus == Focus::Transcript {
            Color::Blue
        } else {
            Color::DarkGray
        };

        let scroll = self.sync_current_message_scroll(max_scroll_for_lines(&lines, area));

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(self.message_view_panel_title(&self.message_view))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_message_stream(&mut self, f: &mut Frame, area: Rect, view: &MessageView) {
        let entries = collect_message_view_entries(view, &self.runtime);
        let lines: Vec<Line> = if entries.is_empty() {
            vec![Line::from(Span::styled(
                empty_message_view_text(view),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            entries
                .into_iter()
                .map(|entry| {
                    let color = self.message_view_accent_color(view);
                    Line::from(Span::styled(
                        format!(" {entry}"),
                        Style::default().fg(color),
                    ))
                })
                .collect()
        };

        let border_color = if self.focus == Focus::Transcript {
            self.message_view_accent_color(&self.message_view)
        } else {
            Color::DarkGray
        };

        let scroll = self.sync_current_message_scroll(max_scroll_for_lines(&lines, area));

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(self.message_view_panel_title(view))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_runtime_status(&self, f: &mut Frame, area: Rect) {
        let phase = &self.runtime.phase;
        let provider = &self.runtime.current_provider;
        let peer = self.runtime.current_peer.as_deref().unwrap_or("-");
        let elapsed = self.runtime.elapsed_display();

        let phase_color = match phase {
            Phase::Idle => Color::Green,
            Phase::Committing => Color::Green, // almost done — green
            Phase::Preparing | Phase::ProbingPeers => Color::Blue,
            Phase::CoreRunning | Phase::Finalizing => Color::Cyan,
            Phase::DelegateRequested => Color::Magenta,
            Phase::PeerRunning => Color::Yellow,
        };

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(" phase: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                phase.to_string(),
                Style::default()
                    .fg(phase_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" core:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(provider),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" peer:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(peer),
            Span::styled(
                if elapsed.is_empty() {
                    String::new()
                } else {
                    format!("  {elapsed}")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(build_peer_probe_spans(
            self.runtime.peer_ready_count,
            self.runtime.peer_total_count,
            self.runtime.peer_probe_done,
        )));
        let host_surface = self.runtime.active_host_surface();
        lines.push(Line::from(build_host_status_spans(host_surface)));
        lines.push(Line::from(build_execution_spans(
            " exec: ",
            self.runtime.active_execution(),
        )));
        lines.push(Line::from(build_hyard_job_count_spans(
            self.runtime.active_hyard_job_count,
            self.runtime.waiting_hyard_job_count,
            self.runtime.inferred_hyard_job_count,
            self.runtime.hyard_jobs.len(),
        )));
        lines.push(Line::from(build_hyard_primary_job_spans(
            self.runtime.primary_hyard_job(),
        )));
        lines.push(Line::from(build_hyard_job_execution_spans(
            self.runtime.primary_hyard_job(),
        )));
        let latest = self.runtime.latest_hyard_event.as_deref().unwrap_or("-");
        let latest_preview = event_preview(latest, LATEST_EVENT_PREVIEW_CHARS);
        lines.push(Line::from(vec![
            Span::styled(" hyard: ", Style::default().fg(Color::DarkGray)),
            Span::styled(latest_preview, Style::default().fg(Color::Magenta)),
        ]));

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(" Status ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(paragraph, area);
    }

    fn draw_event_log(&mut self, f: &mut Frame, area: Rect) {
        let lines: Vec<Line> = if self.runtime.event_log.is_empty() {
            vec![Line::from(Span::styled(
                " (no events yet)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.runtime
                .event_log
                .iter()
                .map(|e| {
                    let color = event_log_color(e);
                    Line::from(Span::styled(format!(" {e}"), Style::default().fg(color)))
                })
                .collect()
        };

        let scroll = self.sync_current_right_pane_scroll(max_scroll_for_lines(&lines, area));
        let title = " Events (F2: raw) ";
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(self.right_pane_border_color())),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_raw_stream(&mut self, f: &mut Frame, area: Rect) {
        let lines: Vec<Line> = if self.runtime.raw_json_lines.is_empty() {
            vec![Line::from(Span::styled(
                " (no raw output yet)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.runtime
                .raw_json_lines
                .iter()
                .map(|l| {
                    let display: String = l
                        .chars()
                        .take(area.width.saturating_sub(3) as usize)
                        .collect();
                    Line::from(Span::styled(
                        format!(" {display}"),
                        Style::default().fg(Color::DarkGray),
                    ))
                })
                .collect()
        };

        let scroll = self.sync_current_right_pane_scroll(max_scroll_for_lines(&lines, area));
        let title = " Raw Stream (F2: toggle) ";
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(self.right_pane_border_color())),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_artifacts(&mut self, f: &mut Frame, area: Rect) {
        self.draw_artifacts_panel(f, area, false);
    }

    fn draw_artifact_detail(&mut self, f: &mut Frame, area: Rect) {
        self.draw_artifacts_panel(f, area, true);
    }

    fn draw_artifacts_panel(&mut self, f: &mut Frame, area: Rect, detail: bool) {
        let lines: Vec<Line> = if self.artifact_entries.is_empty() {
            vec![Line::from(Span::styled(
                " (no artifacts)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            self.artifact_entries
                .iter()
                .flat_map(|entry| {
                    let label_style = if detail {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let mut result = vec![Line::from(Span::styled(
                        format!(" {}", entry.turn_label),
                        label_style,
                    ))];
                    for item in &entry.items {
                        let color = if detail {
                            if item.contains("delegate") {
                                Color::Magenta
                            } else if item.contains("stderr") {
                                Color::Red
                            } else {
                                Color::DarkGray
                            }
                        } else {
                            Color::DarkGray
                        };
                        result.push(Line::from(Span::styled(
                            format!("   {item}"),
                            Style::default().fg(color),
                        )));
                    }
                    if detail {
                        result.push(Line::from(""));
                    }
                    result
                })
                .collect()
        };

        let (title, border) = if detail {
            (" Artifact Detail (F2) ", self.right_pane_border_color())
        } else {
            (" Artifacts (F2) ", Color::DarkGray)
        };

        let scroll =
            detail.then(|| self.sync_current_right_pane_scroll(max_scroll_for_lines(&lines, area)));

        let mut paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
        );
        if let Some(scroll) = scroll {
            paragraph = paragraph.wrap(Wrap { trim: false }).scroll((scroll, 0));
        }
        f.render_widget(paragraph, area);
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let busy = self.runtime.phase.is_busy();
        let executing = self.runtime.phase.is_executing();
        let border_color = if self.focus == Focus::Input && !busy {
            Color::Blue
        } else if executing {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        let title = if executing {
            " Input (running...) "
        } else if busy {
            " Input (finishing...) "
        } else {
            " Input "
        };
        let input = Paragraph::new(self.input.as_str()).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        );
        f.render_widget(input, area);

        if self.focus == Focus::Input && !busy {
            let offset: u16 = self
                .input
                .chars()
                .take(self.cursor)
                .map(|c| if c.is_ascii() { 1u16 } else { 2u16 })
                .sum();
            f.set_cursor_position((area.x + offset + 1, area.y + 1));
        }
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let paragraph = Paragraph::new(Span::styled(
            format!(" {}", self.status),
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(paragraph, area);
    }

    fn draw_hint_bar(&self, f: &mut Frame, area: Rect) {
        let hints = self.compose_hint_messages();
        let (text, color) = if hints.is_empty() {
            (
                format!(
                    " 焦点:{} | Tab/Shift-Tab 切换焦点 | 鼠标点击 Views 切视图 | F2 切换右侧面板 | End 跳到最新 ",
                    self.focus.label(),
                ),
                Color::DarkGray,
            )
        } else {
            (format!(" {}", hints.join("  |  ")), Color::Yellow)
        };

        let paragraph = Paragraph::new(Span::styled(
            text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        f.render_widget(paragraph, area);
    }

    fn sync_status_after_ui_change(&mut self) {
        if self.runtime.phase.is_busy() {
            self.update_running_status();
        } else if self.status.starts_with("session:")
            || self.status.starts_with("provider:")
            || self.status.contains("| focus:")
            || self.status.is_empty()
        {
            self.update_idle_status();
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        if self.focus != focus {
            self.focus = focus;
            self.sync_status_after_ui_change();
            self.runtime.dirty = true;
        }
    }

    fn set_message_view(&mut self, view: MessageView) {
        if self.message_view != view {
            self.message_view = view;
            self.sync_status_after_ui_change();
            self.runtime.dirty = true;
        }
    }

    fn ensure_visible_message_view(&mut self) {
        let visible = self.visible_message_views();
        if !visible.iter().any(|view| view == &self.message_view) {
            self.message_view = visible.into_iter().next().unwrap_or(MessageView::Overview);
            self.sync_status_after_ui_change();
        }
    }

    fn visible_message_views(&self) -> Vec<MessageView> {
        let mut views = Vec::new();
        views.push(MessageView::Overview);
        views.extend(
            self.runtime
                .provider_view_ids()
                .into_iter()
                .filter(|provider| self.runtime.provider_has_activity(provider))
                .map(MessageView::Provider),
        );
        if !views
            .iter()
            .any(|view| matches!(view, MessageView::Provider(provider) if provider == &self.provider_name))
        {
            views.push(MessageView::Provider(self.provider_name.clone()));
        }
        views.push(MessageView::Hyard);
        views
    }

    fn default_provider_mode(&self, provider: &str) -> ProviderPaneMode {
        if !self
            .runtime
            .provider_screen_rendered_lines(provider, 400)
            .is_empty()
            || !self.runtime.provider_terminal_entries(provider).is_empty()
        {
            ProviderPaneMode::Screen
        } else if !self.runtime.provider_view_entries(provider).is_empty()
            || self
                .runtime
                .hyard_jobs
                .iter()
                .any(|job| job.provider == provider)
        {
            ProviderPaneMode::Timeline
        } else {
            ProviderPaneMode::Screen
        }
    }

    fn current_provider_mode(&self, provider: &str) -> ProviderPaneMode {
        self.provider_view_modes
            .get(provider)
            .copied()
            .unwrap_or_else(|| self.default_provider_mode(provider))
    }

    fn set_provider_mode(&mut self, provider: &str, mode: ProviderPaneMode) {
        if self.current_provider_mode(provider) != mode {
            self.provider_view_modes.insert(provider.to_string(), mode);
            self.sync_status_after_ui_change();
            self.runtime.dirty = true;
        } else {
            self.provider_view_modes
                .entry(provider.to_string())
                .or_insert(mode);
        }
    }

    fn cycle_current_provider_mode(&mut self) -> bool {
        let MessageView::Provider(provider) = self.message_view.clone() else {
            return false;
        };
        let next = self.current_provider_mode(&provider).next();
        self.set_provider_mode(&provider, next);
        true
    }

    fn message_view_label(&self, index: usize, view: &MessageView) -> String {
        let ordinal = index + 1;
        match view {
            MessageView::Overview => format!("{ordinal} 总览"),
            MessageView::Provider(provider) => format!("{ordinal} {provider}"),
            MessageView::Hyard => format!("{ordinal} HYARD"),
        }
    }

    fn message_view_panel_title(&self, view: &MessageView) -> String {
        match view {
            MessageView::Overview => " 总览 ".to_string(),
            MessageView::Provider(provider) => {
                let terminal_transport = self.runtime.provider_terminal_transport(provider);
                match self.current_provider_mode(provider) {
                    ProviderPaneMode::Screen => match terminal_transport {
                        Some(transport) => {
                            format!(" {provider} 屏幕镜像 ({}) ", transport.to_uppercase())
                        }
                        None => format!(" {provider} 屏幕镜像 "),
                    },
                    ProviderPaneMode::Raw => match terminal_transport {
                        Some(transport) => {
                            format!(" {provider} 原始输出 ({}) ", transport.to_uppercase())
                        }
                        None => format!(" {provider} 原始输出 "),
                    },
                    ProviderPaneMode::Timeline => format!(" {provider} CLI 时间线 "),
                }
            }
            MessageView::Hyard => " HYARD 活动 ".to_string(),
        }
    }

    fn message_view_accent_color(&self, view: &MessageView) -> Color {
        match view {
            MessageView::Overview => Color::Blue,
            MessageView::Provider(provider) if provider == &self.runtime.current_provider => {
                Color::Cyan
            }
            MessageView::Provider(provider)
                if self.runtime.current_peer.as_deref() == Some(provider.as_str()) =>
            {
                Color::Yellow
            }
            MessageView::Provider(_) => Color::Green,
            MessageView::Hyard => Color::Magenta,
        }
    }

    fn message_view_short_label(&self, view: &MessageView) -> String {
        match view {
            MessageView::Overview => "总览".to_string(),
            MessageView::Provider(provider) => format!(
                "{}/{}",
                provider,
                self.current_provider_mode(provider).short_label()
            ),
            MessageView::Hyard => "HYARD".to_string(),
        }
    }

    fn try_switch_message_view_by_key(&mut self, code: KeyCode) -> bool {
        let Some(index) = digit_key_index(code) else {
            return false;
        };
        let visible = self.visible_message_views();
        let Some(view) = visible.get(index).cloned() else {
            return false;
        };
        self.set_message_view(view);
        true
    }

    fn try_switch_provider_mode_by_key(&mut self, code: KeyCode) -> bool {
        if matches!(code, KeyCode::F(3)) {
            return self.cycle_current_provider_mode();
        }

        let Some(mode) = ProviderPaneMode::from_key(code) else {
            return false;
        };
        let MessageView::Provider(provider) = self.message_view.clone() else {
            return false;
        };
        self.set_provider_mode(&provider, mode);
        true
    }

    fn view_switch_hint(&self) -> String {
        let visible = self.visible_message_views();
        let max_shortcut = visible.len().min(9);
        let base = if max_shortcut >= 2 {
            format!("1-{max_shortcut}:切视图(非输入焦点) / 鼠标点 Views")
        } else {
            "鼠标点 Views".to_string()
        };

        if matches!(self.message_view, MessageView::Provider(_)) {
            format!("{base} / S:屏幕 R:原始 T:时间线 / F3:循环模式")
        } else {
            base
        }
    }

    fn advance_right_pane(&mut self) {
        self.right_pane = match self.right_pane {
            RightPane::Events => RightPane::RawStream,
            RightPane::RawStream => RightPane::Artifacts,
            RightPane::Artifacts => RightPane::Events,
        };
        self.sync_status_after_ui_change();
        self.runtime.dirty = true;
    }

    fn current_message_scroll(&self) -> ScrollState {
        match self.message_view.clone() {
            MessageView::Provider(provider) => *self
                .provider_mode_scrolls
                .get(&(provider.clone(), self.current_provider_mode(&provider)))
                .unwrap_or(&DEFAULT_SCROLL_STATE),
            _ => *self
                .message_scrolls
                .get(&self.message_view)
                .unwrap_or(&DEFAULT_SCROLL_STATE),
        }
    }

    fn current_message_scroll_mut(&mut self) -> &mut ScrollState {
        match self.message_view.clone() {
            MessageView::Provider(provider) => {
                let mode = self.current_provider_mode(&provider);
                self.provider_mode_scrolls
                    .entry((provider, mode))
                    .or_insert_with(ScrollState::new)
            }
            other => self
                .message_scrolls
                .entry(other)
                .or_insert_with(ScrollState::new),
        }
    }

    #[cfg(test)]
    fn message_scroll_mut_for(&mut self, view: MessageView) -> &mut ScrollState {
        self.message_scrolls
            .entry(view)
            .or_insert_with(ScrollState::new)
    }

    #[cfg(test)]
    fn provider_mode_scroll_mut_for(
        &mut self,
        provider: &str,
        mode: ProviderPaneMode,
    ) -> &mut ScrollState {
        self.provider_mode_scrolls
            .entry((provider.to_string(), mode))
            .or_insert_with(ScrollState::new)
    }

    fn current_right_pane_scroll(&self) -> &ScrollState {
        &self.right_pane_scrolls[self.right_pane.index()]
    }

    fn current_right_pane_scroll_mut(&mut self) -> &mut ScrollState {
        &mut self.right_pane_scrolls[self.right_pane.index()]
    }

    fn sync_current_message_scroll(&mut self, max_scroll: u16) -> u16 {
        let state = self.current_message_scroll_mut();
        state.sync(max_scroll);
        state.offset
    }

    fn sync_current_right_pane_scroll(&mut self, max_scroll: u16) -> u16 {
        let state = self.current_right_pane_scroll_mut();
        state.sync(max_scroll);
        state.offset
    }

    fn scroll_message_by(&mut self, delta: i32) {
        self.current_message_scroll_mut().scroll_by(delta);
    }

    fn scroll_to_top(&mut self) {
        self.current_message_scroll_mut().scroll_to_top();
    }

    fn scroll_to_latest(&mut self) {
        self.current_message_scroll_mut().scroll_to_latest();
    }

    fn scroll_right_pane_by(&mut self, delta: i32) {
        self.current_right_pane_scroll_mut().scroll_by(delta);
    }

    fn scroll_right_pane_to_top(&mut self) {
        self.current_right_pane_scroll_mut().scroll_to_top();
    }

    fn scroll_right_pane_to_latest(&mut self) {
        self.current_right_pane_scroll_mut().scroll_to_latest();
    }

    fn right_pane_border_color(&self) -> Color {
        if self.focus == Focus::Sidebar {
            Color::Blue
        } else {
            match self.right_pane {
                RightPane::Events => Color::DarkGray,
                RightPane::RawStream => Color::Blue,
                RightPane::Artifacts => Color::Blue,
            }
        }
    }

    fn compose_hint_messages(&self) -> Vec<String> {
        let mut hints = Vec::new();
        let message_state = self.current_message_scroll();
        if message_state.has_unseen {
            hints.push(format!(
                "{} 视图有新消息 · End 跳到最新",
                self.message_view_short_label(&self.message_view)
            ));
        }

        let right_state = self.current_right_pane_scroll();
        if right_state.has_unseen {
            hints.push(format!(
                "右侧{}面板有新内容 · F2 切换 / End 跳到底部",
                self.right_pane.short_label()
            ));
        }

        hints
    }

    fn apply_core_host_surface_probe(&mut self, result: Result<HostSurfaceProbe, String>) {
        match result {
            Ok(probe) => {
                self.runtime.set_core_host_surface_probe(&probe);
                let host_state = &self.runtime.core_host_surface;
                let status = host_state.readiness().label();
                let detail = host_state.notes.as_deref().unwrap_or(status);
                self.runtime.push_event(format!(
                    "[hyard] 主代理 host surface：{} [{}] ({detail})",
                    host_state.label(),
                    status
                ));
            }
            Err(error) => {
                let unavailable =
                    HostSurfaceProbe::unavailable(vec![format!("probe failed: {error}")]);
                self.runtime.set_core_host_surface_probe(&unavailable);
                self.runtime
                    .push_event(format!("[hyard] 主代理 host 探测失败：{error}"));
            }
        }
    }

    fn sync_peer_host_surface(&mut self, peer_catalog: &PeerCatalog) {
        let peer_probe = self
            .runtime
            .current_peer
            .as_deref()
            .and_then(|peer| peer_catalog.find(peer))
            .and_then(|peer| peer.host_surface.as_ref());

        self.runtime.set_peer_host_surface_probe(peer_probe);
    }

    fn sync_peer_catalog_state(&mut self, peer_catalog: &PeerCatalog, done: bool) {
        let total = peer_catalog.peers.len();
        let ready = peer_catalog
            .peers
            .iter()
            .filter(|peer| peer.available)
            .filter(|peer| {
                peer.host_surface
                    .as_ref()
                    .is_some_and(|surface| surface.is_ready())
            })
            .count();
        self.runtime.set_peer_probe_summary(ready, total, done);
        self.sync_peer_host_surface(peer_catalog);
    }

    fn refresh_hyard_jobs(&mut self) {
        let jobs = read_hyard_job_summaries(&self.job_dir);
        self.runtime.set_hyard_jobs(jobs);
        if self.runtime.phase.is_busy() {
            self.update_running_status();
        } else {
            self.update_idle_status();
        }
    }
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(bp, _)| bp)
        .unwrap_or(s.len())
}

fn digit_key_index(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Char(c) if ('1'..='9').contains(&c) => Some((c as u8 - b'1') as usize),
        _ => None,
    }
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn compute_tab_hitboxes(
    area: Rect,
    views: &[MessageView],
    titles: &[Line],
) -> Vec<(Rect, MessageView)> {
    if area.width <= 2 || area.height <= 2 {
        return Vec::new();
    }

    let inner_x = area.x.saturating_add(1);
    let inner_y = area.y.saturating_add(1);
    let divider_width = 3u16;
    let mut cursor_x = inner_x;
    let mut hitboxes = Vec::with_capacity(views.len());

    for (index, (view, title)) in views.iter().zip(titles.iter()).enumerate() {
        let width = title.width().min(area.width.saturating_sub(2) as usize) as u16;
        if width > 0 {
            hitboxes.push((Rect::new(cursor_x, inner_y, width, 1), view.clone()));
        }
        cursor_x = cursor_x.saturating_add(width);
        if index + 1 < titles.len() {
            cursor_x = cursor_x.saturating_add(divider_width);
        }
    }

    hitboxes
}

fn compute_provider_mode_hitboxes(area: Rect, titles: &[Line]) -> Vec<(Rect, ProviderPaneMode)> {
    if area.width <= 2 || area.height <= 2 {
        return Vec::new();
    }

    let inner_x = area.x.saturating_add(1);
    let inner_y = area.y.saturating_add(1);
    let divider_width = 3u16;
    let mut cursor_x = inner_x;
    let mut hitboxes = Vec::new();

    for (index, (mode, title)) in ProviderPaneMode::all()
        .into_iter()
        .zip(titles.iter())
        .enumerate()
    {
        let width = title.width().min(area.width.saturating_sub(2) as usize) as u16;
        if width > 0 {
            hitboxes.push((Rect::new(cursor_x, inner_y, width, 1), mode));
        }
        cursor_x = cursor_x.saturating_add(width);
        if index + 1 < titles.len() {
            cursor_x = cursor_x.saturating_add(divider_width);
        }
    }

    hitboxes
}

fn max_scroll_for_lines(lines: &[Line], area: Rect) -> u16 {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_width == 0 || inner_height == 0 {
        return 0;
    }

    let total_height: usize = lines
        .iter()
        .map(|line| {
            let width = line.width();
            usize::max(1, width.div_ceil(inner_width))
        })
        .sum();

    total_height.saturating_sub(inner_height) as u16
}

const MAX_ARTIFACT_ENTRIES: usize = 50;

fn push_artifact(entries: &mut Vec<ArtifactDisplayEntry>, entry: ArtifactDisplayEntry) {
    if entries.len() >= MAX_ARTIFACT_ENTRIES {
        entries.remove(0);
    }
    entries.push(entry);
}

const LATEST_EVENT_PREVIEW_CHARS: usize = 40;

fn host_readiness_color(readiness: HostSurfaceReadiness) -> Color {
    match readiness {
        HostSurfaceReadiness::Ready => Color::Green,
        HostSurfaceReadiness::Partial => Color::Yellow,
        HostSurfaceReadiness::Unavailable => Color::Red,
        HostSurfaceReadiness::Unknown => Color::DarkGray,
    }
}

fn peer_probe_color(ready: usize, total: usize, done: bool) -> Color {
    if !done {
        Color::Blue
    } else if total == 0 {
        Color::DarkGray
    } else if ready == total {
        Color::Green
    } else if ready == 0 {
        Color::Red
    } else {
        Color::Yellow
    }
}

fn build_peer_probe_spans(ready: usize, total: usize, done: bool) -> Vec<Span<'static>> {
    let label = if done {
        format!("[{ready}/{total} 就绪]")
    } else {
        format!("[探测中 {total}]")
    };

    vec![
        Span::styled(" peers: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            label,
            Style::default()
                .fg(peer_probe_color(ready, total, done))
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

fn build_hyard_job_count_spans(
    active: usize,
    waiting: usize,
    inferred: usize,
    total: usize,
) -> Vec<Span<'static>> {
    let label = if active == 0 && inferred == 0 {
        format!("{total} 条记录")
    } else if active == 0 {
        format!("{inferred} 个待确认")
    } else if waiting == 0 && inferred == 0 {
        format!("{active} 个活跃")
    } else if waiting == 0 {
        format!("{active} 个活跃 + {inferred} 个待确认")
    } else if inferred == 0 {
        format!("{active} 个活跃（其中 {waiting} 个超时等待）")
    } else {
        format!("{active} 个活跃（其中 {waiting} 个超时等待） + {inferred} 个待确认")
    };

    vec![
        Span::styled(" jobs:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            label,
            Style::default()
                .fg(if active == 0 && inferred == 0 {
                    Color::DarkGray
                } else if waiting > 0 {
                    Color::Magenta
                } else if active == 0 && inferred > 0 {
                    Color::Blue
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

fn build_hyard_primary_job_spans(job: Option<&HyardJobSummary>) -> Vec<Span<'static>> {
    match job {
        Some(job) => vec![
            Span::styled(" job:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(
                    "{} {} ·{}",
                    job.provider,
                    job.status_badge(),
                    job.source_badge()
                ),
                Style::default()
                    .fg(hyard_job_color(job))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" [{}]", job.short_job_id()),
                Style::default().fg(Color::DarkGray),
            ),
        ],
        None => vec![
            Span::styled(" job:   ", Style::default().fg(Color::DarkGray)),
            Span::styled("-", Style::default().fg(Color::DarkGray)),
        ],
    }
}

fn build_hyard_job_execution_spans(job: Option<&HyardJobSummary>) -> Vec<Span<'static>> {
    match job.and_then(|job| job.execution.as_ref()) {
        Some(execution) => build_execution_spans(" jobcmd:", Some(execution)),
        None => vec![
            Span::styled(" jobcmd:", Style::default().fg(Color::DarkGray)),
            Span::styled(" -", Style::default().fg(Color::DarkGray)),
        ],
    }
}

fn build_execution_spans(
    label: &'static str,
    execution: Option<&switchyard_provider_api::ExecutionTelemetry>,
) -> Vec<Span<'static>> {
    match execution {
        Some(execution) => {
            let preview = if execution.used_npm_wrapper_rewrite {
                format!(
                    "{} -> {} [npm→node]",
                    execution_path_preview(&execution.resolved_command, 18),
                    event_preview(
                        execution
                            .js_entry
                            .as_deref()
                            .map(compact_path_hint)
                            .as_deref()
                            .unwrap_or(execution.actual_command.as_str()),
                        22,
                    )
                )
            } else {
                format!(
                    "{} -> {}",
                    execution_path_preview(&execution.original_command, 12),
                    execution_path_preview(&execution.actual_command, 24)
                )
            };
            let preview = match execution.io_transport.as_deref() {
                Some(transport) => format!("[{}] {preview}", transport.to_uppercase()),
                None => preview,
            };
            vec![
                Span::styled(label, Style::default().fg(Color::DarkGray)),
                Span::styled(preview, Style::default().fg(Color::Cyan)),
            ]
        }
        None => vec![
            Span::styled(label, Style::default().fg(Color::DarkGray)),
            Span::styled(" -", Style::default().fg(Color::DarkGray)),
        ],
    }
}

fn execution_path_preview(path: &str, max_chars: usize) -> String {
    event_preview(&compact_path_hint(path), max_chars)
}

fn compact_path_hint(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.contains('\\') || trimmed.contains('/') {
        trimmed
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .unwrap_or(trimmed)
            .to_string()
    } else {
        trimmed.to_string()
    }
}

fn default_provider_mode_for_runtime(provider: &str, runtime: &RuntimeState) -> ProviderPaneMode {
    if !runtime
        .provider_screen_rendered_lines(provider, 400)
        .is_empty()
        || !runtime.provider_terminal_entries(provider).is_empty()
    {
        ProviderPaneMode::Screen
    } else if !runtime.provider_view_entries(provider).is_empty()
        || runtime
            .hyard_jobs
            .iter()
            .any(|job| job.provider == provider)
    {
        ProviderPaneMode::Timeline
    } else {
        ProviderPaneMode::Screen
    }
}

fn collect_provider_mode_entries(
    provider: &str,
    mode: ProviderPaneMode,
    runtime: &RuntimeState,
) -> Vec<String> {
    match mode {
        ProviderPaneMode::Screen => runtime.provider_screen_entries(provider, 400),
        ProviderPaneMode::Raw => runtime
            .provider_terminal_entries(provider)
            .into_iter()
            .map(|entry| escape_raw_terminal_entry(&entry))
            .collect(),
        ProviderPaneMode::Timeline => {
            let entries = runtime.provider_view_entries(provider);
            if entries.is_empty() {
                runtime
                    .hyard_jobs
                    .iter()
                    .filter(|job| job.provider == provider)
                    .flat_map(|job| {
                        format_hyard_job_entries(job, job.is_active() || job.provider == provider)
                    })
                    .collect()
            } else {
                entries
            }
        }
    }
}

fn collect_message_view_entries(view: &MessageView, runtime: &RuntimeState) -> Vec<String> {
    match view {
        MessageView::Overview => Vec::new(),
        MessageView::Provider(provider) => collect_provider_mode_entries(
            provider,
            default_provider_mode_for_runtime(provider, runtime),
            runtime,
        ),
        MessageView::Hyard => runtime
            .hyard_jobs
            .iter()
            .enumerate()
            .flat_map(|(index, job)| format_hyard_job_entries(job, index == 0 || job.is_active()))
            .chain(
                runtime
                    .event_log
                    .iter()
                    .filter(|line| is_hyard_event_text(line))
                    .cloned(),
            )
            .collect(),
    }
}

fn empty_message_view_text(view: &MessageView) -> String {
    match view {
        MessageView::Overview => " （还没有总览内容）".to_string(),
        MessageView::Provider(provider) => format!(" （{provider} 还没有过程输出）"),
        MessageView::Hyard => " （还没有 HYARD 活动）".to_string(),
    }
}

fn empty_provider_mode_text(provider: &str, mode: ProviderPaneMode) -> String {
    match mode {
        ProviderPaneMode::Screen => format!(" （{provider} 还没有屏幕镜像输出）"),
        ProviderPaneMode::Raw => format!(" （{provider} 还没有原始终端输出）"),
        ProviderPaneMode::Timeline => format!(" （{provider} 还没有过程时间线）"),
    }
}

fn escape_raw_terminal_entry(entry: &str) -> String {
    let mut escaped = String::new();
    for ch in entry.chars() {
        match ch {
            '\x1b' => escaped.push_str("\\x1b"),
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\x{:02x}", c as u32)),
            other => escaped.push(other),
        }
    }
    escaped
}

fn build_host_status_spans(host_surface: &HostSurfaceState) -> Vec<Span<'static>> {
    let readiness = host_surface.readiness();
    let mut spans = vec![
        Span::styled(" host:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            host_surface.label().to_string(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", readiness.label()),
            Style::default()
                .fg(host_readiness_color(readiness))
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if let Some(notes) = &host_surface.notes
        && !notes.is_empty()
    {
        spans.push(Span::styled(
            format!(" ({notes})"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans
}

fn event_log_color(event: &str) -> Color {
    if event.contains("FAILED")
        || event.contains("cancelled")
        || event.contains("失败")
        || event.contains("已取消")
    {
        Color::Red
    } else if is_hyard_event_text(event) {
        Color::Magenta
    } else if event.contains("peer/") {
        Color::Yellow
    } else if event.contains("core/") {
        Color::Cyan
    } else {
        Color::DarkGray
    }
}

fn hyard_job_color(job: &HyardJobSummary) -> Color {
    match (job.status.as_str(), job.source) {
        ("completed", HyardJobSource::Live) => Color::Cyan,
        ("completed", HyardJobSource::Inferred) => Color::Blue,
        ("completed", _) => Color::Green,
        ("failed", _) | ("cancelled", _) => Color::Red,
        ("cancel_requested", _) => Color::Magenta,
        ("queued", _) | ("running", _) => {
            if matches!(job.source, HyardJobSource::Live) {
                Color::Cyan
            } else if matches!(job.source, HyardJobSource::Inferred) {
                Color::Blue
            } else if job.wait_timeout_count > 0 {
                Color::Magenta
            } else {
                Color::Yellow
            }
        }
        _ => Color::DarkGray,
    }
}

fn format_hyard_job_entry(job: &HyardJobSummary) -> String {
    let mut pieces = vec![format!(
        "[hyard/job/{}] {} {} [{}]",
        job.short_job_id(),
        job.provider,
        job.status_badge(),
        job.source_badge()
    )];

    if let Some(event) = job.last_event.as_deref() {
        pieces.push(event_preview(event, 28));
    }

    if let Some(preview) = job.last_output_preview.as_deref() {
        pieces.push(event_preview(preview, 36));
    } else if let Some(error) = job.error.as_deref() {
        pieces.push(event_preview(error, 36));
    }

    if job.artifact_count > 0 {
        pieces.push(format!("artifacts={}", job.artifact_count));
    }

    pieces.join(" | ")
}

fn format_hyard_job_entries(job: &HyardJobSummary, expand: bool) -> Vec<String> {
    let mut lines = vec![format_hyard_job_entry(job)];

    if expand && let Some(execution) = &job.execution {
        lines.push(format!(
            "  原始命令: {}",
            event_preview(&execution.original_command, 80)
        ));
        lines.push(format!(
            "  解析命令: {}",
            event_preview(&execution.resolved_command, 80)
        ));
        lines.push(format!(
            "  实际执行: {}",
            event_preview(&execution.actual_display, 80)
        ));
        lines.push(format!(
            "  npm改写: {}",
            if execution.used_npm_wrapper_rewrite {
                "是"
            } else {
                "否"
            }
        ));
        if let Some(node_path) = execution.node_path.as_deref() {
            lines.push(format!("  node 路径: {}", event_preview(node_path, 80)));
        }
        if let Some(js_entry) = execution.js_entry.as_deref() {
            lines.push(format!("  js 入口: {}", event_preview(js_entry, 80)));
        }
    }

    lines
}

fn hyard_status_suffix(active: usize, waiting: usize, inferred: usize) -> String {
    if active == 0 && inferred == 0 {
        String::new()
    } else if active == 0 {
        format!(" | hyard:{inferred} 待确认")
    } else if waiting == 0 {
        if inferred == 0 {
            format!(" | hyard:{active} 活跃")
        } else {
            format!(" | hyard:{active} 活跃/{inferred} 待确认")
        }
    } else {
        if inferred == 0 {
            format!(" | hyard:{active} 活跃(含{waiting}个超时等待)")
        } else {
            format!(" | hyard:{active} 活跃(含{waiting}个超时等待)/{inferred} 待确认")
        }
    }
}

async fn probe_core_host_surface(
    provider_name: &str,
    registry: &ProviderRegistry,
    config_providers: &std::collections::HashMap<String, switchyard_config::ProviderConfig>,
) -> Result<HostSurfaceProbe, String> {
    let provider = registry
        .create(provider_name, config_providers.get(provider_name))
        .ok_or_else(|| format!("provider '{provider_name}' not registered"))?;

    provider
        .probe()
        .await
        .map(|result| result.host_surface)
        .map_err(|error| error.to_string())
}

fn event_preview(text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        "-".to_string()
    } else {
        preview_chars(text, max_chars, "...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use switchyard_host_jobs::{HostJobState, HostJobStatus, HostJobStore};
    use switchyard_provider_api::{
        ExecutionTelemetry, HostSurfaceKind, HostSurfaceProbe, PeerDescriptor,
    };

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
    fn apply_core_host_surface_probe_updates_runtime_and_emits_readiness_event() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        let probe = HostSurfaceProbe::ready(HostSurfaceKind::Skill);

        app.apply_core_host_surface_probe(Ok(probe));

        assert_eq!(app.runtime.core_host_surface.kind, HostSurfaceKind::Skill);
        assert_eq!(
            app.runtime.core_host_surface.readiness(),
            HostSurfaceReadiness::Ready
        );
        assert!(
            app.runtime
                .event_log
                .back()
                .is_some_and(|line| line.contains("[hyard] 主代理 host surface：skill [就绪]"))
        );
    }

    #[test]
    fn sync_peer_host_surface_uses_active_peer_catalog_surface() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.runtime.current_peer = Some("claude".to_string());

        let mut catalog = PeerCatalog::new();
        catalog.add(PeerDescriptor {
            provider_id: "claude".to_string(),
            roles: vec![],
            available: true,
            capabilities: vec![],
            description: "Claude CLI".to_string(),
            host_surface: Some(HostSurfaceProbe::ready(HostSurfaceKind::NativeSlash)),
        });

        app.sync_peer_host_surface(&catalog);

        assert!(app.runtime.peer_host_surface.is_some());
        assert_eq!(
            app.runtime.active_host_surface().kind,
            HostSurfaceKind::NativeSlash
        );
        assert_eq!(
            app.runtime.active_host_surface().readiness(),
            HostSurfaceReadiness::Ready
        );
    }

    #[test]
    fn sync_peer_catalog_state_updates_ready_counts() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        let mut catalog = PeerCatalog::new();
        catalog.add(PeerDescriptor {
            provider_id: "claude".to_string(),
            roles: vec![],
            available: true,
            capabilities: vec![],
            description: "Claude CLI".to_string(),
            host_surface: Some(HostSurfaceProbe::ready(HostSurfaceKind::NativeSlash)),
        });
        catalog.add(PeerDescriptor {
            provider_id: "gemini".to_string(),
            roles: vec![],
            available: true,
            capabilities: vec![],
            description: "Gemini CLI".to_string(),
            host_surface: Some(HostSurfaceProbe {
                kind: HostSurfaceKind::NativeCustomCommand,
                installed: true,
                configured: false,
                discoverable: true,
                notes: vec!["needs config".to_string()],
            }),
        });

        app.sync_peer_catalog_state(&catalog, true);

        assert_eq!(app.runtime.peer_ready_count, 1);
        assert_eq!(app.runtime.peer_total_count, 2);
        assert!(app.runtime.peer_probe_done);
    }

    #[test]
    fn message_view_key_shortcuts_switch_tabs_and_preserve_per_tab_scroll() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        let peer_view = MessageView::Provider("claude".to_string());
        app.runtime.current_peer = Some("claude".to_string());
        app.runtime.provider_view_lines.insert(
            "codex".to_string(),
            VecDeque::from(["planning".to_string()]),
        );
        app.runtime.provider_view_order.push("codex".to_string());
        app.runtime.provider_view_lines.insert(
            "claude".to_string(),
            VecDeque::from(["researching".to_string()]),
        );
        app.runtime.provider_view_order.push("claude".to_string());

        app.message_scroll_mut_for(MessageView::Overview).sync(20);
        app.message_scroll_mut_for(MessageView::Overview)
            .scroll_by(-8);
        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Timeline)
            .sync(14);
        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Timeline)
            .scroll_by(-3);
        app.focus = Focus::Transcript;

        let mut pending = None;
        app.handle_key_idle(KeyCode::Char('3'), KeyModifiers::NONE, &mut pending);

        assert_eq!(app.message_view, peer_view);
        assert_eq!(app.current_message_scroll().offset, 11);

        app.handle_key_running(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(app.message_view, MessageView::Hyard);

        app.handle_key_running(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(app.message_view, MessageView::Overview);
        assert_eq!(app.current_message_scroll().offset, 12);
    }

    #[test]
    fn provider_mode_shortcuts_switch_modes_and_preserve_per_mode_scroll() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.runtime
            .provider_terminal_lines
            .insert("claude".to_string(), VecDeque::from(["hi\r\n".to_string()]));
        app.runtime.provider_view_lines.insert(
            "claude".to_string(),
            VecDeque::from(["timeline".to_string()]),
        );
        app.runtime.provider_view_order.push("claude".to_string());
        app.message_view = MessageView::Provider("claude".to_string());
        app.focus = Focus::Transcript;

        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Screen)
            .sync(30);
        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Screen)
            .scroll_by(-5);
        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Timeline)
            .sync(10);
        app.provider_mode_scroll_mut_for("claude", ProviderPaneMode::Timeline)
            .scroll_by(-2);

        app.handle_key_running(KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(
            app.current_provider_mode("claude"),
            ProviderPaneMode::Timeline
        );
        assert_eq!(app.current_message_scroll().offset, 8);

        app.handle_key_running(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            app.current_provider_mode("claude"),
            ProviderPaneMode::Screen
        );
        assert_eq!(app.current_message_scroll().offset, 25);
    }

    #[test]
    fn numeric_input_does_not_switch_tabs_while_focus_is_input() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.runtime.provider_view_lines.insert(
            "codex".to_string(),
            VecDeque::from(["planning".to_string()]),
        );
        app.runtime.provider_view_order.push("codex".to_string());
        app.runtime.current_peer = Some("claude".to_string());
        app.runtime.provider_view_lines.insert(
            "claude".to_string(),
            VecDeque::from(["working".to_string()]),
        );
        app.runtime.provider_view_order.push("claude".to_string());
        app.focus = Focus::Input;

        let mut pending = None;
        app.handle_key_idle(KeyCode::Char('3'), KeyModifiers::NONE, &mut pending);

        assert_eq!(app.input, "3");
        assert_eq!(app.message_view, MessageView::Overview);
    }

    #[test]
    fn focus_changes_refresh_idle_footer_status_immediately() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.update_idle_status();
        assert!(app.status.contains("focus:输入框"));

        let mut pending = None;
        app.handle_key_idle(KeyCode::Tab, KeyModifiers::NONE, &mut pending);
        assert_eq!(app.focus, Focus::Transcript);
        assert!(app.status.contains("focus:主消息区"));

        app.handle_key_idle(KeyCode::Tab, KeyModifiers::NONE, &mut pending);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.status.contains("focus:右侧面板"));
    }

    #[test]
    fn message_view_change_refreshes_idle_footer_hint_immediately() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.runtime.provider_view_lines.insert(
            "codex".to_string(),
            VecDeque::from(["planning".to_string()]),
        );
        app.runtime.provider_view_order.push("codex".to_string());
        app.update_idle_status();

        app.set_message_view(MessageView::Provider("codex".to_string()));

        assert!(app.status.contains("focus:输入框"));
        assert!(app.status.contains("S:屏幕 R:原始 T:时间线 / F3:循环模式"));
    }

    #[test]
    fn explicit_error_status_is_not_overwritten_by_focus_change() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.handle_turn_error("boom");
        assert_eq!(app.status, "回合失败：boom");

        app.set_focus(Focus::Transcript);

        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.status, "回合失败：boom");
    }

    #[test]
    fn mouse_click_can_switch_dynamic_provider_tabs() {
        use ratatui::backend::TestBackend;

        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.runtime.provider_view_lines.insert(
            "codex".to_string(),
            VecDeque::from(["planning".to_string()]),
        );
        app.runtime.provider_view_order.push("codex".to_string());
        app.runtime.current_peer = Some("claude".to_string());
        app.runtime.provider_view_lines.insert(
            "claude".to_string(),
            VecDeque::from(["working".to_string()]),
        );
        app.runtime.provider_view_order.push("claude".to_string());

        let backend = TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();

        let target = app
            .message_tab_hitboxes
            .iter()
            .find(|(_, view)| *view == MessageView::Provider("claude".to_string()))
            .map(|(rect, _)| *rect)
            .expect("claude tab should exist");

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.x,
            row: target.y,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(
            app.message_view,
            MessageView::Provider("claude".to_string())
        );
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn transcript_scroll_helpers_follow_latest_and_preserve_manual_offset() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));

        app.sync_current_message_scroll(24);
        assert!(app.current_message_scroll().follow_latest);
        assert_eq!(app.current_message_scroll().offset, 24);

        app.scroll_message_by(-5);
        assert!(!app.current_message_scroll().follow_latest);
        assert_eq!(app.current_message_scroll().offset, 19);

        app.sync_current_message_scroll(30);
        assert_eq!(app.current_message_scroll().offset, 19);
        assert!(!app.current_message_scroll().follow_latest);
        assert!(app.current_message_scroll().has_unseen);

        app.scroll_to_latest();
        assert!(app.current_message_scroll().follow_latest);
        assert_eq!(app.current_message_scroll().offset, 30);
        assert!(!app.current_message_scroll().has_unseen);
    }

    #[test]
    fn right_pane_scroll_helpers_are_independent_per_panel() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.sync_current_right_pane_scroll(18);
        app.scroll_right_pane_by(-4);
        assert_eq!(app.current_right_pane_scroll().offset, 14);

        app.advance_right_pane();
        app.sync_current_right_pane_scroll(9);
        assert_eq!(app.current_right_pane_scroll().offset, 9);

        app.advance_right_pane();
        app.advance_right_pane();
        assert_eq!(app.right_pane, RightPane::Events);
        assert_eq!(app.current_right_pane_scroll().offset, 14);
    }

    #[test]
    fn compose_hint_messages_reports_unseen_main_and_sidebar_content() {
        let mut app = App::new("codex".to_string(), PathBuf::from("."));
        app.sync_current_message_scroll(20);
        app.scroll_message_by(-5);
        app.sync_current_message_scroll(26);

        app.sync_current_right_pane_scroll(10);
        app.scroll_right_pane_by(-3);
        app.sync_current_right_pane_scroll(15);

        let hints = app.compose_hint_messages();
        assert!(hints.iter().any(|hint| hint.contains("总览 视图有新消息")));
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("右侧事件面板有新内容"))
        );
    }

    #[test]
    fn max_scroll_for_lines_accounts_for_wrapping_and_visible_height() {
        let lines = vec![
            Line::from("1234567890"),
            Line::from("abcdefghij"),
            Line::from("klmnopqrst"),
        ];
        let area = Rect::new(0, 0, 8, 4);

        assert_eq!(max_scroll_for_lines(&lines, area), 4);
    }

    #[test]
    fn collect_message_view_entries_filters_provider_and_hyard_channels() {
        let mut runtime = RuntimeState::new("codex");
        let core_turn_id = Uuid::now_v7();
        let peer_turn_id = Uuid::now_v7();
        runtime.apply(&RuntimeEvent::CoreTurnStarted {
            turn_id: core_turn_id,
            provider: "codex".to_string(),
        });
        runtime.apply(&RuntimeEvent::CoreItemUpdated {
            turn_id: core_turn_id,
            provider: "codex".to_string(),
            text: "planning".to_string(),
        });
        runtime.apply(&RuntimeEvent::DelegateRequested {
            core_turn_id,
            peer: "claude".to_string(),
            role: "reviewer".to_string(),
            task_summary: "review".to_string(),
        });
        runtime.apply(&RuntimeEvent::PeerTurnStarted {
            turn_id: peer_turn_id,
            provider: "claude".to_string(),
        });
        runtime.apply(&RuntimeEvent::PeerItemUpdated {
            turn_id: peer_turn_id,
            provider: "claude".to_string(),
            text: "[assistant]".to_string(),
        });
        runtime.set_hyard_jobs(vec![HyardJobSummary {
            job_id: "job-12345678".to_string(),
            provider: "claude".to_string(),
            status: "running".to_string(),
            last_event: Some("item_updated:claude".to_string()),
            last_output_preview: Some("researching".to_string()),
            execution: None,
            wait_timeout_count: 1,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }]);
        runtime.push_event("[hyard] delegate -> claude (reviewer): review".to_string());

        let core_lines =
            collect_message_view_entries(&MessageView::Provider("codex".to_string()), &runtime);
        assert!(core_lines.iter().any(|line| line.contains("planning")));
        assert!(
            core_lines
                .iter()
                .any(|line| line.contains("已委托给 claude"))
        );

        let peer_lines =
            collect_message_view_entries(&MessageView::Provider("claude".to_string()), &runtime);
        assert!(peer_lines.iter().any(|line| line.contains("收到委托")));
        assert!(peer_lines.iter().any(|line| line.contains("[assistant]")));

        let hyard_lines = collect_message_view_entries(&MessageView::Hyard, &runtime);
        assert!(hyard_lines.iter().any(|line| {
            line.contains("[hyard/job/job-1234] claude 运行中·w1 [缓存]")
                && line.contains("researching")
        }));
        assert!(
            hyard_lines
                .iter()
                .any(|line| line.contains("delegate -> claude (reviewer): review"))
        );
    }

    #[test]
    fn collect_message_view_entries_expands_hyard_execution_details() {
        let mut runtime = RuntimeState::new("codex");
        runtime.set_hyard_jobs(vec![HyardJobSummary {
            job_id: "job-12345678".to_string(),
            provider: "gemini".to_string(),
            status: "running".to_string(),
            last_event: Some("execution_resolved:gemini".to_string()),
            last_output_preview: Some("researching".to_string()),
            execution: Some(sample_execution()),
            wait_timeout_count: 0,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }]);

        let lines = collect_message_view_entries(&MessageView::Hyard, &runtime);

        assert!(lines.iter().any(|line| line.contains("原始命令: gemini")));
        assert!(lines.iter().any(|line| {
            line.contains("解析命令: C:\\Users\\demo\\AppData\\Roaming\\npm\\gemini.cmd")
        }));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("实际执行: C:\\Program Files\\nodejs\\node.exe"))
        );
        assert!(lines.iter().any(|line| line.contains("npm改写: 是")));
        assert!(lines.iter().any(|line| line.contains("node 路径:")));
        assert!(lines.iter().any(|line| line.contains("js 入口:")));
    }

    #[test]
    fn provider_message_view_prefers_terminal_transcript_when_available() {
        let mut runtime = RuntimeState::new("codex");
        runtime.apply(&RuntimeEvent::CoreItemUpdated {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
            text: "timeline entry".to_string(),
        });
        runtime.apply(&RuntimeEvent::CoreTerminalOutput {
            turn_id: Uuid::now_v7(),
            provider: "codex".to_string(),
            text: "terminal line".to_string(),
            transport: Some("pty".to_string()),
        });

        let lines =
            collect_message_view_entries(&MessageView::Provider("codex".to_string()), &runtime);

        assert_eq!(lines, vec!["terminal line".to_string()]);
    }

    #[test]
    fn provider_message_view_falls_back_to_hyard_job_entries_for_active_job() {
        let mut runtime = RuntimeState::new("codex");
        runtime.set_hyard_jobs(vec![HyardJobSummary {
            job_id: "job-gemini-1234".to_string(),
            provider: "gemini".to_string(),
            status: "running".to_string(),
            last_event: Some("awaiting_result".to_string()),
            last_output_preview: Some("collecting sources".to_string()),
            execution: Some(sample_execution()),
            wait_timeout_count: 1,
            artifact_count: 2,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }]);

        let lines =
            collect_message_view_entries(&MessageView::Provider("gemini".to_string()), &runtime);

        assert!(lines.iter().any(|line| line.contains("gemini 运行中·w1")));
        assert!(lines.iter().any(|line| line.contains("collecting sources")));
        assert!(lines.iter().any(|line| line.contains("原始命令: gemini")));
    }

    #[test]
    fn refresh_hyard_jobs_reads_job_manifests_into_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join(".switchyard").join("sessions");
        let job_dir = dir.path().join(".switchyard").join("jobs");
        fs::create_dir_all(&session_dir).unwrap();
        fs::create_dir_all(&job_dir).unwrap();
        let store = HostJobStore::new(job_dir.clone());
        let mut job = HostJobState::new("codex", "hello", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.last_event = Some("item_updated:codex".to_string());
        job.last_output_preview = Some("hello".to_string());
        job.wait_timeout_count = 1;
        job.artifact_count = 2;
        store.save(&job).unwrap();

        let mut app = App::with_paths("codex".to_string(), session_dir, job_dir);
        app.refresh_hyard_jobs();

        assert_eq!(app.runtime.hyard_jobs.len(), 1);
        assert_eq!(app.runtime.active_hyard_job_count, 1);
        assert_eq!(app.runtime.waiting_hyard_job_count, 1);
        assert_eq!(
            app.runtime
                .primary_hyard_job()
                .map(|job| job.provider.as_str()),
            Some("codex")
        );
    }

    #[test]
    fn build_host_status_spans_include_readiness_badge_and_color() {
        let state = HostSurfaceState::from_probe(&HostSurfaceProbe {
            kind: HostSurfaceKind::Plugin,
            installed: true,
            configured: false,
            discoverable: true,
            notes: vec!["needs config".to_string()],
        });

        let spans = build_host_status_spans(&state);
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(text.contains("host:"));
        assert!(text.contains("plugin"));
        assert!(text.contains("[部分可用]"));
        assert_eq!(spans[3].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn build_peer_probe_spans_show_probing_and_ready_states() {
        let probing = build_peer_probe_spans(0, 2, false);
        let probing_text = probing
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(probing_text.contains("[探测中 2]"));
        assert_eq!(probing[1].style.fg, Some(Color::Blue));

        let ready = build_peer_probe_spans(2, 2, true);
        let ready_text = ready
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(ready_text.contains("[2/2 就绪]"));
        assert_eq!(ready[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn host_readiness_color_maps_all_states() {
        assert_eq!(
            host_readiness_color(HostSurfaceReadiness::Ready),
            Color::Green
        );
        assert_eq!(
            host_readiness_color(HostSurfaceReadiness::Partial),
            Color::Yellow
        );
        assert_eq!(
            host_readiness_color(HostSurfaceReadiness::Unavailable),
            Color::Red
        );
        assert_eq!(
            host_readiness_color(HostSurfaceReadiness::Unknown),
            Color::DarkGray
        );
    }

    #[test]
    fn event_log_color_prioritizes_hyard_and_failure_classes() {
        assert_eq!(
            event_log_color("[hyard] delegate -> claude (reviewer): review"),
            Color::Magenta
        );
        assert_eq!(event_log_color("[peer/claude] 已开始执行"), Color::Yellow);
        assert_eq!(event_log_color("[core/codex] 已开始处理"), Color::Cyan);
        assert_eq!(event_log_color("[core/codex] 失败：boom"), Color::Red);
    }

    #[test]
    fn build_hyard_primary_job_spans_show_provider_status_and_job_id() {
        let spans = build_hyard_primary_job_spans(Some(&HyardJobSummary {
            job_id: "019d5709-f2b1-7002-8643-67a616f32d71".to_string(),
            provider: "claude".to_string(),
            status: "running".to_string(),
            last_event: None,
            last_output_preview: None,
            execution: None,
            wait_timeout_count: 2,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        }));

        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("claude 运行中·w2"));
        assert!(text.contains("缓存"));
        assert!(text.contains("[019d5709]"));
    }

    #[test]
    fn build_hyard_job_execution_spans_show_rewritten_command_preview() {
        let execution = sample_execution();
        let job = HyardJobSummary {
            job_id: "019d5709-f2b1-7002-8643-67a616f32d71".to_string(),
            provider: "gemini".to_string(),
            status: "running".to_string(),
            last_event: None,
            last_output_preview: None,
            execution: Some(execution),
            wait_timeout_count: 0,
            artifact_count: 0,
            result_ready: false,
            error: None,
            updated_at: "2026-04-04T12:00:00Z".to_string(),
            source: HyardJobSource::Store,
        };

        let spans = build_hyard_job_execution_spans(Some(&job));
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(text.contains("jobcmd:"));
        assert!(text.contains("gemini.cmd"));
        assert!(text.contains("[npm→node]"));
        assert!(text.contains("index.js"));
    }
}
