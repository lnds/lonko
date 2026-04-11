use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event as CrosstermEvent, EventStream, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::interval;

use crate::{
    control::{ghostty, tmux, tmux::tmux_session_for_pane},
    event::Event,
    focus,
    sources::{hooks, hooks::HookPayload, lifecycle, transcript, tmux_scanner},
    state::{AppState, KeyOutcome, Session, SessionStatus, Tab},
    ui,
};

// ── Pure helpers (testable without App) ────────────────────────────────────────

/// Compute the scroll offset for a list view so the selected item stays centred.
pub fn compute_scroll_offset(selected: usize, total: usize, visible: usize) -> usize {
    if total == 0 || visible == 0 {
        return 0;
    }
    let half = visible / 2;
    if selected < half {
        0
    } else if selected + (visible - half) >= total {
        total.saturating_sub(visible)
    } else {
        selected - half
    }
}

/// Write the no-follow sentinel so lonko-follow.sh skips the next hook trigger.
pub fn write_no_follow_sentinel() {
    let sentinel = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("lonko-no-follow");
    let _ = std::fs::write(&sentinel, "");
}

/// Map a hook event name to a SessionStatus update.
/// Returns `None` for unknown events (caller should leave status unchanged).
pub fn hook_event_to_status(
    event_name: &str,
    payload: &HookPayload,
    session: &mut Session,
) -> Option<SessionStatus> {
    match event_name {
        "SessionStart" => Some(SessionStatus::Idle),
        "UserPromptSubmit" => {
            if let Some(p) = &payload.prompt {
                let text = p.trim();
                if !text.is_empty() {
                    session.last_prompt = Some(text.to_string());
                }
            }
            Some(SessionStatus::Running)
        }
        "PreToolUse" => {
            let tool = payload.tool_name.clone().unwrap_or_else(|| "?".into());
            session.last_tool = Some(tool.clone());
            Some(SessionStatus::RunningTool(tool))
        }
        "PostToolUse" => Some(SessionStatus::Running),
        "Stop" | "SubagentStop" => {
            let path = session.transcript_path.clone()
                .map(std::path::PathBuf::from)
                .or_else(|| Some(transcript::transcript_path(&session.cwd, &session.id)));
            if let Some(path) = path {
                if let Some(mut info) = transcript::read_latest(&path) {
                    info.branch = transcript::git_branch(&session.cwd).or(info.branch);
                    session.apply_transcript_info(info);
                } else {
                    let live = transcript::git_branch(&session.cwd);
                    if live.is_some() { session.branch = live; }
                }
            }
            Some(SessionStatus::Idle)
        }
        "SessionEnd" => {
            session.completed_at = Some(std::time::Instant::now());
            Some(SessionStatus::Completed)
        }
        "Notification" => {
            let msg = payload.message.clone().unwrap_or_default();
            match payload.notification_type.as_deref() {
                Some("permission_prompt") => Some(SessionStatus::WaitingForUser(msg)),
                _ => Some(SessionStatus::WaitingForInput),
            }
        }
        _ => None,
    }
}

/// Send a desktop notification when a session needs attention.
pub fn notify_if_needed(project_name: &str, status: &SessionStatus) {
    let (summary, body) = match status {
        SessionStatus::WaitingForUser(msg) => {
            (format!("lonko · {} ⚠", project_name), msg.clone())
        }
        SessionStatus::WaitingForInput => {
            (format!("lonko · {}", project_name), "listo, esperando tu input".into())
        }
        _ => return,
    };
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .summary(&summary)
            .body(&body)
            .timeout(notify_rust::Timeout::Milliseconds(8000))
            .show();
    });
}

pub struct App {
    pub state: AppState,
    /// Channel sender stored so handle_event can trigger scans from the tick handler.
    scan_tx: Option<tokio::sync::mpsc::UnboundedSender<Event>>,
    /// Last mouse click: (row, instant) for double-click detection.
    last_click: Option<(u16, std::time::Instant)>,
    /// Monotonic counter shared with pending focus tasks; increment to cancel stale spawns.
    focus_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl App {
    pub fn new() -> Self {
        let mut state = AppState::default();
        state.bookmarks = crate::state::load_bookmarks();
        Self {
            state,
            scan_tx: None,
            last_click: None,
            focus_gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut tick_interval = interval(Duration::from_millis(100));
        let mut crossterm_events = EventStream::new();

        let (tx, mut rx) = unbounded_channel::<Event>();
        self.scan_tx = Some(tx.clone());

        // Store initial terminal size
        if let Ok((w, h)) = crossterm::terminal::size() {
            self.state.term_width = w;
            self.state.term_height = h;
        }

        // Capture own tmux pane ID so we can follow the user between windows.
        self.state.own_pane = std::env::var("TMUX_PANE").ok();

        // Scan existing session files (may be empty/absent — kept as fallback).
        lifecycle::scan_existing(&tx);

        // Bootstrap: scan tmux panes for claude processes immediately.
        // This detects sessions that already exist before any hook fires.
        tmux_scanner::scan(&tx, &[], self.state.own_pane.as_deref());

        // Initialize focused_session_id immediately from the active tmux pane,
        // so the highlight is visible from the first frame without waiting for the poll tick.
        if let Some(active) = tmux::active_pane() {
            let is_own = self.state.own_pane.as_deref() == Some(active.as_str());
            if !is_own {
                self.state.focused_session_id = self.state.sessions.iter()
                    .find(|s| s.tmux_pane.as_deref() == Some(active.as_str()))
                    .map(|s| s.id.clone());
            }
        }

        // Read focus-pane hint written by lonko-panel.sh
        let focus_file = dirs::home_dir()
            .unwrap_or_default()
            .join(".cache/lonko-focus-pane");
        if let Ok(pane) = std::fs::read_to_string(&focus_file) {
            let pane = pane.trim().to_string();
            if !pane.is_empty() {
                self.state.focus_pane = Some(pane);
                let _ = std::fs::remove_file(&focus_file);
            }
        }

        // Spawn the filesystem watcher (must be kept alive)
        let _watcher = lifecycle::spawn_watcher(tx.clone())
            .map_err(|e| color_eyre::eyre::eyre!(e))?;

        // Spawn the Unix socket listener for hook events
        hooks::spawn_listener(tx.clone())
            .map_err(|e| color_eyre::eyre::eyre!(e))?;

        loop {
            terminal.draw(|frame| ui::render(frame, &self.state))?;

            let event = tokio::select! {
                _ = tick_interval.tick() => Event::Tick,
                Some(ev) = rx.recv() => ev,
                maybe_event = crossterm_events.next() => {
                    match maybe_event {
                        Some(Ok(CrosstermEvent::Key(key))) => Event::Key(key),
                        Some(Ok(CrosstermEvent::Mouse(mouse))) => Event::Mouse(mouse),
                        Some(Ok(CrosstermEvent::Resize(w, h))) => Event::Resize(w, h),
                        Some(Ok(CrosstermEvent::FocusGained)) => Event::FocusGained,
                        Some(Ok(CrosstermEvent::FocusLost)) => Event::FocusLost,
                        _ => continue,
                    }
                }
            };

            if self.handle_event(event)? {
                break;
            }
        }

        Ok(())
    }

    fn handle_mouse_click(&mut self, col: u16, row: u16) {
        let _col = col;
        let h = self.state.term_height;

        // Header click (rows 0-2): switch tabs based on column position.
        // Tab labels in the inner row (row 1): "Agents" (cols 1-6) │ "Sessions" (cols 10-17)
        // Boundary at col 9 (middle of the divider " │ ").
        if row < 3 {
            if row == 1 {
                if _col <= 9 {
                    self.state.active_tab = Tab::Agents;
                } else {
                    self.state.active_tab = Tab::Sessions;
                }
            }
            return;
        }

        if row >= h.saturating_sub(1) {
            return;
        }

        if self.state.active_tab == Tab::Sessions {
            self.handle_mouse_click_sessions(row);
            return;
        }

        // Agents tab: variable-height cards (main=5+1, sub=3+1), starts at y=3
        let visible = self.state.visible_sessions();
        let total = visible.len();
        if total == 0 { return; }

        let list_h = h.saturating_sub(3 + 1);
        // Approximate cards visible and scroll using uniform stride as heuristic
        let approx_visible = ((list_h + 1) / 6).max(1) as usize;
        let cards_visible = approx_visible.min(total);
        let scroll = compute_scroll_offset(self.state.selected, total, cards_visible);

        // Linear scan to find which card was clicked based on row offset from y=3
        let click_y = row - 3;
        let mut y_acc: u16 = 0;
        let mut card_idx: Option<usize> = None;
        for (i, s) in visible[scroll..].iter().enumerate() {
            let ch = if s.is_subagent() { 3u16 } else { 5u16 };
            if click_y >= y_acc && click_y < y_acc + ch {
                card_idx = Some(i);
                break;
            }
            y_acc += ch + 1; // card height + separator
        }

        let global_idx = match card_idx {
            Some(idx) => scroll + idx,
            None => return,
        };
        if global_idx >= total {
            return;
        }

        // Double-click detection: two clicks on the same row within 400ms → focus
        let now = std::time::Instant::now();
        let is_double = self.last_click
            .as_ref()
            .is_some_and(|(last_row, last_time)| {
                *last_row == row && now.duration_since(*last_time).as_millis() < 400
            });
        self.last_click = Some((row, now));

        if is_double {
            self.state.selected = global_idx;
            let session = self.state.selected_session().cloned();
            if let Some(session) = session {
                let pid = session.pid;
                let session_id = session.id.clone();
                let pane = session.tmux_pane.clone()
                    .or_else(|| tmux::find_pane_for_pid(pid));
                if let Some(p) = pane {
                    if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                        s.tmux_pane = Some(p.clone());
                    }
                    self.state.focused_session_id = Some(session_id);
                    // Repetir select-pane durante 300ms para ganarle a tmux mouse-mode.
                    // Cada MouseUp/MouseDown re-selecciona lonko; nosotros lo sobreescribimos
                    // repetidamente hasta que no haya más eventos de mouse pendientes.
                    use std::sync::atomic::Ordering;
                    let my_gen = self.focus_gen.fetch_add(1, Ordering::SeqCst) + 1;
                    let gen_arc = self.focus_gen.clone();
                    tokio::spawn(async move {
                        for delay_ms in [30u64, 60, 100, 160, 240] {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            if gen_arc.load(Ordering::SeqCst) != my_gen { break; }
                            let _ = std::process::Command::new("tmux")
                                .args(["select-pane", "-t", &p])
                                .status();
                            let _ = std::process::Command::new("tmux")
                                .args(["switch-client", "-t", &p])
                                .status();
                        }
                    });
                }
            }
        } else {
            self.state.selected = global_idx;
        }
    }

    fn handle_mouse_click_sessions(&mut self, row: u16) {
        let list_top: u16 = 3;
        if row < list_top {
            return;
        }
        let row_in_list = row - list_top;

        let list_h = self.state.term_height.saturating_sub(list_top + 1);
        if self.state.tmux_sessions.is_empty() {
            return;
        }

        let page = crate::ui::tmux_sessions::session_page_layout(
            &self.state.tmux_sessions,
            self.state.tmux_selected,
            self.state.tmux_expanded,
            list_h,
        );

        // Find which card (and row within it) was clicked.
        let hit = page.iter().find(|c| {
            row_in_list >= c.row_start && row_in_list < c.row_start + c.card_h
        });
        let Some(card) = hit else { return };

        let global_idx = card.global_idx;
        let row_within_card = row_in_list - card.row_start;

        // Double-click detection: two clicks on the same row within 400ms.
        let now = std::time::Instant::now();
        let is_double = self.last_click
            .as_ref()
            .is_some_and(|(last_row, last_time)| {
                *last_row == row && now.duration_since(*last_time).as_millis() < 400
            });
        self.last_click = Some((row, now));

        let is_selected = global_idx == self.state.tmux_selected;
        let is_expanded = is_selected && self.state.tmux_expanded;

        // Check if click lands on a window row within an expanded card.
        // Layout: rows 0-2 = header, rows 3..3+n = window rows, last row = activity bar.
        let window_row: Option<usize> = if is_expanded && row_within_card >= 3 {
            let n = self.state.tmux_sessions[global_idx].windows.len();
            let win_idx = (row_within_card - 3) as usize;
            if win_idx < n { Some(win_idx) } else { None }
        } else {
            None
        };

        if is_double {
            self.state.tmux_selected = global_idx;
            if let Some(win) = window_row {
                self.state.tmux_window_cursor = Some(win);
            }
            self.focus_tmux_session();
            return;
        }

        if !is_selected {
            // Click on unselected card: select + expand, clear window cursor.
            self.state.tmux_selected = global_idx;
            self.state.tmux_expanded = true;
            self.state.tmux_window_cursor = None;
        } else if let Some(win) = window_row {
            // Click on a window row in the already-selected expanded card.
            self.state.tmux_window_cursor = Some(win);
        } else {
            // Click on header area of the already-selected card: toggle expand.
            self.state.tmux_expanded = !self.state.tmux_expanded;
            self.state.tmux_window_cursor = None;
        }
    }

    /// Mouse wheel scroll: navigate the current tab's list by `delta` (+1 down, -1 up).
    fn handle_mouse_scroll(&mut self, delta: isize) {
        match self.state.active_tab {
            Tab::Agents => {
                if delta > 0 {
                    self.state.select_next();
                } else {
                    self.state.select_prev();
                }
            }
            Tab::Sessions => {
                self.state.navigate_tmux_session(delta);
            }
        }
    }

    /// Focus the selected tmux session (Sessions tab), optionally at a specific window.
    fn focus_tmux_session(&mut self) {
        let Some(session) = self.state.tmux_sessions.get(self.state.tmux_selected) else { return };
        let name = session.name.clone();
        if let Some(win_idx) = self.state.tmux_window_cursor {
            if let Some(window) = session.windows.get(win_idx) {
                let _ = tmux::focus_session_window(&name, window.index);
                return;
            }
        }
        let _ = std::process::Command::new("tmux")
            .args(["switch-client", "-t", &name])
            .status();
    }

    /// Returns true when lonko is the only pane left in its current tmux session,
    /// so it should exit cleanly instead of lingering as a solitary pane/window.
    /// Skips lonko-internal sessions (lonko-tray, floating-*) where lonko is meant
    /// to keep running in the background.
    fn should_self_quit_when_alone(&self) -> bool {
        let Some(own) = self.state.own_pane.as_deref() else { return false };
        let Some(session) = tmux::tmux_session_for_pane(own) else { return false };
        if session == "lonko-tray" || session.starts_with("floating-") {
            return false;
        }
        let panes = tmux::list_pane_ids_in_session(&session);
        // Session is empty (already torn down) or the only remaining pane is lonko's.
        panes.iter().all(|p| p == own)
    }

    /// Esconde el panel moviéndolo de vuelta a lonko-tray (lonko sigue corriendo).
    fn hide_panel(&self) {
        let Some(ref own) = self.state.own_pane else { return };

        // Capture the window id before break-pane so we can restore its layout.
        let win_id = std::process::Command::new("tmux")
            .args(["display-message", "-t", own, "-p", "#{window_id}"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Ensure lonko-tray exists
        let tray_exists = std::process::Command::new("tmux")
            .args(["has-session", "-t", "lonko-tray"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !tray_exists {
            let _ = std::process::Command::new("tmux")
                .args(["new-session", "-d", "-s", "lonko-tray"])
                .status();
        }

        let _ = std::process::Command::new("tmux")
            .args(["break-pane", "-d", "-s", own, "-t", "lonko-tray:"])
            .status();

        // Restore the window's saved layout (undoes the distortion that happened
        // when lonko was added to this window). Drops the layout file on success.
        if let Some(win) = win_id {
            let home = std::env::var("HOME").unwrap_or_default();
            let layout_path = format!("{home}/.cache/lonko-layouts/{win}.layout");
            if let Ok(layout) = std::fs::read_to_string(&layout_path) {
                let layout = layout.trim();
                if !layout.is_empty() {
                    let _ = std::process::Command::new("tmux")
                        .args(["select-layout", "-t", &win, layout])
                        .status();
                }
                let _ = std::fs::remove_file(&layout_path);
            }
        }
    }

    fn focus_selected(&mut self) {
        let Some(session) = self.state.selected_session() else { return };
        let pid = session.pid;
        let session_id = session.id.clone();
        let stored_pane = session.tmux_pane.clone();

        // Use stored pane or discover it by walking the process tree
        let pane = stored_pane.or_else(|| tmux::find_pane_for_pid(pid));

        if let Some(ref pane) = pane {
            tracing::debug!("focus_selected: pane={pane} pid={pid}");
            // Cache the discovered pane
            if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                s.tmux_pane = Some(pane.clone());
            }
            write_no_follow_sentinel();
            let _ = tmux::select_pane(pane);
            let _ = tmux::focus_pane(pane);
            self.state.focused_session_id = Some(session_id);
        } else {
            tracing::warn!("focus_selected: no pane found for pid={pid}, using select_last_pane");
            let _ = tmux::select_last_pane();
        }
    }

    fn refresh_selected_transcript(&mut self) {
        let selected = self.state.selected;
        let Some(session) = self.state.sessions.get(selected) else { return };
        let cwd = session.cwd.clone();
        let path = session.transcript_path.clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| transcript::transcript_path(&session.cwd, &session.id));
        let Some(mut info) = transcript::read_latest(&path) else { return };
        // Prefer live git branch over stale transcript value
        info.branch = transcript::git_branch(&cwd).or(info.branch);
        self.state.sessions[selected].apply_transcript_info(info);
    }

    /// Send a permission response to the first session waiting for user approval.
    /// Targets the first `WaitingForUser` session regardless of selection, so the
    /// user can grant permission without navigating to that session first.
    fn send_permission(&mut self, key: &str) {
        let waiting = self.state.sessions.iter().find(|s| s.status.is_waiting());
        let Some(session) = waiting else { return };
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));
        if let Some(ref p) = pane
            && let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id)
                && s.tmux_pane.is_none() {
                    s.tmux_pane = Some(p.clone());
                }
        let Some(pane) = pane else { return };
        let _ = tmux::send_keys(&pane, key);
    }

    fn handle_hook(&mut self, payload: crate::sources::hooks::HookPayload) {
        let parent_session_id = match &payload.session_id {
            Some(id) => id.clone(),
            None => return,
        };

        // Detect subagent: has a non-empty agent_type and agent_id
        let is_subagent = payload.agent_type.as_ref().is_some_and(|t| !t.is_empty());
        let effective_id = if is_subagent {
            match &payload.agent_id {
                Some(id) if !id.is_empty() => id.clone(),
                _ => return,
            }
        } else {
            parent_session_id.clone()
        };

        // Resolve the session: look up by session_id first, then by pane_id
        // (to promote provisional tmux-scan sessions), then by cwd as fallback.
        let hook_pane = payload.tmux_pane.as_deref().filter(|p| !p.is_empty());
        let hook_cwd  = payload.cwd.as_deref().filter(|c| !c.is_empty());

        if is_subagent {
            // For subagents, create a session entry if it doesn't exist yet
            if !self.state.sessions.iter().any(|s| s.id == effective_id) {
                let cwd = hook_cwd.unwrap_or_default().to_string();
                if cwd.is_empty() { return; }

                let parent_depth = self.state.sessions.iter()
                    .find(|s| s.id == parent_session_id)
                    .map(|s| s.depth)
                    .unwrap_or(0);

                let agent_type = payload.agent_type.as_deref().unwrap_or("sub");
                let mut session = Session::new(effective_id.clone(), 0, cwd);
                session.status = SessionStatus::Running;
                session.parent_id = Some(parent_session_id.clone());
                session.depth = (parent_depth + 1).min(2);
                session.project_name = agent_type.to_string();
                if let Some(pane) = hook_pane {
                    session.tmux_pane = Some(pane.to_string());
                }
                if let Some(tp) = payload.agent_transcript_path.as_deref().filter(|t| !t.is_empty()) {
                    session.transcript_path = Some(tp.to_string());
                }
                self.state.sessions.push(session);
            }
        } else {
            if !self.state.resolve_hook_session(
                &effective_id,
                hook_pane,
                hook_cwd,
                payload.transcript_path.as_deref(),
                hook_cwd.and_then(transcript::git_branch),
            ) {
                return;
            }
        }

        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|s| s.id == effective_id)
        else {
            return;
        };

        // Update tmux pane if available
        if let Some(pane) = &payload.tmux_pane
            && !pane.is_empty() {
                session.tmux_pane = Some(pane.clone());
            }

        // Cache transcript path (prefer agent_transcript_path for subagents)
        if is_subagent {
            if let Some(tp) = &payload.agent_transcript_path
                && !tp.is_empty() {
                    session.transcript_path = Some(tp.clone());
                }
        } else if let Some(tp) = &payload.transcript_path
            && !tp.is_empty() {
                session.transcript_path = Some(tp.clone());
            }

        // Update cwd if available (skip for subagents — they share the parent's cwd)
        if !is_subagent {
            if let Some(cwd) = &payload.cwd
                && !cwd.is_empty() && session.cwd != *cwd {
                    session.cwd = cwd.clone();
                    session.project_name = cwd.split('/').next_back().unwrap_or(cwd).to_string();
                }
        }

        session.last_activity = std::time::Instant::now();

        let event_name = payload.hook_event_name.as_deref().unwrap_or("");

        // SubagentStop for a subagent means it's done
        if is_subagent && event_name == "SubagentStop" {
            session.completed_at = Some(std::time::Instant::now());
            session.status = SessionStatus::Completed;
        } else {
            let Some(new_status) = hook_event_to_status(event_name, &payload, session) else {
                return; // unknown event, don't change state
            };
            session.status = new_status;
        }

        // Desktop notification when session needs attention and Ghostty is not in focus
        if !ghostty::has_focus() {
            notify_if_needed(&session.project_name, &session.status);
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Tick                                       => {
                self.on_tick();
                // Fallback auto-quit check: `TmuxPaneGone` only fires for panes lonko
                // had tracked as running Claude, so closing a plain shell pane wouldn't
                // trigger it. Poll every ~2s with a short grace period on startup.
                if self.state.tick >= 30
                    && self.state.tick % 20 == 11
                    && self.should_self_quit_when_alone()
                {
                    tracing::info!("auto-quit: lonko is the only pane left in its tmux session (tick)");
                    return Ok(true);
                }
            }
            Event::SessionDiscovered(file)                    => self.on_session_discovered(file),
            Event::SessionRemoved(pid)                        => { self.state.remove_session_by_pid(pid); }
            Event::TmuxPaneDiscovered { pane_id, claude_pid, cwd } => {
                self.on_tmux_pane_discovered(pane_id, claude_pid, cwd);
            }
            Event::TmuxPaneGone { pane_id }                   => {
                self.state.handle_pane_gone(&pane_id);
                if self.should_self_quit_when_alone() {
                    tracing::info!("auto-quit: lonko is the only pane left in its tmux session");
                    return Ok(true);
                }
            }
            Event::Key(key)                                   => return self.on_key(key),
            Event::Hook(payload)                              => self.handle_hook(payload),
            Event::FocusGained                                => self.state.focused = true,
            Event::FocusLost                                  => self.state.focused = false,
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.handle_mouse_click(mouse.column, mouse.row);
                    }
                    MouseEventKind::ScrollDown => self.handle_mouse_scroll(1),
                    MouseEventKind::ScrollUp => self.handle_mouse_scroll(-1),
                    _ => {}
                }
            }
            Event::Resize(w, h) => {
                self.state.term_width = w;
                self.state.term_height = h;
            }
            Event::PermissionResponse(key) => self.send_permission(&key),
        }
        Ok(false)
    }

    fn on_tick(&mut self) {
        self.state.tick = self.state.tick.wrapping_add(1);
        // Poll the active tmux pane every ~1s to keep the focused session current.
        // Skip update when lonko's own pane is active — keep last known focus.
        if self.state.tick.is_multiple_of(10)
            && let Some(active) = tmux::active_pane()
        {
            let is_own = self.state.own_pane.as_deref() == Some(active.as_str());
            if !is_own {
                let focused_id = self.state.sessions.iter()
                    .find(|s| s.tmux_pane.as_deref() == Some(active.as_str()))
                    .map(|s| s.id.clone());
                self.state.focused_session_id = focused_id;
            }
        }
        // Prune sessions that completed more than 30 seconds ago
        self.state.prune_completed(30);
        // Write session cache every second for `lonko focus N`.
        if self.state.tick % 10 == 1 {
            self.write_sessions_cache();
        }
        // Refresh tmux sessions list every 2s for the Sessions tab.
        if self.state.tick % 20 == 3 {
            let mut sessions = tmux::list_tmux_sessions();
            // Mark sessions that have a Claude agent running in them.
            let claude_panes: std::collections::HashSet<String> = self.state.sessions
                .iter()
                .filter_map(|s| s.tmux_pane.clone())
                .collect();
            for ts in &mut sessions {
                ts.has_claude = ts.windows.iter().any(|_w| {
                    // Check if any known Claude pane belongs to this session
                    claude_panes.iter().any(|pane| {
                        tmux_session_for_pane(pane).as_deref() == Some(ts.name.as_str())
                    })
                });
            }
            // Clamp selection
            if !sessions.is_empty() {
                self.state.tmux_selected = self.state.tmux_selected.min(sessions.len() - 1);
            }
            self.state.tmux_sessions = sessions;
        }
        // Scan tmux panes every 5 seconds to catch new/gone sessions.
        if self.state.tick.is_multiple_of(50)
            && let Some(ref tx) = self.scan_tx
        {
            let known_panes: Vec<String> = self.state.sessions
                .iter()
                .filter_map(|s| s.tmux_pane.clone())
                .collect();
            tmux_scanner::scan(tx, &known_panes, self.state.own_pane.as_deref());
        }
    }

    fn on_session_discovered(&mut self, file: crate::sources::lifecycle::SessionFile) {
        // If pre-created by hook (pid=0), update with real pid now
        if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == file.session_id && s.pid == 0) {
            s.pid = file.pid;
        }
        // Resolve tmux pane immediately so eviction logic works by pane.
        let tmux_pane = tmux::find_pane_for_pid(file.pid);
        // Skip if already tracked by pid, session_id, or pane.
        let exists = self.state.sessions.iter().any(|s| {
            s.pid == file.pid
                || s.id == file.session_id
                || (tmux_pane.is_some() && s.tmux_pane == tmux_pane)
        });
        if exists { return; }

        let mut session = Session::new(file.session_id.clone(), file.pid, file.cwd.clone());
        session.status = SessionStatus::Idle;
        session.tmux_pane = tmux_pane;
        let path = transcript::transcript_path(&file.cwd, &file.session_id);
        if let Some(mut info) = transcript::read_latest(&path) {
            info.branch = transcript::git_branch(&file.cwd).or(info.branch);
            session.apply_transcript_info(info);
        } else {
            session.branch = transcript::git_branch(&file.cwd);
        }
        self.state.sessions.push(session);
        if self.state.sessions.len() == 1 {
            self.state.selected = 0;
        }

        // Resolve the pane for the newly added session (last in the list).
        let last_pane = {
            let s = self.state.sessions.last().unwrap();
            s.tmux_pane.clone().or_else(|| tmux::find_pane_for_pid(s.pid))
        };

        // If focused_session_id is unknown, check if this new session is active.
        let active = tmux::active_pane();
        self.state.try_focus_active_pane(active.as_deref());

        // Auto-select if this session lives in the focus pane
        self.state.try_apply_focus_hint(last_pane.as_deref());
    }

    fn on_tmux_pane_discovered(&mut self, pane_id: String, claude_pid: u32, cwd: String) {
        let already_known = self.state.sessions.iter()
            .any(|s| s.tmux_pane.as_deref() == Some(&pane_id) || s.pid == claude_pid);
        if already_known || cwd.is_empty() { return; }

        let session_id = format!("tmux:{}", pane_id.trim_start_matches('%'));
        let mut session = Session::new(session_id.clone(), claude_pid, cwd.clone());
        session.status = SessionStatus::Idle;
        session.tmux_pane = Some(pane_id.clone());
        let path = transcript::transcript_path(&cwd, &session_id);
        if let Some(mut info) = transcript::read_latest(&path) {
            info.branch = transcript::git_branch(&cwd).or(info.branch);
            session.apply_transcript_info(info);
        } else {
            session.branch = transcript::git_branch(&cwd);
        }
        self.state.sessions.push(session);
        if self.state.sessions.len() == 1 {
            self.state.selected = 0;
        }
        if self.state.focused_session_id.is_none()
            && let Some(active) = tmux::active_pane()
            && active == pane_id
        {
            self.state.focused_session_id = Some(session_id);
        }
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.state.bookmark_mode {
            if let Some(note) = self.state.apply_bookmark_key(key.code, ctrl) {
                if let Some(session) = self.state.selected_session() {
                    let cwd = session.cwd.clone();
                    if note.is_empty() {
                        self.state.bookmarks.remove(&cwd);
                    } else {
                        self.state.bookmarks.insert(cwd, note);
                    }
                    crate::state::save_bookmarks(&self.state.bookmarks);
                }
            }
            return Ok(false);
        }
        if self.state.worktree_mode {
            if let Some(branch) = self.state.apply_worktree_key(key.code, ctrl) {
                let cwd = self.state.worktree_cwd.take().unwrap_or_default();
                if !cwd.is_empty() {
                    self.spawn_worktree(&cwd, &branch);
                }
            }
            return Ok(false);
        }
        if self.state.search_mode {
            if self.state.apply_search_key(key.code, ctrl) == KeyOutcome::Quit {
                return Ok(true);
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => {
                if self.state.active_tab == Tab::Sessions && self.state.tmux_expanded {
                    self.state.tmux_expanded = false;
                    self.state.tmux_window_cursor = None;
                } else if !self.state.search_query.is_empty() {
                    self.state.search_query.clear();
                    self.state.selected = 0;
                } else if self.state.show_detail {
                    self.state.show_detail = false;
                } else {
                    let _ = tmux::select_last_pane();
                }
            }
            KeyCode::Char('/') => { self.state.search_mode = true; }
            KeyCode::Char('d') => {
                self.state.show_detail = !self.state.show_detail;
                if self.state.show_detail { self.refresh_selected_transcript(); }
            }
            KeyCode::Char('q') => { self.hide_panel(); }
            KeyCode::Char('c') if ctrl => return Ok(true),
            KeyCode::Char('j') | KeyCode::Down => {
                if self.state.active_tab == Tab::Sessions {
                    if self.state.tmux_expanded {
                        self.state.navigate_tmux_window(1);
                    } else {
                        self.state.navigate_tmux_session(1);
                    }
                } else {
                    self.state.select_next();
                    if self.state.show_detail { self.refresh_selected_transcript(); }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.state.active_tab == Tab::Sessions {
                    if self.state.tmux_expanded {
                        self.state.navigate_tmux_window(-1);
                    } else {
                        self.state.navigate_tmux_session(-1);
                    }
                } else {
                    self.state.select_prev();
                    if self.state.show_detail { self.refresh_selected_transcript(); }
                }
            }
            KeyCode::Char('h') | KeyCode::Left
                if self.state.active_tab == Tab::Sessions =>
            {
                self.state.navigate_tmux_window(-1);
            }
            KeyCode::Char('l') | KeyCode::Right
                if self.state.active_tab == Tab::Sessions =>
            {
                self.state.navigate_tmux_window(1);
            }
            KeyCode::Tab => {
                self.state.toggle_tab();
                self.state.tmux_window_cursor = None;
                self.state.tmux_expanded = false;
            }
            KeyCode::Char('a' | 'A') => {
                self.state.active_tab = Tab::Agents;
                self.state.tmux_window_cursor = None;
                self.state.tmux_expanded = false;
            }
            KeyCode::Char('s' | 'S') => {
                self.state.active_tab = Tab::Sessions;
                self.state.tmux_window_cursor = None;
                self.state.tmux_expanded = false;
            }
            KeyCode::Char(' ')
                if self.state.active_tab == Tab::Sessions =>
            {
                if self.state.tmux_expanded {
                    self.state.tmux_expanded = false;
                    self.state.tmux_window_cursor = None;
                } else {
                    self.state.tmux_expanded = true;
                    // Position cursor at the active window
                    let active_idx = self.state.tmux_sessions
                        .get(self.state.tmux_selected)
                        .and_then(|s| s.windows.iter().position(|w| w.active))
                        .unwrap_or(0);
                    self.state.tmux_window_cursor = Some(active_idx);
                }
            }
            KeyCode::Enter => {
                if self.state.active_tab == Tab::Sessions {
                    self.focus_tmux_session();
                    self.state.tmux_expanded = false;
                } else {
                    self.focus_selected();
                }
            }
            KeyCode::Char('b') if self.state.active_tab == Tab::Agents => {
                if let Some(session) = self.state.selected_session() {
                    let cwd = session.cwd.clone();
                    self.state.bookmark_input = self.state.bookmarks
                        .get(&cwd)
                        .cloned()
                        .unwrap_or_default();
                    self.state.bookmark_mode = true;
                }
            }
            KeyCode::Char('g') => {
                self.launch_worktree_prompt();
            }
            // Permission shortcuts (y=yes/1, w=always/2, n=no/3)
            KeyCode::Char('y') => self.send_permission("1"),
            KeyCode::Char('w') => self.send_permission("2"),
            KeyCode::Char('n') => self.send_permission("3"),
            KeyCode::Char('x') if !self.state.has_waiting() => {
                self.kill_and_remove_worktree();
            }
            KeyCode::Char('X') if !self.state.has_waiting() => {
                self.kill_selected_agent();
            }
            KeyCode::Char(c @ '1'..='9') => {
                let n = (c as u8 - b'0') as usize;
                self.focus_nth(n);
            }
            _ => {}
        }
        Ok(false)
    }

    /// Soft kill: send Ctrl-C to the selected agent's tmux pane.
    fn kill_selected_agent(&mut self) {
        let Some(session) = self.state.selected_session() else { return };
        if matches!(session.status, SessionStatus::Completed) { return; }
        // Never kill the session lonko is running in
        if let (Some(own), Some(sp)) = (&self.state.own_pane, &session.tmux_pane) {
            if own == sp { return; }
        }
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));
        if let Some(ref p) = pane {
            let _ = tmux::send_ctrl_c(p);
        }
        if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
            s.completed_at = Some(std::time::Instant::now());
            s.status = SessionStatus::Completed;
        }
    }

    /// Hard kill: send Ctrl-C, then destroy the tmux session and remove the git worktree.
    /// Refuses to kill lonko's own session or sessions not running in a worktree.
    fn kill_and_remove_worktree(&mut self) {
        let Some(session) = self.state.selected_session() else { return };

        // Never kill the session lonko is running in
        if let (Some(own), Some(sp)) = (&self.state.own_pane, &session.tmux_pane) {
            if own == sp { return; }
        }

        let cwd = session.cwd.clone();
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));

        if !crate::worktree::is_worktree(&cwd) {
            // Not a worktree — fall back to soft kill
            self.kill_selected_agent();
            return;
        }

        // Send Ctrl-C to stop Claude
        if let Some(ref p) = pane {
            let _ = tmux::send_ctrl_c(p);
        }

        // Resolve tmux session name before removing from state
        let tmux_session_name = pane.as_deref()
            .and_then(tmux::tmux_session_for_pane);

        // Remove from lonko state
        self.state.sessions.retain(|s| s.id != session_id);
        // Clamp selection
        let len = self.state.sessions.len();
        if len > 0 {
            self.state.selected = self.state.selected.min(len - 1);
        }

        // Background cleanup: kill tmux session + remove worktree.
        // Bail if we couldn't resolve the tmux session — worktree and session
        // should live and die together.
        let Some(tmux_session_name) = tmux_session_name else { return };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tmux::kill_session(&tmux_session_name);
            if let Err(e) = crate::worktree::remove(&cwd) {
                let _ = std::process::Command::new("tmux")
                    .args(["display-message", &format!("worktree remove: {e}")])
                    .status();
            }
        });
    }

    /// Enter worktree mode: resolve the cwd and start accepting branch name input.
    fn launch_worktree_prompt(&mut self) {
        let cwd = if self.state.active_tab == Tab::Agents {
            self.state.selected_session().map(|s| s.cwd.clone())
        } else {
            self.state.tmux_sessions.get(self.state.tmux_selected)
                .and_then(|s| tmux::session_cwd(&s.name))
        };
        let Some(cwd) = cwd else { return };
        if crate::worktree::git_root(&cwd).is_none() { return; }
        self.state.worktree_cwd = Some(cwd);
        self.state.worktree_input.clear();
        self.state.worktree_mode = true;
    }

    /// Create a git worktree and launch Claude in a new tmux session.
    fn spawn_worktree(&self, cwd: &str, branch: &str) {
        let cwd = cwd.to_string();
        let branch = branch.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::worktree::run(&cwd, &branch) {
                // Show error via tmux message since we can't easily display in the TUI
                let _ = std::process::Command::new("tmux")
                    .args(["display-message", &format!("worktree: {e}")])
                    .status();
            }
        });
    }

    /// Write the ordered session list to two cache files:
    /// - ~/.cache/lonko-sessions: one pane_id per line (for `lonko focus N`)
    /// - ~/.cache/lonko-sessions-info: "N\tname\tcwd" per line (for shortcut-list.sh)
    fn write_sessions_cache(&self) {
        let sessions: Vec<&crate::state::Session> = self.state.sessions.iter().collect();

        // Pane IDs file (for lonko focus N)
        // Write ALL sessions (one per line), ignoring any active search filter so that
        // `lonko focus N` always maps to the canonical session order.
        let pane_content: String = sessions
            .iter()
            .map(|s| {
                let pane = s.tmux_pane.as_deref()
                    .map(str::to_string)
                    .or_else(|| tmux::find_pane_for_pid(s.pid));
                format!("{}\n", pane.as_deref().unwrap_or(""))
            })
            .collect();
        let _ = std::fs::write(focus::cache_path(), pane_content);

        // Info file (for shortcut-list.sh display)
        let info_content: String = sessions
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}\t{}\t{}\n", i + 1, s.project_name, s.cwd))
            .collect();
        let info_path = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("lonko-sessions-info");
        let _ = std::fs::write(info_path, info_content);
    }


    /// Focus the Nth visible session (1-indexed) by switching the tmux client.
    fn focus_nth(&mut self, n: usize) {
        let sessions = self.state.visible_sessions();
        let Some(session) = sessions.get(n.saturating_sub(1)) else { return };
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));
        if let Some(ref pane) = pane {
            if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                s.tmux_pane = Some(pane.clone());
            }
            self.state.selected = n - 1;
            write_no_follow_sentinel();
            let _ = tmux::select_pane(pane);
            let _ = tmux::focus_pane(pane);
            self.state.focused_session_id = Some(session_id);
            self.write_sessions_cache();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::hooks::HookPayload;

    fn empty_payload() -> HookPayload {
        HookPayload {
            hook_event_name: None,
            session_id: None,
            transcript_path: None,
            cwd: None,
            tool_name: None,
            prompt: None,
            message: None,
            notification_type: None,
            tmux_pane: None,
            parent_session_id: None,
            agent_id: None,
            agent_type: None,
            agent_transcript_path: None,
        }
    }

    fn mk_session() -> Session {
        Session::new("s1".into(), 100, "/tmp/proj".into())
    }

    // ── compute_scroll_offset ──────────────────────────────────────────────

    #[test]
    fn scroll_offset_at_start() {
        assert_eq!(compute_scroll_offset(0, 10, 5), 0);
        assert_eq!(compute_scroll_offset(1, 10, 5), 0);
    }

    #[test]
    fn scroll_offset_centres_selection() {
        // selected=5, total=20, visible=5 → half=2, so scroll=5-2=3
        assert_eq!(compute_scroll_offset(5, 20, 5), 3);
    }

    #[test]
    fn scroll_offset_clamps_at_end() {
        // selected=9 (last), total=10, visible=5 → 10-5=5
        assert_eq!(compute_scroll_offset(9, 10, 5), 5);
    }

    #[test]
    fn scroll_offset_empty_list() {
        assert_eq!(compute_scroll_offset(0, 0, 5), 0);
        assert_eq!(compute_scroll_offset(0, 5, 0), 0);
    }

    #[test]
    fn scroll_offset_visible_equals_total() {
        assert_eq!(compute_scroll_offset(3, 5, 5), 0);
    }

    // ── hook_event_to_status ───────────────────────────────────────────────

    #[test]
    fn hook_session_start_returns_idle() {
        let mut s = mk_session();
        let p = empty_payload();
        let result = hook_event_to_status("SessionStart", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::Idle)));
    }

    #[test]
    fn hook_user_prompt_submit_captures_prompt() {
        let mut s = mk_session();
        let p = HookPayload {
            prompt: Some("  hello world  ".into()),
            ..empty_payload()
        };
        let result = hook_event_to_status("UserPromptSubmit", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::Running)));
        assert_eq!(s.last_prompt.as_deref(), Some("hello world"));
    }

    #[test]
    fn hook_user_prompt_submit_ignores_blank() {
        let mut s = mk_session();
        s.last_prompt = Some("keep".into());
        let p = HookPayload {
            prompt: Some("   ".into()),
            ..empty_payload()
        };
        hook_event_to_status("UserPromptSubmit", &p, &mut s);
        assert_eq!(s.last_prompt.as_deref(), Some("keep"));
    }

    #[test]
    fn hook_pre_tool_use_stores_tool() {
        let mut s = mk_session();
        let p = HookPayload {
            tool_name: Some("Bash".into()),
            ..empty_payload()
        };
        let result = hook_event_to_status("PreToolUse", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::RunningTool(ref t)) if t == "Bash"));
        assert_eq!(s.last_tool.as_deref(), Some("Bash"));
    }

    #[test]
    fn hook_pre_tool_use_defaults_to_question_mark() {
        let mut s = mk_session();
        let p = empty_payload();
        let result = hook_event_to_status("PreToolUse", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::RunningTool(ref t)) if t == "?"));
    }

    #[test]
    fn hook_post_tool_use_returns_running() {
        let mut s = mk_session();
        let result = hook_event_to_status("PostToolUse", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Running)));
    }

    #[test]
    fn hook_session_end_marks_completed() {
        let mut s = mk_session();
        let result = hook_event_to_status("SessionEnd", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Completed)));
        assert!(s.completed_at.is_some());
    }

    #[test]
    fn hook_notification_permission_prompt() {
        let mut s = mk_session();
        let p = HookPayload {
            message: Some("allow edit?".into()),
            notification_type: Some("permission_prompt".into()),
            ..empty_payload()
        };
        let result = hook_event_to_status("Notification", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::WaitingForUser(ref m)) if m == "allow edit?"));
    }

    #[test]
    fn hook_notification_other_type() {
        let mut s = mk_session();
        let p = HookPayload {
            notification_type: Some("info".into()),
            ..empty_payload()
        };
        let result = hook_event_to_status("Notification", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::WaitingForInput)));
    }

    #[test]
    fn hook_unknown_event_returns_none() {
        let mut s = mk_session();
        let result = hook_event_to_status("SomeNewEvent", &empty_payload(), &mut s);
        assert!(result.is_none());
    }

    #[test]
    fn hook_stop_returns_idle() {
        let mut s = mk_session();
        // Stop and SubagentStop both return Idle (transcript read will fail in test, that's ok)
        let result = hook_event_to_status("Stop", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Idle)));
    }

    #[test]
    fn hook_subagent_stop_returns_idle() {
        let mut s = mk_session();
        let result = hook_event_to_status("SubagentStop", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Idle)));
    }

    // ── write_no_follow_sentinel ───────────────────────────────────────────

    #[test]
    fn sentinel_file_is_created() {
        let sentinel = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("lonko-no-follow");
        let _ = std::fs::remove_file(&sentinel);

        write_no_follow_sentinel();

        assert!(sentinel.exists());
        let _ = std::fs::remove_file(&sentinel);
    }
}
