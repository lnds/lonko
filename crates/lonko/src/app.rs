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

/// Write the no-follow sentinel so lonko-follow.sh skips the next hook trigger.
pub fn write_no_follow_sentinel() {
    let sentinel = crate::state::lonko_cache_dir().join("lonko-no-follow");
    // Ensure the cache dir exists — on a fresh install it may not,
    // and a silent write failure here means the follow script never
    // sees the sentinel and kills lonko on every switch-client.
    if let Some(parent) = sentinel.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&sentinel, "");
}

/// Rewrite the no-follow sentinel over ~200 ms so the second of the two hooks
/// `switch-client` fires (`client-session-changed` then `after-select-window`)
/// still sees it after the first invocation consumes it. Short by design: a
/// longer window would silently suppress legitimate follows the user triggers
/// immediately afterwards. Requires a Tokio runtime; all current callers run
/// inside the event loop.
fn refresh_no_follow_sentinel_async() {
    tokio::spawn(async move {
        for delay_ms in [30u64, 60, 100] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            write_no_follow_sentinel();
        }
    });
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
                // Skip `<<autonomous-loop-dynamic>` and similar runtime
                // sentinels — they're scheduled re-fires, not prompts the
                // user just typed.
                if !text.is_empty() && !transcript::is_system_injected(text) {
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
/// Attach to the remote tmux session containing `pane_id`, following the
/// `remote/<host>` convention used by the user's tmux setup:
///
///   - A single long-lived local session named `remote/<short-host>`
///     holds an `ssh -t <host> 'tmux attach'` for reuse.
///   - Tmux hooks (`update-status-left.sh` in dotfiles) hide the local
///     status bar on `remote/*` sessions so the remote tmux's own status
///     bar shows through, making the attach visually indistinguishable
///     from a direct remote connection.
///   - Switching between remote sessions is done by telling the
///     *remote* tmux to `switch-client` via a separate ssh call,
///     rather than opening another nested tmux session locally.
///
/// This function creates the `remote/<host>` session lazily, switches
/// the local client to it, and then asks the remote tmux to move to
/// the session containing `pane_id`.
pub fn attach_remote_agent(host: &str, pane_id: &str) {
    let short = short_host(host);
    let local_session = format!("remote/{short}");
    ensure_remote_host_session(host, &local_session);

    // Suppress the follow hook for this switch-client: the `remote/<host>`
    // wrapper session already contains a nested lonko (via the ssh attach),
    // so moving the local lonko into it would stack two panels in the same
    // window. The `remote/*` guard in lonko-follow.sh covers this, and the
    // sentinel is an extra belt-and-suspenders defense against the guard
    // missing a hook due to timing. Rewrite the sentinel across a short
    // window so the second hook (`after-select-window` after
    // `client-session-changed`) still sees it.
    write_no_follow_sentinel();
    refresh_no_follow_sentinel_async();
    let _ = std::process::Command::new("tmux")
        .args(["switch-client", "-t", &local_session])
        .stderr(std::process::Stdio::null())
        .status();

    // Ask the remote tmux to move to the session containing pane_id.
    let pane_escaped = pane_id.replace('\'', "'\\''");
    let remote_switch = format!(
        "tmux switch-client -t \"$(tmux display-message -p -F '#{{session_name}}' -t '{}')\"",
        pane_escaped,
    );
    let _ = std::process::Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "LogLevel=ERROR",
            host,
            &remote_switch,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Lowercased first DNS label of a host. `Kayshon.ts.net` → `kayshon`.
fn short_host(host: &str) -> String {
    host.split('.').next().unwrap_or(host).to_ascii_lowercase()
}

/// Create `local_session` if it doesn't already exist. The session
/// runs a single `ssh -t <host> 'tmux attach'` — when that ssh exits
/// the local session ends too.
fn ensure_remote_host_session(host: &str, local_session: &str) {
    let has_session = std::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("={local_session}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if has_session {
        return;
    }

    // Bootstrap the remote's default `main` session if absent, then attach.
    // Matches the dotfile convention in remote-connect.sh.
    let host_escaped = host.replace('\'', "'\\''");
    let ssh_cmd = format!(
        "ssh -t '{host_escaped}' 'tmux new-session -d -s main 2>/dev/null; tmux attach -t main'"
    );
    let _ = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", local_session, &ssh_cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn notify_if_needed(project_name: &str, status: &SessionStatus) {
    let (summary, body) = match status {
        SessionStatus::WaitingForUser(msg) => {
            (format!("lonko · {} ⚠", project_name), msg.clone())
        }
        SessionStatus::WaitingForInput => {
            (format!("lonko · {}", project_name), "ready, waiting for your input".into())
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
    /// Last mouse click: (tab, global_idx, instant) for double-click detection.
    last_click: Option<(Tab, usize, std::time::Instant)>,
    /// Monotonic counter shared with pending focus tasks; increment to cancel stale spawns.
    focus_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// SSH reverse-tunnel bridges keyed by host. One child `ssh -N -R`
    /// per online Tailnet host. Dropped on shutdown (see Drop impl on
    /// `RemoteBridge`, which reaps the child).
    remote_bridges: std::collections::HashMap<String, crate::sources::remote_bridge::RemoteBridge>,
    /// Hosts with a bridge start currently in flight on a blocking task.
    /// Prevents double-spawn while the task is pending.
    remote_bridge_starting: std::collections::HashSet<String>,
    /// Latest Tailnet peers reported as online (before excluded-host filtering).
    /// Populated every time a `RemotePeersOnline` event lands; read by
    /// `sync_remote_bridges` so bridges can be kept alive regardless of
    /// which tab the user is on.
    remote_online_hosts: std::collections::HashSet<String>,
}

impl App {
    pub fn new() -> Self {
        let mut state = AppState::default();
        state.bookmarks = crate::state::load_bookmarks();
        let config = crate::config::load();
        // UI toggle (LONKO-52) lives in its own file and wins over
        // config.toml so the user's last choice survives restart without
        // rewriting their config.
        state.remote_enabled = crate::config::load_remote_enabled_override()
            .unwrap_or(config.remote.enabled);
        state.remote_poll_ticks = config.remote.poll_interval_secs.max(1) * 10;
        state.excluded_hosts = crate::config::load_excluded_hosts();
        Self {
            state,
            scan_tx: None,
            last_click: None,
            focus_gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            remote_bridges: std::collections::HashMap::new(),
            remote_bridge_starting: std::collections::HashSet::new(),
            remote_online_hosts: std::collections::HashSet::new(),
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
        // Tab labels in the inner row (row 1):
        //   "Agents" (cols 1-6) │ "Sessions" (cols 10-17) │ "Remote" (cols 21-26)
        // Boundaries at col 9 and col 20 (the middles of the " │ " dividers).
        if row < 3 {
            if row == 1 {
                if _col <= 9 {
                    self.state.active_tab = Tab::Agents;
                } else if _col <= 20 || !self.state.remote_enabled {
                    self.state.active_tab = Tab::Sessions;
                } else {
                    self.state.active_tab = Tab::Remote;
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

        // Agents tab: variable-height cards, starts at y=3
        let visible = self.state.visible_sessions();
        let total = visible.len();
        if total == 0 { return; }

        let list_h = h.saturating_sub(3 + 1);
        let (header_flags, collapsed_flags) =
            ui::list::compute_header_and_collapsed(&visible, &self.state);
        let remote_sep_flags =
            ui::list::compute_remote_sep_flags(&visible, self.state.remote_enabled);
        let (scroll, cards_visible) = ui::list::compute_scroll(
            &visible, self.state.selected, list_h, &header_flags, &collapsed_flags, &remote_sep_flags, &self.state.bookmarks,
        );

        // Linear scan to find which card was clicked based on row offset from y=3.
        // Must mirror the render layout: remote-sep + header + card + separator
        // (separator between cards only).
        let click_y = row - 3;
        let mut y_acc: u16 = 0;
        let mut card_idx: Option<usize> = None;
        let page_end = (scroll + cards_visible).min(visible.len());
        for (i, s) in visible[scroll..page_end].iter().enumerate() {
            let global = scroll + i;
            if i > 0 {
                y_acc += 1; // separator between cards (not before first)
            }
            if remote_sep_flags[global] {
                y_acc += ui::list::REMOTE_SEP_HEIGHT;
            }
            if header_flags[global] {
                let hdr_h = ui::list::GROUP_HEADER_HEIGHT;
                if collapsed_flags[global] {
                    // Collapsed: the header IS the clickable target
                    if click_y >= y_acc && click_y < y_acc + hdr_h {
                        card_idx = Some(i);
                        break;
                    }
                    y_acc += hdr_h;
                    continue;
                }
                y_acc += hdr_h;
            }
            let ch = ui::list::card_height(s, &self.state.bookmarks);
            if click_y >= y_acc && click_y < y_acc + ch {
                card_idx = Some(i);
                break;
            }
            y_acc += ch;
        }

        let global_idx = match card_idx {
            Some(idx) => scroll + idx,
            None => return,
        };
        if global_idx >= total {
            return;
        }

        // Double-click detection: two clicks on the same card within 400ms → focus
        let now = std::time::Instant::now();
        let is_double = self.last_click
            .as_ref()
            .is_some_and(|(last_tab, last_idx, last_time)| {
                *last_tab == Tab::Agents && *last_idx == global_idx && now.duration_since(*last_time).as_millis() < 400
            });
        self.last_click = Some((Tab::Agents, global_idx, now));

        if is_double {
            self.state.selected = global_idx;
            let session = self.state.selected_session().cloned();
            if let Some(session) = session {
                // Remote agent: route attach through SSH (same behavior
                // as Enter), then bail out of the local focus-retry loop.
                if let Some(host) = session.host.as_deref() {
                    if let Some(pane) = session.tmux_pane.as_deref() {
                        attach_remote_agent(host, pane);
                    }
                    return;
                }
                let pid = session.pid;
                let session_id = session.id.clone();
                let pane = session.tmux_pane.clone()
                    .or_else(|| tmux::find_pane_for_pid(pid));
                if let Some(p) = pane {
                    if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                        s.tmux_pane = Some(p.clone());
                    }
                    self.state.focused_session_id = Some(session_id);
                    // Do the window move + switch-client up front so the
                    // user lands on a window that already has the sidebar
                    // parked at 25%. The retry loop below only re-asserts
                    // `select-pane` to beat tmux mouse-mode's own cursor
                    // reselection (every MouseUp/MouseDown re-selects the
                    // lonko pane), so we don't want to repeat the expensive
                    // join-pane + switch-client on every tick.
                    self.focus_local_agent_pane(&p);
                    use std::sync::atomic::Ordering;
                    let my_gen = self.focus_gen.fetch_add(1, Ordering::SeqCst) + 1;
                    let gen_arc = self.focus_gen.clone();
                    tokio::spawn(async move {
                        for delay_ms in [30u64, 60, 100, 160, 240] {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            if gen_arc.load(Ordering::SeqCst) != my_gen { break; }
                            let _ = tmux::select_pane(&p);
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

        // Compute layout and capture window counts for each visible card in a
        // scoped borrow so we can mutate state below.
        let (global_idx, card_row_start, n_windows) = {
            let visible = self.state.visible_tmux_sessions();
            if visible.is_empty() {
                return;
            }
            let page = crate::ui::tmux_sessions::session_page_layout(
                &visible,
                self.state.tmux_selected,
                self.state.tmux_expanded,
                list_h,
            );
            let hit = page.into_iter().find(|c| {
                row_in_list >= c.row_start && row_in_list < c.row_start + c.card_h
            });
            let Some(card) = hit else { return };
            let n = visible[card.global_idx].windows.len();
            (card.global_idx, card.row_start, n)
        };

        let row_within_card = row_in_list - card_row_start;

        // Double-click detection: two clicks on the same card within 400ms.
        let now = std::time::Instant::now();
        let is_double = self.last_click
            .as_ref()
            .is_some_and(|(last_tab, last_idx, last_time)| {
                *last_tab == Tab::Sessions && *last_idx == global_idx && now.duration_since(*last_time).as_millis() < 400
            });
        self.last_click = Some((Tab::Sessions, global_idx, now));

        let is_selected = global_idx == self.state.tmux_selected;
        let is_expanded = is_selected && self.state.tmux_expanded;

        // Check if click lands on a window row within an expanded card.
        // Layout: rows 0-2 = header, rows 3..3+n = window rows, last row = activity bar.
        let window_row: Option<usize> = if is_expanded && row_within_card >= 3 {
            let win_idx = (row_within_card - 3) as usize;
            if win_idx < n_windows { Some(win_idx) } else { None }
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
            Tab::Remote => self.state.navigate_remote(delta),
        }
    }

    /// Focus the selected tmux session (Sessions tab), optionally at a specific window.
    fn focus_tmux_session(&mut self) {
        let Some(session) = self.state.selected_tmux_session() else { return };
        let name = session.name.clone();
        if let Some(win_idx) = self.state.tmux_window_cursor
            && let Some(window) = session.windows.get(win_idx) {
                let _ = tmux::focus_session_window(&name, window.index);
                return;
            }
        let _ = std::process::Command::new("tmux")
            .args(["switch-client", "-t", &name])
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// Toggle exclusion of the host under the cursor in the Remote tab.
    /// Excluded hosts are removed from the list and skipped during polling.
    fn toggle_exclude_remote_host(&mut self) {
        let Some(hostname) = self.state.selected_remote_host().map(|s| s.to_string()) else {
            return;
        };
        if self.state.excluded_hosts.contains(&hostname) {
            self.state.excluded_hosts.remove(&hostname);
        } else {
            self.state.excluded_hosts.insert(hostname.clone());
            // Remove from visible list immediately.
            self.state.remote_hosts.retain(|h| h.hostname != hostname);
            // Clamp selection.
            let count = self.state.remote_item_count();
            if count > 0 {
                self.state.remote_selected = self.state.remote_selected.min(count - 1);
            } else {
                self.state.remote_selected = 0;
            }
        }
        crate::config::save_excluded_hosts(&self.state.excluded_hosts);
    }

    /// Attach to the selected remote tmux session using the
    /// `remote/<host>` convention (see `attach_remote_agent` for the
    /// design rationale). Reuses the wrapper local session if already
    /// open, and tells the remote tmux to switch to the picked session
    /// via a separate ssh call.
    fn attach_remote_session(&self) {
        let Some((host, session_name)) = self.state.selected_remote_session() else { return };
        let short = short_host(host);
        let local_session = format!("remote/{short}");
        ensure_remote_host_session(host, &local_session);

        // See `attach_remote_agent`: suppress the follow script so lonko
        // stays put when we switch-client into `remote/<host>`.
        write_no_follow_sentinel();
        refresh_no_follow_sentinel_async();
        let _ = std::process::Command::new("tmux")
            .args(["switch-client", "-t", &local_session])
            .stderr(std::process::Stdio::null())
            .status();

        let escaped = session_name.replace('\'', "'\\''");
        let _ = std::process::Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "LogLevel=ERROR",
                host,
                &format!("tmux switch-client -t '{escaped}'"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
        // Require a non-empty result: `list_pane_ids_in_session` also returns an
        // empty vec when the tmux subprocess fails transiently (server restart,
        // IO error), and we don't want that to self-quit. The genuinely-gone case
        // is already covered by `tmux_session_for_pane` returning None above.
        !panes.is_empty() && panes.iter().all(|p| p == own)
    }

    /// Hide the panel by moving it back to lonko-tray (lonko keeps running).
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

        // Ensure lonko-tray exists. Silence stdio so tmux's "can't find session"
        // (expected when the tray hasn't been created yet) and similar messages
        // don't bleed onto the TUI alternate screen.
        let tray_exists = std::process::Command::new("tmux")
            .args(["has-session", "-t", "lonko-tray"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !tray_exists {
            let _ = std::process::Command::new("tmux")
                .args(["new-session", "-d", "-s", "lonko-tray"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }

        let _ = std::process::Command::new("tmux")
            .args(["break-pane", "-d", "-s", own, "-t", "lonko-tray:"])
            .stderr(std::process::Stdio::null())
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
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                let _ = std::fs::remove_file(&layout_path);
            }
        }
    }

    fn focus_selected(&mut self) {
        let Some(session) = self.state.selected_session() else { return };
        // Remote agent: open a new tmux window that SSH-attaches to the
        // remote tmux session containing this pane. Falls back to a no-op
        // when we don't yet know the pane (hook hasn't landed).
        if let Some(host) = session.host.as_deref() {
            if let Some(pane) = session.tmux_pane.as_deref() {
                attach_remote_agent(host, pane);
            }
            return;
        }
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
            self.focus_local_agent_pane(pane);
            self.state.focused_session_id = Some(session_id);
        } else {
            tracing::warn!("focus_selected: no pane found for pid={pid}, using select_last_pane");
            let _ = tmux::select_last_pane();
        }
    }

    /// Focus a local agent pane as smoothly as possible:
    ///   - Fast path: when the pane already lives in lonko's window, a
    ///     plain `select-pane` focuses it without a client-session-changed
    ///     round-trip.
    ///   - Slow path: pre-move lonko's own pane into the target window
    ///     via `join-pane` BEFORE `switch-client`, so the user arrives to
    ///     a window that already has the sidebar in place (no flash, no
    ///     post-arrival reflow). We skip the pre-move when the target
    ///     window already has a lonko (e.g. an ssh pane whose remote tmux
    ///     carries its own sidebar — moving ours in would stack two
    ///     panels, LONKO-53).
    ///
    /// The no-follow sentinel is written before the pre-move so any hook
    /// that races in finds lonko already parked where it belongs and
    /// exits via the `already-here` branch instead of redoing the move.
    fn focus_local_agent_pane(&self, pane: &str) {
        let Some(target_win) = tmux::tmux_window_for_pane(pane) else {
            // Can't resolve the target window — fall back to the plain
            // select + switch-client pair so focus still works, albeit
            // with the older flicker.
            let _ = tmux::select_pane(pane);
            let _ = tmux::focus_pane(pane);
            return;
        };

        // Compare against *lonko's* window explicitly — using the client's
        // "current window" misfires when lonko lives in one session and
        // the user's client is on another (e.g. lonko-tray).
        let lonko_win = self.state.own_pane
            .as_deref()
            .and_then(tmux::tmux_window_for_pane);

        if lonko_win.as_deref() == Some(target_win.as_str()) {
            let _ = tmux::select_pane(pane);
            return;
        }

        if let Some(own) = self.state.own_pane.as_deref()
            && !tmux::window_has_lonko_pane(&target_win)
        {
            write_no_follow_sentinel();
            let _ = tmux::join_pane_right(own, &target_win, 25);
        }

        let _ = tmux::select_pane(pane);
        let _ = tmux::focus_pane(pane);
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
    ///
    /// For sessions originating on a remote host, the keystroke is delivered
    /// via SSH to that host's tmux server (the pane ID would not resolve
    /// against the local tmux).
    fn send_permission(&mut self, key: &str) {
        let waiting = self.state.sessions.iter().find(|s| s.status.is_waiting());
        let Some(session) = waiting else { return };
        let pid = session.pid;
        let session_id = session.id.clone();
        let host = session.host.clone();
        // For remote sessions there is no usable local PID, so we can only
        // trust whatever pane ID the bridge already propagated.
        let pane = if host.is_some() {
            session.tmux_pane.clone()
        } else {
            session.tmux_pane.clone()
                .or_else(|| tmux::find_pane_for_pid(pid))
        };
        if let Some(ref p) = pane
            && let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id)
                && s.tmux_pane.is_none() {
                    s.tmux_pane = Some(p.clone());
                }
        let Some(pane) = pane else { return };
        let result = match host.as_deref() {
            Some(h) => tmux::send_keys_remote(h, &pane, key),
            None    => tmux::send_keys(&pane, key),
        };
        if let Err(e) = result {
            tracing::warn!("send_permission failed: {e}");
        }
    }

    fn handle_hook(&mut self, payload: crate::sources::hooks::HookPayload) {
        if let Some(host) = payload.host.as_deref() {
            tracing::info!(
                "remote hook host={host} event={:?} session={:?} tmux_pane={:?} cwd={:?}",
                payload.hook_event_name,
                payload.session_id,
                payload.tmux_pane,
                payload.cwd,
            );
        }

        let parent_session_id = match &payload.session_id {
            Some(id) => id.clone(),
            None => {
                if payload.host.is_some() {
                    tracing::warn!("remote hook dropped: no session_id");
                }
                return;
            }
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

                let (parent_depth, parent_repo_root) = self.state.sessions.iter()
                    .find(|s| s.id == parent_session_id)
                    .map(|s| (s.depth, s.repo_root.clone()))
                    .unwrap_or((0, None));

                let agent_type = payload.agent_type.as_deref().unwrap_or("sub");
                let mut session = Session::new(effective_id.clone(), 0, cwd);
                session.status = SessionStatus::Running;
                session.parent_id = Some(parent_session_id.clone());
                session.depth = (parent_depth + 1).min(2);
                session.project_name = agent_type.to_string();
                // Subagents inherit their parent's group so they cluster together.
                session.repo_root = parent_repo_root;
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
                payload.host.as_deref(),
            ) {
                return;
            }
            // Fill in the group key for brand-new sessions; the cwd fallback
            // ensures non-git sessions never re-trigger the shell call on
            // subsequent hook events.
            if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == effective_id)
                && s.repo_root.is_none()
                && !s.cwd.is_empty()
            {
                s.repo_root = Some(
                    crate::worktree::repo_common_root(&s.cwd).unwrap_or_else(|| s.cwd.clone()),
                );
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
        if !is_subagent
            && let Some(cwd) = &payload.cwd
                && !cwd.is_empty() && session.cwd != *cwd {
                    session.cwd = cwd.clone();
                    session.project_name = cwd.split('/').next_back().unwrap_or(cwd).to_string();
                }

        // Stamp the originating host so later operations (permission sends,
        // worktree creation, kill) can route to the right tmux server.
        // Only overwrite when the incoming payload asserts a host: a later
        // local-only hook should not clobber a session that belongs to a
        // remote machine.
        if payload.host.is_some() {
            session.host = payload.host.clone();
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
            notify_if_needed(session.display_name(), &session.status);
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Tick                                       => {
                self.on_tick();
                // Fallback auto-quit check: `TmuxPaneGone` only fires for panes lonko
                // had tracked as running Claude, so closing a plain shell pane wouldn't
                // trigger it. Every 2s (20 ticks * 100ms), offset by 11 to avoid
                // colliding with the other periodic tasks scheduled in on_tick
                // (% 10 == 1, % 20 == 3, is_multiple_of(10|50)). 3s startup grace.
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
            Event::Hook(payload)                              => self.handle_hook(*payload),
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
            Event::TmuxSessionsRefreshed(sessions) => {
                self.state.tmux_sessions = sessions;
                let visible_len = self.state.visible_tmux_sessions().len();
                if visible_len > 0 {
                    self.state.tmux_selected = self.state.tmux_selected.min(visible_len - 1);
                } else {
                    self.state.tmux_selected = 0;
                }
            }
            Event::RemoteSnapshot(snapshot) => {
                self.on_remote_snapshot(snapshot);
            }
            Event::RemotePeersOnline(online) => {
                // Cache the set of hosts so `sync_remote_bridges` can reach
                // them even when the Remote tab isn't open (tmux polling
                // is gated by the active tab, but bridges are not).
                self.remote_online_hosts = online.iter().cloned().collect();
                // Remove hosts that are no longer in the Tailnet peer list.
                self.state.remote_hosts.retain(|h| online.contains(&h.hostname));
                let count = self.state.remote_item_count();
                if count > 0 {
                    self.state.remote_selected = self.state.remote_selected.min(count - 1);
                } else {
                    self.state.remote_selected = 0;
                }
                // Start bridges without waiting for the next 2s tick —
                // matters most right after lonko launches, when every
                // extra second delays when remote agents first appear
                // in the Agents list.
                if self.state.remote_enabled {
                    self.sync_remote_bridges();
                }
            }
            Event::RemoteBridgeStarted { host, result } => {
                self.remote_bridge_starting.remove(&host);
                match result {
                    Ok(bridge) => {
                        tracing::debug!("remote bridge to {host} ready");
                        self.remote_bridges.insert(host, bridge);
                    }
                    Err(e) => {
                        tracing::warn!("remote bridge to {host} failed: {e}");
                    }
                }
            }
            Event::PrPickerLoaded { cwd, result } => {
                // Drop the payload if the user already closed the picker or
                // moved on to a different repo — otherwise we'd flash stale
                // results into a fresh session.
                if !self.state.pr_picker_mode
                    || self.state.pr_picker_cwd.as_deref() != Some(cwd.as_str())
                {
                    return Ok(false);
                }
                self.state.pr_picker_loading = false;
                match result {
                    Ok(prs) => {
                        self.state.pr_picker_prs = prs;
                        self.state.pr_picker_selected = 0;
                        self.state.pr_picker_error = None;
                    }
                    Err(e) => {
                        self.state.pr_picker_error = Some(e);
                    }
                }
            }
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
        // Runs on a blocking task: the work fans out to ~7 forks (one
        // `list-sessions`, one `list-windows` per session, plus a single
        // `list-panes -a` via `session_pane_map` for the has_claude
        // lookup). It used to block the event loop with ~80 `display-
        // message` forks per refresh (redundantly nested over window
        // count). The main loop now only schedules the work; the result
        // lands as `Event::TmuxSessionsRefreshed`.
        if self.state.tick % 20 == 3
            && let Some(ref tx) = self.scan_tx
        {
            let claude_panes: std::collections::HashSet<String> = self.state.sessions
                .iter()
                .filter_map(|s| s.tmux_pane.clone())
                .collect();
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let mut sessions = tmux::list_tmux_sessions();
                let pane_map = tmux::session_pane_map();
                for ts in &mut sessions {
                    ts.has_claude = pane_map
                        .get(&ts.name)
                        .is_some_and(|panes| claude_panes.iter().any(|p| panes.contains(p)));
                }
                let _ = tx.send(Event::TmuxSessionsRefreshed(sessions));
            });
        }
        // Scan tmux panes every 5 seconds to catch new/gone sessions.
        // Remote sessions are excluded: their pane IDs belong to a different
        // tmux server and would trigger spurious `TmuxPaneGone` (which in
        // turn removes the session, making the remote agent flicker in and
        // out of the Agents list).
        if self.state.tick.is_multiple_of(50)
            && let Some(ref tx) = self.scan_tx
        {
            let known_panes: Vec<String> = self.state.sessions
                .iter()
                .filter(|s| s.host.is_none())
                .filter_map(|s| s.tmux_pane.clone())
                .collect();
            let own_pane = self.state.own_pane.clone();
            let tx_scan = tx.clone();
            // pgrep + walks `ps -o ppid=` per claude PID + `tmux list-panes -a`;
            // taken off the main thread to keep the event loop responsive.
            tokio::task::spawn_blocking(move || {
                tmux_scanner::scan(&tx_scan, &known_panes, own_pane.as_deref());
            });
            // Reap local agents whose Claude process has exited. The scanner
            // only fires TmuxPaneGone for panes that vanished; a phantom whose
            // pane is gone *and* tmux_pane is None (e.g. lifecycle-only
            // discoveries that never saw a hook) would otherwise linger.
            self.state.reap_dead_local_sessions(|pid| unsafe {
                libc::kill(pid as libc::pid_t, 0) == 0
            });
        }

        // Tailnet peer discovery runs on the background of every 2s tick
        // whenever remote support is enabled, independent of the active
        // tab. The resulting `RemotePeersOnline` event is what keeps
        // `sync_remote_bridges` informed — without it, bridges would
        // only come up while the user happens to be looking at the
        // Remote tab. Tick 1 also fires, so first-time discovery
        // happens ~100 ms after lonko launches instead of waiting a
        // full 2 s interval.
        if self.state.remote_enabled
            && (self.state.tick.is_multiple_of(20) || self.state.tick == 1)
            && let Some(ref tx) = self.scan_tx
        {
            let excluded = self.state.excluded_hosts.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let peers = match tokio::task::spawn_blocking(
                    crate::sources::tailnet::list_online_peers
                ).await {
                    Ok(Ok(p)) => p,
                    Ok(Err(e)) => { tracing::warn!("tailnet discovery failed: {e}"); return; }
                    Err(e) => { tracing::warn!("tailnet task panicked: {e}"); return; }
                };
                let online_names: Vec<String> = peers.into_iter()
                    .map(|p| p.hostname)
                    .filter(|h| !excluded.contains(h))
                    .collect();
                let _ = tx.send(Event::RemotePeersOnline(online_names));
            });

            self.sync_remote_bridges();
        }

        // Per-host tmux polling runs whenever remote support is enabled
        // (not gated on the active tab). It is the ONLY way remote
        // Agents show up proactively in the Agents tab — without it we
        // would have to wait for the first hook event, which never
        // comes if the remote Claude is idle.
        if self.state.remote_enabled
            && self.state.tick.is_multiple_of(10)
            && let Some(ref tx) = self.scan_tx
        {
            let tick = self.state.tick;
            let online = self.remote_online_hosts.clone();

            let known_hosts: std::collections::HashMap<String, u64> = self.state.remote_hosts
                .iter()
                .map(|h| (h.hostname.clone(), h.next_poll_tick))
                .collect();

            for host in online {
                if let Some(&next_tick) = known_hosts.get(&host)
                    && tick < next_tick { continue; }
                let tx = tx.clone();
                tokio::task::spawn_blocking(move || {
                    match crate::sources::remote_tmux::poll_host(&host) {
                        Ok(snapshot) => {
                            let _ = tx.send(Event::RemoteSnapshot(snapshot));
                        }
                        Err(e) => {
                            tracing::debug!("remote poll {host} failed: {e}");
                            let _ = tx.send(Event::RemoteSnapshot(
                                crate::sources::remote_tmux::RemoteSnapshot {
                                    host: host.clone(),
                                    sessions: vec![],
                                    claude_panes: vec![],
                                    is_error: true,
                                },
                            ));
                        }
                    }
                });
            }
        }
    }

    /// Reconcile `remote_bridges` with the set of hosts that are currently
    /// reachable on the Tailnet and not excluded. Starts a bridge for
    /// each host that needs one; drops bridges whose host fell off the
    /// list or whose SSH child has exited. Start attempts run on a
    /// blocking task so the short preparatory SSH probe does not stall
    /// the UI; the resulting `RemoteBridge` arrives via
    /// `Event::RemoteBridgeStarted`.
    /// Flip remote support on/off at runtime. When disabling, every
    /// remote-only artifact is torn down immediately: bridges killed,
    /// provisional remote agents dropped from the Agents list, the
    /// Remote tab's host cache cleared, and the user bounced off the
    /// Remote tab if they happened to be on it. When enabling, the
    /// next on_tick discovery round wires everything back up.
    ///
    /// The choice persists across restarts via a small text file
    /// (`~/.config/lonko/remote-enabled`) that overrides the static
    /// config.toml value, so a user who toggles off on kayshon while
    /// their config says `enabled = true` stays off on reboot.
    fn toggle_remote_support(&mut self) {
        let new_enabled = !self.state.remote_enabled;
        self.state.remote_enabled = new_enabled;
        crate::config::save_remote_enabled_override(new_enabled);

        if new_enabled {
            tracing::info!("remote support enabled (runtime toggle)");
            return;
        }

        tracing::info!("remote support disabled (runtime toggle)");

        // Kill all bridges; the map's Drop impls reap the ssh children.
        self.remote_bridges.clear();
        self.remote_bridge_starting.clear();

        // Drop the Tailnet caches and Remote-tab host list.
        self.remote_online_hosts.clear();
        self.state.remote_hosts.clear();
        self.state.remote_selected = 0;

        // Remove every session that belongs to a remote host — both
        // `remote:` provisionals and hook-promoted real sessions.
        // Without this the Agents list would keep showing stale cards
        // for hosts we're no longer polling.
        self.state.sessions.retain(|s| s.host.is_none());
        let len = self.state.visible_len();
        self.state.selected = if len == 0 { 0 } else { self.state.selected.min(len - 1) };

        if self.state.active_tab == Tab::Remote {
            self.state.active_tab = Tab::Agents;
        }
    }

    fn sync_remote_bridges(&mut self) {
        let Some(ref tx) = self.scan_tx else { return };

        // Desired set: latest Tailnet peers, minus any explicitly excluded
        // by the user. Uses the cache populated on `RemotePeersOnline`
        // rather than `remote_hosts` (which is only filled when the
        // Remote tab has been activated).
        let desired: std::collections::HashSet<String> = self
            .remote_online_hosts
            .iter()
            .filter(|h| !self.state.excluded_hosts.contains(*h))
            .cloned()
            .collect();

        // Drop bridges for hosts that should no longer have one, and
        // reap any bridge whose SSH child has exited (so we retry next
        // cycle with a fresh spawn).
        self.remote_bridges.retain(|host, bridge| {
            if !desired.contains(host) {
                tracing::debug!("tearing down remote bridge to {host} (not online)");
                return false;
            }
            if !bridge.is_alive() {
                tracing::warn!("remote bridge to {host} exited; will retry");
                return false;
            }
            true
        });

        // Start bridges for desired hosts that don't have one yet, unless
        // a start task is already in flight.
        for host in desired {
            if self.remote_bridges.contains_key(&host)
                || self.remote_bridge_starting.contains(&host)
            {
                continue;
            }
            self.remote_bridge_starting.insert(host.clone());
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::sources::remote_bridge::RemoteBridge::start(&host)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::RemoteBridgeStarted { host, result });
            });
        }
    }

    /// Compute the next poll tick for a host based on its failure count.
    /// Doubles the base interval per failure, capped at 5 minutes.
    fn backoff_ticks(base_ticks: u64, fail_count: u32, current_tick: u64) -> u64 {
        let shift = fail_count.min(32);
        let delay = base_ticks.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX)).min(3000);
        current_tick + delay
    }

    fn on_remote_snapshot(&mut self, snapshot: crate::sources::remote_tmux::RemoteSnapshot) {
        let crate::sources::remote_tmux::RemoteSnapshot {
            host: snapshot_host,
            sessions,
            claude_panes,
            is_error,
        } = snapshot;
        let tick = self.state.tick;
        let base = self.state.remote_poll_ticks;
        if let Some(host) = self.state.remote_hosts.iter_mut().find(|h| h.hostname == snapshot_host) {
            if is_error {
                host.status = crate::state::HostStatus::Unreachable;
                host.fail_count += 1;
                host.next_poll_tick = Self::backoff_ticks(base, host.fail_count, tick);
            } else {
                host.status = crate::state::HostStatus::Online;
                host.sessions = sessions;
                host.fail_count = 0;
                host.next_poll_tick = tick + base;
            }
        } else {
            let (status, fail_count, next_poll_tick) = if is_error {
                (crate::state::HostStatus::Unreachable, 1, Self::backoff_ticks(base, 1, tick))
            } else {
                (crate::state::HostStatus::Online, 0, tick + base)
            };
            self.state.remote_hosts.push(crate::state::RemoteHost {
                hostname: snapshot_host.clone(),
                status,
                sessions,
                fail_count,
                next_poll_tick,
            });
            // Keep hosts sorted alphabetically.
            self.state.remote_hosts.sort_by(|a, b| {
                a.hostname.to_ascii_lowercase().cmp(&b.hostname.to_ascii_lowercase())
            });
        }

        // Seed provisional Agent entries for each pane on this host that
        // currently has a Claude process running. Hooks (when they
        // eventually fire) promote these in-place — see
        // `resolve_hook_session`'s `remote:<host>:<pane>` branch.
        //
        // Then reconcile the other way: if we have an agent for this host
        // whose pane no longer shows up in the snapshot, treat it as gone
        // (fade to Completed, then prune). Without this step, provisional
        // remote agents linger after the user kills their Claude pane.
        if !is_error {
            self.seed_remote_provisional_agents(&snapshot_host, &claude_panes);
            let live: std::collections::HashSet<&str> = claude_panes
                .iter()
                .map(|p| p.pane_id.as_str())
                .collect();
            self.state.reconcile_remote_panes(&snapshot_host, &live);
        }

        // Clamp selection.
        let count = self.state.remote_item_count();
        if count > 0 {
            self.state.remote_selected = self.state.remote_selected.min(count - 1);
        }
    }

    /// Create a provisional `remote:<host>:<pane>` session for every
    /// Claude pane reported by the latest poll, skipping any we already
    /// track (either as an unpromoted provisional or as a real hook-
    /// discovered session).
    fn seed_remote_provisional_agents(
        &mut self,
        host: &str,
        claude_panes: &[crate::sources::remote_tmux::RemoteClaudePane],
    ) {
        for pane in claude_panes {
            let provisional_id = format!("remote:{host}:{}", pane.pane_id);
            let already_tracked = self.state.sessions.iter().any(|s| {
                s.id == provisional_id
                    || (s.host.as_deref() == Some(host)
                        && s.tmux_pane.as_deref() == Some(pane.pane_id.as_str()))
            });
            if already_tracked {
                continue;
            }

            let cwd = if pane.cwd.is_empty() {
                format!("remote:{host}") // fallback; display_name needs a cwd
            } else {
                pane.cwd.clone()
            };

            let mut session = Session::new(provisional_id, 0, cwd.clone());
            session.status = SessionStatus::Idle;
            session.tmux_pane = Some(pane.pane_id.clone());
            session.host = Some(host.to_string());
            // Paths from the remote generally don't resolve against the
            // local git; fall back to the literal cwd so the session
            // still groups sensibly in the Agents list.
            session.repo_root = Some(
                crate::worktree::repo_common_root(&cwd).unwrap_or_else(|| cwd.clone()),
            );
            tracing::info!(
                "seeded provisional remote agent: host={host} pane={} cwd={cwd}",
                pane.pane_id
            );
            self.state.sessions.push(session);
        }
    }

    fn on_session_discovered(&mut self, file: crate::sources::lifecycle::SessionFile) {
        // Resolve the session's transcript. Prefer the lifecycle file's own
        // `sessionId` when its transcript still exists on disk — that path
        // is unambiguous even with N>1 Claudes in the same cwd. Only fall
        // back to `most_recent_transcript_session` (a cwd-level lookup)
        // when the lifecycle id was invalidated by `/clear` and its file
        // is gone; without that fallback, post-`/clear` sessions would
        // attach to a stale transcript whose last prompt is obsolete.
        let by_id_path = transcript::transcript_path(&file.cwd, &file.session_id);
        let (transcript_path, session_id) = if by_id_path.exists() {
            (by_id_path, file.session_id.clone())
        } else {
            match transcript::most_recent_transcript_session(&file.cwd) {
                Some((path, id)) => (path, id),
                None => (by_id_path, file.session_id.clone()),
            }
        };

        // If pre-created by hook (pid=0), update with real pid now.
        if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id && s.pid == 0) {
            s.pid = file.pid;
            return;
        }

        // Fall back to pane-based convergence. The hook's stamped
        // session_id diverges from the transcript's after `/clear`, so
        // the id match above can miss; without this fallback the
        // provisional stays at pid=0 forever and escapes the reaper.
        let tmux_pane = tmux::find_pane_for_pid(file.pid);
        if let Some(pane) = tmux_pane.as_deref()
            && self.state.promote_pidless_by_pane(file.pid, pane)
        {
            return;
        }

        // Decide whether to insert and under what id. `lifecycle_session_id`
        // skips when this event already maps to a tracked session, and
        // returns a synthetic `lifecycle:<pid>` when N>1 Claudes in the
        // same cwd both fell through to the most-recent-transcript fallback
        // and now collide on `session_id`.
        let Some(session_id) =
            self.state
                .lifecycle_session_id(&session_id, file.pid, tmux_pane.as_deref())
        else {
            return;
        };

        let mut session = Session::new(session_id, file.pid, file.cwd.clone());
        session.status = SessionStatus::Idle;
        session.tmux_pane = tmux_pane;
        session.transcript_path = Some(transcript_path.to_string_lossy().into_owned());
        session.repo_root = Some(
            crate::worktree::repo_common_root(&file.cwd).unwrap_or_else(|| file.cwd.clone()),
        );
        if let Some(mut info) = transcript::read_latest(&transcript_path) {
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
        session.repo_root =
            Some(crate::worktree::repo_common_root(&cwd).unwrap_or_else(|| cwd.clone()));
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
            if let Some(note) = self.state.apply_bookmark_key(key.code, ctrl)
                && let Some(session) = self.state.selected_session() {
                    let cwd = session.cwd.clone();
                    if note.is_empty() {
                        self.state.bookmarks.remove(&cwd);
                    } else {
                        self.state.bookmarks.insert(cwd, note);
                    }
                    crate::state::save_bookmarks(&self.state.bookmarks);
                }
            return Ok(false);
        }
        if self.state.new_agent_mode {
            if let Some((prompt, cwd)) = self.state.apply_new_agent_key(key.code, ctrl) {
                self.spawn_new_agent(&cwd, &prompt);
            }
            return Ok(false);
        }
        if self.state.pr_picker_mode {
            // Ctrl-C exits lonko entirely (same as the main handler). This
            // keeps the shortcut consistent even when a modal is open — the
            // previous "swallow Ctrl-C to close the modal" behavior left
            // users hitting Ctrl-C repeatedly with no visible effect when
            // the overlay didn't render in narrow panes.
            if ctrl && matches!(key.code, KeyCode::Char('c')) {
                self.state.clear_pr_picker();
                return Ok(true);
            }
            if let Some(submit) = self.state.apply_pr_picker_key(key.code, ctrl)
                && !submit.cwd.is_empty()
            {
                self.spawn_pr_by_number(&submit.cwd, submit.number, &submit.title);
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
        if self.state.show_help {
            match key.code {
                KeyCode::Esc
                | KeyCode::Char('?')
                | KeyCode::Char('h')
                | KeyCode::Char('q') => self.state.show_help = false,
                _ => {}
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
                    self.state.tmux_selected = 0;
                    self.state.tmux_window_cursor = None;
                    self.state.tmux_expanded = false;
                } else if self.state.show_detail {
                    self.state.show_detail = false;
                } else {
                    let _ = tmux::select_last_pane();
                }
            }
            KeyCode::Char('/') => { self.state.search_mode = true; }
            KeyCode::Char('?') => { self.state.show_help = true; }
            KeyCode::Char('d') => {
                self.state.show_detail = !self.state.show_detail;
                if self.state.show_detail { self.refresh_selected_transcript(); }
            }
            KeyCode::Char('q') => { self.hide_panel(); }
            KeyCode::Char('c') if ctrl => return Ok(true),
            KeyCode::Char('j') | KeyCode::Down => {
                match self.state.active_tab {
                    Tab::Sessions => {
                        if self.state.tmux_expanded {
                            self.state.navigate_tmux_window(1);
                        } else {
                            self.state.navigate_tmux_session(1);
                        }
                    }
                    Tab::Remote => self.state.navigate_remote(1),
                    _ => {
                        self.state.select_next();
                        if self.state.show_detail { self.refresh_selected_transcript(); }
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.state.active_tab {
                    Tab::Sessions => {
                        if self.state.tmux_expanded {
                            self.state.navigate_tmux_window(-1);
                        } else {
                            self.state.navigate_tmux_session(-1);
                        }
                    }
                    Tab::Remote => self.state.navigate_remote(-1),
                    _ => {
                        self.state.select_prev();
                        if self.state.show_detail { self.refresh_selected_transcript(); }
                    }
                }
            }
            KeyCode::Char('h') if self.state.active_tab == Tab::Agents => {
                self.state.show_help = true;
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
            KeyCode::Char('R') => {
                self.toggle_remote_support();
            }
            KeyCode::Char('r') if self.state.remote_enabled => {
                self.state.active_tab = Tab::Remote;
                self.state.tmux_window_cursor = None;
                self.state.tmux_expanded = false;
            }
            KeyCode::Char(' ')
                if self.state.active_tab == Tab::Agents =>
            {
                // Toggle collapse on the selected session's repo group.
                if let Some(session) = self.state.selected_session()
                    && let Some(repo) = session.repo_root.clone() {
                        self.state.toggle_group_collapse(&repo);
                    }
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
                    let active_idx = self.state.selected_tmux_session()
                        .and_then(|s| s.windows.iter().position(|w| w.active))
                        .unwrap_or(0);
                    self.state.tmux_window_cursor = Some(active_idx);
                }
            }
            KeyCode::Enter => {
                if self.state.active_tab == Tab::Sessions {
                    self.focus_tmux_session();
                    self.state.tmux_expanded = false;
                } else if self.state.active_tab == Tab::Remote {
                    self.attach_remote_session();
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
            KeyCode::Char('g') if self.state.active_tab == Tab::Agents => {
                self.launch_worktree_prompt();
            }
            KeyCode::Char('p') if self.state.active_tab == Tab::Agents => {
                self.open_pr_picker();
            }
            KeyCode::Char('e') if self.state.active_tab == Tab::Agents => {
                // Expand / collapse subagents inline under the selected main.
                // No-op on subagent cards (you can only toggle from the parent).
                if let Some(s) = self.state.selected_session()
                    && !s.is_subagent()
                    && self.state.subagent_count_for(&s.id) > 0
                {
                    let parent_id = s.id.clone();
                    self.state.toggle_subagent_expand(&parent_id);
                }
            }
            // Permission shortcuts (y=yes/1, w=always/2, n=no/3)
            KeyCode::Char('y') => self.send_permission("1"),
            KeyCode::Char('w') => self.send_permission("2"),
            KeyCode::Char('n') if self.state.has_waiting() => self.send_permission("3"),
            KeyCode::Char('n') if self.state.active_tab == Tab::Agents => {
                self.launch_new_agent_prompt();
            }
            KeyCode::Char('x') if !self.state.has_waiting() => {
                match self.state.active_tab {
                    Tab::Agents  => self.kill_and_remove_worktree(),
                    Tab::Sessions => self.kill_selected_tmux_session(),
                    Tab::Remote  => self.toggle_exclude_remote_host(),
                }
            }
            KeyCode::Char('X') if !self.state.has_waiting() => {
                match self.state.active_tab {
                    Tab::Agents  => self.kill_selected_agent(),
                    Tab::Sessions => self.kill_selected_tmux_session(),
                    Tab::Remote  => {
                        // Restore all excluded hosts.
                        if !self.state.excluded_hosts.is_empty() {
                            self.state.excluded_hosts.clear();
                            crate::config::save_excluded_hosts(&self.state.excluded_hosts);
                        }
                    }
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let n = (c as u8 - b'0') as usize;
                self.focus_nth(n);
            }
            _ => {}
        }
        Ok(false)
    }

    /// Check whether `pane` belongs to the same tmux window as lonko.
    /// Returns `true` when the pane should NOT be killed. Window-scoped
    /// (not session-scoped) so a worktree agent running in a sibling
    /// window of lonko's own tmux session can still be torn down.
    fn is_own_tmux_window(&self, pane: Option<&str>) -> bool {
        let Some(own) = &self.state.own_pane else { return false };
        let Some(p) = pane else { return false };
        // Fast path: same pane ID.
        if own == p { return true; }
        // Slow path: resolve both to their tmux window ID (globally unique).
        let own_win = tmux::tmux_window_for_pane(own);
        let tgt_win = tmux::tmux_window_for_pane(p);
        matches!((own_win, tgt_win), (Some(a), Some(b)) if a == b)
    }

    /// Soft kill: send Ctrl-C to the selected agent's tmux pane.
    fn kill_selected_agent(&mut self) {
        let Some(session) = self.state.selected_session() else { return };
        if matches!(session.status, SessionStatus::Completed) { return; }
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));
        // Never send Ctrl-C to lonko's own pane.
        if self.is_own_tmux_window(pane.as_deref()) {
            return;
        }
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

        let cwd = session.cwd.clone();
        let pid = session.pid;
        let session_id = session.id.clone();
        let branch = session.branch.clone();
        let repo_root = session.repo_root.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));

        // Never kill the tmux window lonko is running in.
        // Compare at the window level (not session) because tmux kill-window
        // only removes the single window, so a worktree agent in a sibling
        // window of lonko's own session can still be safely torn down.
        if self.is_own_tmux_window(pane.as_deref()) {
            return;
        }

        // Worktree was removed externally: the cwd no longer exists on disk,
        // so `is_worktree` and `git worktree remove` would both fail. Tear
        // down the agent anyway (kill pane + drop from state) and prune the
        // dangling worktree entry from the main repo so the branch can be
        // cleaned up. Without this, the agent stays Completed for the prune
        // TTL and a second `x` is a no-op.
        if !std::path::Path::new(&cwd).exists() {
            self.kill_orphan_worktree_agent(&session_id, pane, branch, repo_root);
            return;
        }

        if !crate::worktree::is_worktree(&cwd) {
            // Not a worktree — fall back to soft kill
            self.kill_selected_agent();
            return;
        }

        // Send Ctrl-C to stop Claude
        if let Some(ref p) = pane {
            let _ = tmux::send_ctrl_c(p);
        }

        // Keep the pane ID so we can kill just its window (not the whole session).
        let target_pane = pane.clone();

        // Remove from lonko state
        self.state.sessions.retain(|s| s.id != session_id);
        // Clamp selection against the visible list (which may be filtered).
        let vlen = self.state.visible_len();
        if vlen > 0 {
            self.state.selected = self.state.selected.min(vlen - 1);
        } else {
            self.state.selected = 0;
        }
        // Update cache immediately so `lonko focus N` reflects the change.
        self.write_sessions_cache();

        // Resolve branch (fall back to live git) and main repo before moving into the closure.
        let branch = branch.or_else(|| crate::sources::transcript::git_branch(&cwd));
        let main_repo = crate::worktree::repo_common_root(&cwd);

        // Background cleanup: kill tmux window + remove worktree + clean merged branch.
        // Bail if we couldn't resolve the pane — worktree and window
        // should live and die together.
        let Some(target_pane) = target_pane else { return };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tmux::kill_window(&target_pane);
            if let Err(e) = crate::worktree::remove(&cwd) {
                tmux::display_message(&format!("worktree remove: {e}"));
                return;
            }

            // Try to clean up the branch left behind by the worktree.
            if let (Some(branch), Some(repo)) = (branch, main_repo) {
                let msg = match crate::worktree::cleanup_branch(&repo, &branch) {
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: true,
                        remote_deleted: true,
                    } => format!("cleaned up local + remote branch '{branch}' (PR merged)"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: true,
                        remote_deleted: false,
                    } => format!("branch '{branch}': local deleted, remote delete failed"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: false,
                        remote_deleted: true,
                    } => format!("branch '{branch}': remote deleted, local delete failed"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: false,
                        remote_deleted: false,
                    } => format!("branch '{branch}': PR merged but branch delete failed"),
                    crate::worktree::CleanupOutcome::SafeDeleted => {
                        format!("deleted branch '{branch}' (no unique commits)")
                    }
                    crate::worktree::CleanupOutcome::Kept => {
                        format!("kept branch '{branch}' (has unique commits)")
                    }
                };
                tmux::display_message(&msg);
            }
        });
    }

    /// Tear down an agent whose worktree directory no longer exists on disk.
    /// Removes the session from state, kills the tmux window, prunes the
    /// stale worktree administrative entry from the main repo, and tries to
    /// clean up the orphan branch.
    fn kill_orphan_worktree_agent(
        &mut self,
        session_id: &str,
        pane: Option<String>,
        branch: Option<String>,
        repo_root: Option<String>,
    ) {
        if let Some(ref p) = pane {
            let _ = tmux::send_ctrl_c(p);
        }

        self.state.sessions.retain(|s| s.id != session_id);
        let vlen = self.state.visible_len();
        if vlen > 0 {
            self.state.selected = self.state.selected.min(vlen - 1);
        } else {
            self.state.selected = 0;
        }
        self.write_sessions_cache();

        let Some(target_pane) = pane else { return };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tmux::kill_window(&target_pane);

            // Drop the dangling worktree entry from the main repo so a later
            // `git branch -d` doesn't refuse with "branch is checked out".
            let Some(repo) = repo_root else { return };
            if let Err(e) = crate::worktree::prune(&repo) {
                tmux::display_message(&format!("worktree prune: {e}"));
                return;
            }

            if let Some(branch) = branch {
                let msg = match crate::worktree::cleanup_branch(&repo, &branch) {
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: true,
                        remote_deleted: true,
                    } => format!("cleaned up local + remote branch '{branch}' (PR merged)"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: true,
                        remote_deleted: false,
                    } => format!("branch '{branch}': local deleted, remote delete failed"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: false,
                        remote_deleted: true,
                    } => format!("branch '{branch}': remote deleted, local delete failed"),
                    crate::worktree::CleanupOutcome::Merged {
                        local_deleted: false,
                        remote_deleted: false,
                    } => format!("branch '{branch}': PR merged but branch delete failed"),
                    crate::worktree::CleanupOutcome::SafeDeleted => {
                        format!("deleted branch '{branch}' (no unique commits)")
                    }
                    crate::worktree::CleanupOutcome::Kept => {
                        format!("kept branch '{branch}' (has unique commits)")
                    }
                };
                tmux::display_message(&msg);
            }
        });
    }

    /// Kill the tmux session selected in the Sessions tab.
    /// Refuses to kill the session lonko itself is running in.
    fn kill_selected_tmux_session(&self) {
        let Some(ts) = self.state.selected_tmux_session() else { return };

        // Never kill lonko's own tmux session.
        if let Some(own_pane) = &self.state.own_pane
            && let Some(own_sess) = tmux_session_for_pane(own_pane)
                && own_sess == ts.name {
                    return;
                }

        let name = ts.name.clone();
        std::thread::spawn(move || {
            let _ = tmux::kill_session(&name);
        });
    }

    /// Enter worktree mode: resolve the cwd and start accepting branch name input.
    fn launch_worktree_prompt(&mut self) {
        let cwd = if self.state.active_tab == Tab::Agents {
            self.state.selected_session().map(|s| s.cwd.clone())
        } else {
            self.state.selected_tmux_session()
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
                tmux::display_message(&format!("worktree: {e}"));
            }
        });
    }

    /// Enter new-agent mode: resolve a cwd and show the prompt popup.
    fn launch_new_agent_prompt(&mut self) {
        let cwd = if self.state.active_tab == crate::state::Tab::Agents {
            self.state.selected_session().map(|s| s.cwd.clone())
        } else {
            self.state.selected_tmux_session()
                .and_then(|s| tmux::session_cwd(&s.name))
        };

        // Fallback: active tmux pane's cwd
        let cwd = cwd.or_else(|| {
            tmux::active_pane().and_then(|p| {
                std::process::Command::new("tmux")
                    .args(["display-message", "-t", &p, "-p", "#{pane_current_path}"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        });

        self.state.open_new_agent(cwd.unwrap_or_default());
    }

    /// Spawn a new Claude Code agent in a background thread.
    fn spawn_new_agent(&self, cwd: &str, prompt: &str) {
        let cwd = cwd.to_string();
        let prompt = prompt.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::new_agent::run(&cwd, &prompt) {
                tmux::display_message(&format!("new-agent: {e}"));
            }
        });
    }

    /// Open the PR picker modal: mark it as loading and kick off a
    /// background `gh pr list` for the selected agent's repo. The fetch
    /// result comes back through `Event::PrPickerLoaded`, which the main
    /// handler writes into `AppState`.
    fn open_pr_picker(&mut self) {
        let cwd = self
            .state
            .selected_session()
            .map(|s| s.cwd.clone())
            .or_else(|| self.state.sessions.iter().find(|s| s.host.is_none()).map(|s| s.cwd.clone()));
        let Some(cwd) = cwd else {
            tmux::display_message("pr-picker: no local agent to anchor the repo");
            return;
        };
        if !crate::worktree::has_gh() {
            tmux::display_message("pr-picker: `gh` CLI not found");
            return;
        }
        self.state.pr_picker_mode = true;
        self.state.pr_picker_loading = true;
        self.state.pr_picker_error = None;
        self.state.pr_picker_prs.clear();
        self.state.pr_picker_selected = 0;
        self.state.pr_picker_query.clear();
        self.state.pr_picker_cwd = Some(cwd.clone());

        let Some(ref tx) = self.scan_tx else { return };
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::worktree::list_open_prs(&cwd);
            let _ = tx.send(Event::PrPickerLoaded { cwd, result });
        });
    }

    /// Create a worktree + tmux session + claude for an arbitrary PR number.
    /// Used by the PR picker when the user confirms a row with Enter.
    fn spawn_pr_by_number(&self, cwd: &str, number: u32, title: &str) {
        let cwd = cwd.to_string();
        let title = title.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::worktree::run_from_pr_number(&cwd, number, &title) {
                tmux::display_message(&format!("pr-picker: {e}"));
            }
        });
    }

    /// Write the ordered session list to two cache files:
    /// - ~/.cache/lonko-sessions: one pane_id per line (for `lonko focus N`)
    /// - ~/.cache/lonko-sessions-info: "N\tname\tcwd" per line (for shortcut-list.sh)
    fn write_sessions_cache(&self) {
        let sessions = self.state.ordered_sessions();

        // Pane IDs file (for lonko focus N)
        // Write ALL sessions (one per line) in canonical display order
        // (grouped by repo, trunk first) so `lonko focus N` matches the UI.
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
            .map(|(i, s)| format!("{}\t{}\t{}\n", i + 1, s.display_name(), s.cwd))
            .collect();
        let info_path = crate::state::lonko_cache_dir().join("lonko-sessions-info");
        let _ = std::fs::write(info_path, info_content);
    }


    /// Focus the Nth session (1-indexed) in canonical display order by switching
    /// the tmux client.  Uses `ordered_sessions()` so the numbering matches
    /// the cache file read by `lonko focus N`.
    fn focus_nth(&mut self, n: usize) {
        let sessions = self.state.ordered_sessions();
        let Some(session) = sessions.get(n.saturating_sub(1)) else { return };
        let pid = session.pid;
        let session_id = session.id.clone();
        let pane = session.tmux_pane.clone()
            .or_else(|| tmux::find_pane_for_pid(pid));
        if let Some(ref pane) = pane {
            if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                s.tmux_pane = Some(pane.clone());
            }
            // Set selected to the matching index in the visible list (which
            // may differ from the ordered list when a search filter is active).
            let vis = self.state.visible_sessions();
            self.state.selected = vis.iter()
                .position(|s| s.id == session_id)
                .unwrap_or(0);
            self.focus_local_agent_pane(pane);
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
            host: None,
            agent_id: None,
            agent_type: None,
            agent_transcript_path: None,
        }
    }

    fn mk_session() -> Session {
        Session::new("s1".into(), 100, "/tmp/proj".into())
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
        let sentinel = crate::state::lonko_cache_dir().join("lonko-no-follow");
        let _ = std::fs::remove_file(&sentinel);

        write_no_follow_sentinel();

        assert!(sentinel.exists());
        let _ = std::fs::remove_file(&sentinel);
    }
}
