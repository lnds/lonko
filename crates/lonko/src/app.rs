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
    sources::{chat, hooks, lifecycle, transcript, tmux_scanner},
    state::{AppState, KeyOutcome, Session, SessionStatus, Tab},
    ui,
};

mod chat_view;
mod inflight;
mod navigate;
mod panel;
mod remote;
pub use remote::attach_remote_agent;
use inflight::InflightGuard;

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

/// Send a desktop notification when a session needs attention.
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
    /// Last click bookkeeping for double-click detection. The `String`
    /// is a stable identifier of the row that was clicked (session id
    /// in the Agents tab, tmux session name in the Sessions tab) so a
    /// reorder of the underlying list between the two clicks doesn't
    /// fire the double-click action on a different agent that happens
    /// to have shifted into the original row's index.
    last_click: Option<(Tab, usize, std::time::Instant, String)>,
    /// When the last focus/attach action fired. `attach_remote_agent`
    /// and `focus_local_agent_pane` block the event loop for 1-3 s
    /// (ssh handshakes, switch-client round-trips), and any mouse
    /// clicks the user made meanwhile queue up on the event channel.
    /// When that backlog drains, paired clicks can re-trigger the
    /// double-click action on whatever happens to be selected next.
    /// Inside this lockout window we degrade subsequent clicks to
    /// plain selection so a slow attach can't multi-fire. The stamp
    /// is taken AFTER the action returns (not before) so the lockout
    /// covers the entire backlog of clicks that arrived during the
    /// blocking work.
    last_action_at: Option<std::time::Instant>,
    /// In-flight guard for the per-2 s `TmuxSessionsRefreshed` job and
    /// the per-5 s `tmux_scanner::scan` job. Both run on
    /// `spawn_blocking`; without these flags a slow tmux server
    /// (think NFS home, hung process) would let successive ticks pile
    /// new blocking tasks on top of the previous ones, draining as a
    /// burst of redundant state updates when tmux finally answers.
    /// Each flag is cleared by the spawned task on completion (panic
    /// or otherwise) via a guard struct.
    sessions_refresh_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tmux_scan_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// In-flight guard for the per-30 s open-PR refresh. The blocking task
    /// fans out one `gh pr list` per unique local `repo_root`; each call
    /// can take 100-500 ms, and stacking them on a slow network would
    /// burn `gh` invocations without delivering fresher data.
    pr_refresh_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// In-flight guard for the per-second `active_pane()` poll. The two
    /// tmux forks (`list-clients` + `display-message`) used to run on
    /// the event loop, blocking the render every second whenever tmux
    /// was busy. Now scheduled on `spawn_blocking`, with this flag
    /// preventing successive ticks from stacking new tasks while the
    /// previous one is still running.
    active_pane_inflight: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    /// Suppresses auto-hide while a panel-move is in flight.
    /// `focus_local_agent_pane`, `attach_remote_agent`, and
    /// `attach_remote_session` all trigger tmux state transitions
    /// (`break-pane`/`join-pane`/`switch-client`) that fire
    /// `client-session-changed` and `after-select-window` hooks
    /// asynchronously. During the transient window between break-pane
    /// and the destination's join-pane, alone-detection can see lonko
    /// briefly parked alone in a session-of-one and call `hide_panel`,
    /// making lonko visibly disappear after a normal agent switch.
    /// Set this to `now + 500ms` at the start of every move op; the
    /// alone-detection short-circuits while it is in the future.
    panel_moving_until: Option<std::time::Instant>,
    /// Live registry of `lonko-channel` plugin connections, keyed by
    /// PPID (= the Claude Code session's PID). Used to push `chat.send`
    /// frames into a specific agent's running channel plugin.
    chat_registry: chat::Registry,
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
            last_action_at: None,
            sessions_refresh_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tmux_scan_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pr_refresh_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            active_pane_inflight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            focus_gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            remote_bridges: std::collections::HashMap::new(),
            remote_bridge_starting: std::collections::HashSet::new(),
            remote_online_hosts: std::collections::HashSet::new(),
            panel_moving_until: None,
            chat_registry: chat::Registry::new(),
        }
    }

    /// Schedule a transcript read + git_branch lookup off the event loop.
    /// Result lands as `Event::TranscriptInfoLoaded` and is applied via
    /// the main task. The blocking parse can take tens of milliseconds
    /// for a large JSONL plus the `git rev-parse` fork — running it
    /// inline (as the Stop hook used to) would stall the render loop
    /// every time an agent finished.
    fn spawn_transcript_load(
        &self,
        session_id: String,
        path: std::path::PathBuf,
        cwd: String,
    ) {
        let Some(ref tx) = self.scan_tx else { return };
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let info = transcript::read_latest(&path);
            let branch = transcript::git_branch(&cwd);
            if info.is_some() || branch.is_some() {
                let _ = tx.send(Event::TranscriptInfoLoaded { session_id, info, branch });
            }
        });
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

        // Spawn the chat socket listener (lonko-channel plugin connections)
        chat::spawn_listener(tx.clone(), self.chat_registry.clone())
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
            &visible, self.state.selected, list_h, &header_flags, &collapsed_flags, &remote_sep_flags, &self.state,
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
            let ch = ui::list::card_height(
                s,
                &self.state.bookmarks,
                ui::list::pr_info_for_session(&self.state, s),
            );
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

        // Identify the row by its session id; `None` means the index
        // is out of bounds (list shrunk between layout and click).
        // Index into `visible` (the sorted/filtered list used to compute
        // the layout), not `self.state.sessions` (the raw list). Group
        // sorting reorders `visible` so the two indices diverge as soon
        // as more than one repo group is present, and the bug surfaces
        // as the wrong session being selected/double-clicked.
        let click_id = match visible.get(global_idx).map(|s| s.id.clone()) {
            Some(id) => id,
            None => {
                self.last_click = None;
                return;
            }
        };
        let is_double = self.classify_click(Tab::Agents, global_idx, click_id);

        if is_double {
            // Clear the previous click so the next user click starts
            // fresh. The lockout timestamp itself is stamped AFTER the
            // synchronous action returns (see end of this branch); a
            // pre-stamp would expire halfway through a 3 s attach and
            // still let some queued clicks slip past.
            self.last_click = None;
            self.state.selected = global_idx;
            let session = self.state.selected_session().cloned();
            if let Some(session) = session {
                // Remote agent: route attach through SSH (same behavior
                // as Enter), then bail out of the local focus-retry loop.
                if let Some(host) = session.host.as_deref() {
                    if let Some(pane) = session.tmux_pane.as_deref() {
                        self.mark_panel_moving();
                        attach_remote_agent(host, pane);
                    }
                    self.last_action_at = Some(std::time::Instant::now());
                    return;
                }
                let pid = session.pid;
                let session_id = session.id.clone();
                let pane = session.tmux_pane.clone()
                    .or_else(|| tmux::find_pane_for_pid(pid));
                if let Some(p) = pane {
                    self.state.cache_pane_for_session(&session_id, &p);
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
                    self.last_action_at = Some(std::time::Instant::now());
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

        // Identify the row by its session name (Sessions tab uses
        // names, Agents tab uses ids). The capture defends against
        // list refreshes between clicks routing the action to a
        // different session that slid into the same index.
        let click_id = match self.state.visible_tmux_sessions()
            .get(global_idx)
            .map(|s| s.name.clone())
        {
            Some(id) => id,
            None => {
                self.last_click = None;
                return;
            }
        };
        let is_double = self.classify_click(Tab::Sessions, global_idx, click_id);

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
            // Clear the previous click and stamp the lockout AFTER the
            // synchronous action returns (`focus_tmux_session` issues a
            // switch-client) so the lockout window covers any clicks
            // that arrived during the action.
            self.last_click = None;
            self.state.tmux_selected = global_idx;
            if let Some(win) = window_row {
                self.state.tmux_window_cursor = Some(win);
            }
            self.focus_tmux_session();
            self.last_action_at = Some(std::time::Instant::now());
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
    /// Evaluate a click at `(tab, global_idx)` with the row's stable
    /// id (session id for Agents, session name for Sessions) and
    /// update the last-click bookkeeping. Returns `true` when the
    /// click should be treated as a double-click: same tab, same row
    /// id, ≤ 400 ms since the previous click, and we're not in the
    /// post-action lockout window. Single-click otherwise.
    ///
    /// The lockout timestamp is stamped by the caller AFTER the
    /// synchronous focus/attach action returns, so the whole
    /// click backlog the user racked up during the action is
    /// covered when it finally drains.
    fn classify_click(&mut self, tab: Tab, global_idx: usize, click_id: String) -> bool {
        const ACTION_LOCKOUT_MS: u128 = 1000;
        const DOUBLE_CLICK_MS: u128 = 400;
        let now = std::time::Instant::now();
        let in_lockout = self.last_action_at
            .is_some_and(|t| now.duration_since(t).as_millis() < ACTION_LOCKOUT_MS);
        let is_double = !in_lockout
            && self.last_click
                .as_ref()
                .is_some_and(|(last_tab, last_idx, last_time, last_id)| {
                    *last_tab == tab
                        && *last_idx == global_idx
                        && last_id == &click_id
                        && now.duration_since(*last_time).as_millis() < DOUBLE_CLICK_MS
                });
        self.last_click = Some((tab, global_idx, now, click_id));
        is_double
    }

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


    fn refresh_selected_transcript(&self) {
        // Index via the visible list (matches `selected_session()`); the
        // raw `self.state.sessions[self.state.selected]` lookup that this
        // used to do drifted to the wrong session whenever group sorting
        // reordered the list.
        let Some(session) = self.state.selected_session() else { return };
        let session_id = session.id.clone();
        let cwd = session.cwd.clone();
        let path = session.transcript_path.clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| transcript::transcript_path(&session.cwd, &session.id));
        // Off the event loop: the JSONL parse and `git rev-parse` fork
        // were the leading source of per-keystroke lag in the detail view.
        self.spawn_transcript_load(session_id, path, cwd);
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
        // Capture the agent label up front so we can surface it in the
        // post-response notification — once the panel auto-hides, the
        // user has no other cue indicating which agent received the
        // y/n/w they just pressed.
        let display_name = session.display_name().to_string();
        // For remote sessions there is no usable local PID, so we can only
        // trust whatever pane ID the bridge already propagated.
        let pane = if host.is_some() {
            session.tmux_pane.clone()
        } else {
            session.tmux_pane.clone()
                .or_else(|| tmux::find_pane_for_pid(pid))
        };
        if let Some(ref p) = pane {
            self.state.cache_pane_for_session(&session_id, p);
        }
        let Some(pane) = pane else { return };
        let result = match host.as_deref() {
            Some(h) => tmux::send_keys_remote(h, &pane, key),
            None    => tmux::send_keys(&pane, key),
        };
        if let Err(e) = result {
            tracing::warn!("send_permission failed: {e}");
            return;
        }

        // Surface which agent the response went to. Stationary-panel
        // model means lonko hides as soon as no agent is still
        // waiting, so without this notification the user has no
        // record of which prompt they just answered.
        let action = match key {
            "1" | "y" => "approved",
            "3" | "w" => "always allowed",
            _         => "denied",
        };
        let summary = format!("lonko · {display_name} · {action}");
        std::thread::spawn(move || {
            let _ = notify_rust::Notification::new()
                .summary(&summary)
                .timeout(notify_rust::Timeout::Milliseconds(4000))
                .show();
        });

        // Auto-hide once no agent is still waiting. The hook that
        // actually flips this session's status to non-waiting hasn't
        // landed yet (the agent's `Stop` is what does that), so
        // optimistically count this session as no-longer-waiting:
        // anything else still in `is_waiting()` means the user has
        // more work to do and lonko should remain visible.
        let still_waiting = self.state.sessions
            .iter()
            .filter(|s| s.id != session_id)
            .any(|s| s.status.is_waiting());
        if !still_waiting {
            self.hide_panel();
        }
    }

    fn handle_hook(&mut self, payload: crate::sources::hooks::HookPayload) {
        // Telemetry for remote hooks (the local hook firehose is too
        // chatty to log every one). Lives in App rather than AppState
        // because tracing is an orchestration concern.
        if let Some(host) = payload.host.as_deref() {
            tracing::info!(
                "remote hook host={host} event={:?} session={:?} tmux_pane={:?} cwd={:?}",
                payload.hook_event_name,
                payload.session_id,
                payload.tmux_pane,
                payload.cwd,
            );
        }

        // Compute the live git branch outside of `apply_hook` so the
        // state layer stays free of `git rev-parse` forks. Only forks
        // when the payload actually carries a cwd.
        let live_branch = payload.cwd.as_deref()
            .filter(|c| !c.is_empty())
            .and_then(transcript::git_branch);

        let Some(effect) = self.state.apply_hook(&payload, live_branch) else {
            if payload.host.is_some() && payload.session_id.is_none() {
                tracing::warn!("remote hook dropped: no session_id");
            }
            return;
        };

        // Two distinct attention paths. When Ghostty is NOT focused,
        // fire a desktop notification so the user gets pulled in via
        // macOS notif center. When Ghostty IS focused and the hook
        // newly transitioned the agent into WaitingForUser, auto-show
        // lonko alongside the user's current window so the permission
        // prompt is visible inline (`-d` keeps focus on the agent).
        if !ghostty::has_focus() {
            notify_if_needed(&effect.display_name, &effect.status);
        } else if effect.is_now_waiting {
            self.auto_show_panel();
        }

        // Defer the transcript parse + git_branch fork off the event
        // loop for any Stop-style hook. The deferred result lands as
        // `TranscriptInfoLoaded` and refines model / cost / last_prompt
        // / branch on the same session a few ms later.
        if let Some(seed) = effect.transcript_seed {
            self.spawn_transcript_load(seed.session_id, seed.path, seed.cwd);
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Tick                                       => {
                self.on_tick();
                // Fallback alone-detection: `TmuxPaneGone` only fires for
                // panes lonko had tracked as running Claude, so closing
                // a plain shell pane wouldn't trigger it. Every 2 s
                // (20 ticks * 100 ms), offset by 11 to avoid colliding
                // with the other periodic tasks scheduled in on_tick
                // (% 10 == 1, % 20 == 3, is_multiple_of(10|50)).
                // 3 s startup grace.
                //
                // Was an auto-*quit*; that lost the agents list and the
                // remote bridges every time the user happened to close
                // their last work pane in lonko's session. Now we
                // auto-*hide* to `lonko-tray` instead, keeping the
                // process alive so super+s brings it back instantly.
                if self.state.tick >= 30
                    && self.state.tick % 20 == 11
                    && self.should_self_quit_when_alone()
                {
                    tracing::info!("auto-hide: lonko was alone in its tmux session, parking it back in lonko-tray (tick)");
                    self.hide_panel();
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
                    tracing::info!("auto-hide: lonko was alone in its tmux session after pane-gone, parking it back in lonko-tray");
                    self.hide_panel();
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
            Event::ActivePaneRefreshed(active) => {
                if let Some(ref active) = active {
                    let is_own = self.state.own_pane.as_deref() == Some(active.as_str());
                    if !is_own {
                        let focused_id = self.state.sessions.iter()
                            .find(|s| s.tmux_pane.as_deref() == Some(active.as_str()))
                            .map(|s| s.id.clone());
                        self.state.focused_session_id = focused_id;
                    }
                }
            }
            Event::TranscriptInfoLoaded { session_id, info, branch } => {
                if let Some(s) = self.state.sessions.iter_mut().find(|s| s.id == session_id) {
                    if let Some(mut info) = info {
                        // Live git_branch wins over the (possibly stale) one
                        // baked into the transcript, matching the previous
                        // synchronous behavior.
                        if branch.is_some() { info.branch = branch; }
                        s.apply_transcript_info(info);
                    } else if let Some(branch) = branch {
                        // Transcript was unreadable but we still have a
                        // fresh branch — same fallback the old sync path
                        // used to do.
                        s.branch = Some(branch);
                    }
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
                if !self.state.pr_picker.mode
                    || self.state.pr_picker.cwd.as_deref() != Some(cwd.as_str())
                {
                    return Ok(false);
                }
                self.state.pr_picker.loading = false;
                match result {
                    Ok(prs) => {
                        self.state.pr_picker.prs = prs;
                        self.state.pr_picker.selected = 0;
                        self.state.pr_picker.error = None;
                    }
                    Err(e) => {
                        self.state.pr_picker.error = Some(e);
                    }
                }
            }
            Event::WorktreePickerLoaded { cwd, result } => {
                // Drop the payload if the user already closed the picker or
                // moved on to a different repo.
                if !self.state.worktree_picker.mode
                    || self.state.worktree_picker.cwd.as_deref() != Some(cwd.as_str())
                {
                    return Ok(false);
                }
                self.state.worktree_picker.loading = false;
                match result {
                    Ok(items) => {
                        self.state.worktree_picker.items = items;
                        self.state.worktree_picker.selected = 0;
                        self.state.worktree_picker.error = None;
                    }
                    Err(e) => {
                        self.state.worktree_picker.error = Some(e);
                    }
                }
            }
            Event::PrsByRepoRefreshed { repo_root, items } => {
                let map: std::collections::HashMap<String, crate::state::PrInfo> =
                    items.into_iter().collect();
                self.state.pr_infos_by_repo.insert(repo_root, map);
            }
            Event::ChatOnline { ppid, pid } => {
                self.state.on_chat_online(ppid, pid);
            }
            Event::ChatOffline { ppid } => {
                self.state.on_chat_offline(ppid);
            }
            Event::ChatReply { agent_id, text, in_reply_to } => {
                self.state.on_chat_reply(&agent_id, text, in_reply_to);
            }
            Event::ChatAck { msg_id, status } => {
                self.state.on_chat_ack(&msg_id, &status);
            }
        }
        Ok(false)
    }

    fn on_tick(&mut self) {
        self.state.tick = self.state.tick.wrapping_add(1);
        // Poll the active tmux pane every ~1s to keep the focused session
        // current. The two-fork query (list-clients + display-message)
        // runs on `spawn_blocking` and lands as `Event::ActivePaneRefreshed`
        // so a busy/slow tmux server can't stall the render loop here.
        if self.state.tick.is_multiple_of(10)
            && let Some(ref tx) = self.scan_tx
            && let Some(guard) = InflightGuard::try_acquire(&self.active_pane_inflight)
        {
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let _g = guard;
                let active = tmux::active_pane();
                let _ = tx.send(Event::ActivePaneRefreshed(active));
            });
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
            && let Some(guard) = InflightGuard::try_acquire(&self.sessions_refresh_inflight)
        {
            let claude_panes: std::collections::HashSet<String> = self.state.sessions
                .iter()
                .filter_map(|s| s.tmux_pane.clone())
                .collect();
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                // `guard` clears the in-flight flag on Drop, even on
                // panic — otherwise a blip would leave the refresh
                // permanently stuck.
                let _g = guard;

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
            && let Some(guard) = InflightGuard::try_acquire(&self.tmux_scan_inflight)
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
                let _g = guard;
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

        // Refresh open-PR numbers per repo every 30 s. One `gh pr list` per
        // unique local `repo_root`; results land as `PrsByRepoRefreshed` and
        // populate the cache the agent-card renderer consults to draw the
        // `#NNNN` badge. Errors are logged and ignored — the cache then
        // simply stays at its last good value, so a flaky network doesn't
        // make badges flicker. The `+ 7` offset spaces this work away from
        // the other periodic tasks scheduled in `on_tick` (multiples of 10,
        // 20, 50, 100).
        if self.state.tick % 300 == 7
            && let Some(ref tx) = self.scan_tx
            && let Some(guard) = InflightGuard::try_acquire(&self.pr_refresh_inflight)
        {
            let repos: Vec<String> = self.state.sessions
                .iter()
                .filter(|s| s.host.is_none())
                .filter_map(|s| s.repo_root.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            if repos.is_empty() {
                drop(guard);
            } else {
                let tx = tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _g = guard;
                    if !crate::worktree::has_gh() {
                        return;
                    }
                    for repo_root in repos {
                        // Two queries per repo: open PRs (for the live `#NNNN`
                        // badge) and recently-merged PRs (so the badge can stay
                        // up with a blinking `M` after merge instead of silently
                        // disappearing). Merged loses to open if both report the
                        // same branch — open is the more current truth.
                        let mut items: Vec<(String, crate::state::PrInfo)> = Vec::new();
                        match crate::worktree::list_recent_merged_prs(&repo_root) {
                            Ok(merged) => {
                                for (branch, number) in merged {
                                    items.push((branch, crate::state::PrInfo {
                                        number,
                                        status: crate::state::PrMergeStatus::Merged,
                                    }));
                                }
                            }
                            Err(e) => {
                                tracing::warn!("pr-refresh merged {repo_root}: {e}");
                            }
                        }
                        match crate::worktree::list_open_prs(&repo_root) {
                            Ok(prs) => {
                                let open_branches: std::collections::HashSet<String> =
                                    prs.iter().map(|p| p.branch.clone()).collect();
                                items.retain(|(b, _)| !open_branches.contains(b));
                                for p in prs {
                                    items.push((p.branch, crate::state::PrInfo {
                                        number: p.number,
                                        status: crate::state::PrMergeStatus::Open,
                                    }));
                                }
                                let _ = tx.send(Event::PrsByRepoRefreshed { repo_root, items });
                            }
                            Err(e) => {
                                tracing::warn!("pr-refresh open {repo_root}: {e}");
                                // Still publish merged-only data so the M badge
                                // can keep working even if the open query failed.
                                if !items.is_empty() {
                                    let _ = tx.send(Event::PrsByRepoRefreshed { repo_root, items });
                                }
                            }
                        }
                    }
                });
            }
        }

        // Tailnet peer discovery runs on a background task every 10 s
        // whenever remote support is enabled, independent of the active
        // tab. The resulting `RemotePeersOnline` event keeps
        // `sync_remote_bridges` informed — without it, bridges would
        // only come up while the user happens to be looking at the
        // Remote tab. Tick 1 still fires so first-time discovery
        // happens ~100 ms after lonko launches.
        //
        // Was 2 s; bumped to 10 s because each call shells out to
        // `tailscale status --json` and on a degraded Wi-Fi every
        // invocation pokes Tailscale's Network Extension, which feeds
        // the `airportd`/`nehelper` storm we hit in the field. Peers
        // don't appear/disappear that fast — 10 s is plenty for UX.
        if self.state.remote_enabled
            && (self.state.tick.is_multiple_of(100) || self.state.tick == 1)
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

        // Hard exit always wins, regardless of the active modal. The
        // previous design swallowed Ctrl-C inside `pr_picker.mode` so
        // it could cancel the picker first, but the user could end up
        // hitting Ctrl-C several times with no visible effect when the
        // overlay didn't render in narrow panes. Exit unconditionally
        // — modal state is process-local and doesn't need cleanup.
        if ctrl && matches!(key.code, KeyCode::Char('c')) {
            return Ok(true);
        }

        if let Some(outcome) = self.dispatch_modal_key(key.code, ctrl) {
            return Ok(outcome);
        }
        self.dispatch_normal_key(key.code, ctrl)
    }

    /// Activate the currently-selected row in the active tab. Enter-key
    /// dispatch: agents → focus the agent's pane, sessions → focus the
    /// tmux session, remote → SSH-attach to the picked session.
    fn activate_selected(&mut self) {
        if self.state.active_tab == Tab::Sessions {
            self.focus_tmux_session();
            self.state.tmux_expanded = false;
        } else if self.state.active_tab == Tab::Remote {
            self.attach_remote_session();
        } else {
            self.focus_selected();
        }
    }

    /// Move the cursor in the currently-active tab. `delta` is `+1` for
    /// "down/next" and `-1` for "up/prev". Sessions tab also handles
    /// the within-session window cursor when expanded.
    fn navigate_by_tab(&mut self, delta: isize) {
        match self.state.active_tab {
            Tab::Sessions => {
                if self.state.tmux_expanded {
                    self.state.navigate_tmux_window(delta);
                } else {
                    self.state.navigate_tmux_session(delta);
                }
            }
            Tab::Remote => self.state.navigate_remote(delta),
            _ => {
                if delta >= 0 {
                    self.state.select_next();
                } else {
                    self.state.select_prev();
                }
                if self.state.show_detail { self.refresh_selected_transcript(); }
            }
        }
    }

    /// If a modal is currently open, route the key to its handler and
    /// return `Some(should_quit)`. Returns `None` when no modal claims
    /// the key, so the caller can fall through to the global handler.
    /// Modals are mutually exclusive in practice; the order below
    /// reflects that — the first match wins.
    fn dispatch_modal_key(&mut self, code: KeyCode, ctrl: bool) -> Option<bool> {
        if self.state.bookmark.mode {
            if let Some(note) = self.state.apply_bookmark_key(code, ctrl)
                && let Some(cwd) = self.state.bookmark.cwd.take() {
                    if note.is_empty() {
                        self.state.bookmarks.remove(&cwd);
                    } else {
                        self.state.bookmarks.insert(cwd, note);
                    }
                    crate::state::save_bookmarks(&self.state.bookmarks);
                }
            return Some(false);
        }
        if self.state.new_agent.mode {
            if let Some((prompt, cwd)) = self.state.apply_new_agent_key(code, ctrl) {
                self.spawn_new_agent(&cwd, &prompt);
            }
            return Some(false);
        }
        if self.state.pr_picker.mode {
            if let Some(submit) = self.state.apply_pr_picker_key(code, ctrl)
                && !submit.cwd.is_empty()
            {
                self.spawn_pr_by_number(&submit.cwd, submit.number, &submit.title);
            }
            return Some(false);
        }
        if self.state.worktree_picker.mode {
            if let Some(submit) = self.state.apply_worktree_picker_key(code, ctrl)
                && !submit.path.is_empty()
            {
                self.spawn_resume_worktree(&submit.path);
            }
            return Some(false);
        }
        if self.state.worktree.mode {
            if let Some(branch) = self.state.apply_worktree_key(code, ctrl) {
                let cwd = self.state.worktree.cwd.take().unwrap_or_default();
                if !cwd.is_empty() {
                    self.spawn_worktree(&cwd, &branch);
                }
            }
            return Some(false);
        }
        if self.state.search_mode {
            if self.state.apply_search_key(code, ctrl) == KeyOutcome::Quit {
                return Some(true);
            }
            return Some(false);
        }
        if self.state.show_help {
            match code {
                KeyCode::Esc
                | KeyCode::Char('?')
                | KeyCode::Char('h')
                | KeyCode::Char('q') => self.state.show_help = false,
                _ => {}
            }
            return Some(false);
        }
        if self.state.chat_view.is_some() {
            // Ctrl-C still quits even with chat open.
            if ctrl && matches!(code, KeyCode::Char('c')) {
                return Some(true);
            }
            self.apply_chat_view_key(code);
            return Some(false);
        }
        None
    }

    /// Global key handler: runs when no modal is open. The big match
    /// of the application's main keymap.
    fn dispatch_normal_key(&mut self, code: KeyCode, ctrl: bool) -> Result<bool> {
        match code {
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
            KeyCode::Char('c') if self.state.active_tab == Tab::Agents => {
                self.open_chat_for_selected();
            }
            KeyCode::Char('j') | KeyCode::Down => self.navigate_by_tab(1),
            KeyCode::Char('k') | KeyCode::Up => self.navigate_by_tab(-1),
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
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('b') if self.state.active_tab == Tab::Agents => {
                if let Some(session) = self.state.selected_session() {
                    let cwd = session.cwd.clone();
                    self.state.bookmark.input = self.state.bookmarks
                        .get(&cwd)
                        .cloned()
                        .unwrap_or_default();
                    self.state.bookmark.cwd = Some(cwd);
                    self.state.bookmark.mode = true;
                }
            }
            KeyCode::Char('g') if self.state.active_tab == Tab::Agents => {
                self.launch_worktree_prompt();
            }
            KeyCode::Char('p') if self.state.active_tab == Tab::Agents => {
                self.open_pr_picker();
            }
            KeyCode::Char('u') if self.state.active_tab == Tab::Agents => {
                self.open_worktree_picker();
            }
            KeyCode::Char('o') if self.state.active_tab == Tab::Agents => {
                self.open_selected_pr();
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
        self.state.worktree.cwd = Some(cwd);
        self.state.worktree.input.clear();
        self.state.worktree.mode = true;
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

    /// Open the worktree picker: mark it loading and kick off a background
    /// `wt list --format json` for the selected agent's repo. The result
    /// comes back through `Event::WorktreePickerLoaded`. The picker lists the
    /// linked worktrees of that repo so the user can resume Claude in one of
    /// them (`claude --continue`), regardless of whether it still has an
    /// agent card.
    fn open_worktree_picker(&mut self) {
        let cwd = self
            .state
            .selected_session()
            .filter(|s| s.host.is_none())
            .map(|s| s.cwd.clone())
            .or_else(|| self.state.sessions.iter().find(|s| s.host.is_none()).map(|s| s.cwd.clone()));
        let Some(cwd) = cwd else {
            tmux::display_message("worktree: no local agent to anchor the repo");
            return;
        };
        if crate::worktree::git_root(&cwd).is_none() {
            tmux::display_message("worktree: not a git repository");
            return;
        }
        self.state.worktree_picker.mode = true;
        self.state.worktree_picker.loading = true;
        self.state.worktree_picker.error = None;
        self.state.worktree_picker.items.clear();
        self.state.worktree_picker.selected = 0;
        self.state.worktree_picker.query.clear();
        self.state.worktree_picker.cwd = Some(cwd.clone());

        let Some(ref tx) = self.scan_tx else { return };
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::worktree::list_worktrees(&cwd);
            let _ = tx.send(Event::WorktreePickerLoaded { cwd, result });
        });
    }

    /// Resume Claude in `path` (a worktree directory) in a background thread.
    /// Used by the worktree picker when the user confirms a row with Enter.
    fn spawn_resume_worktree(&self, path: &str) {
        let path = path.to_string();
        std::thread::spawn(move || {
            if let Err(e) = crate::worktree::resume(&path) {
                tmux::display_message(&format!("resume: {e}"));
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
        self.state.pr_picker.mode = true;
        self.state.pr_picker.loading = true;
        self.state.pr_picker.error = None;
        self.state.pr_picker.prs.clear();
        self.state.pr_picker.selected = 0;
        self.state.pr_picker.query.clear();
        self.state.pr_picker.cwd = Some(cwd.clone());

        let Some(ref tx) = self.scan_tx else { return };
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::worktree::list_open_prs(&cwd);
            let _ = tx.send(Event::PrPickerLoaded { cwd, result });
        });
    }

    /// Open the GitHub PR for the selected agent's branch in the user's
    /// browser via `gh pr view <num> --web`. The number comes from the
    /// background-refreshed cache (the same data that powers the `#NNNN`
    /// badge), so this is a no-op when the badge is absent — we surface
    /// a brief tmux message instead of silently swallowing the keypress.
    fn open_selected_pr(&mut self) {
        let Some(session) = self.state.selected_session() else { return };
        let cwd = session.cwd.clone();
        let pr = self.state.pr_info_for(
            session.repo_root.as_deref(),
            session.branch.as_deref(),
        );
        let Some(info) = pr else {
            tmux::display_message("no PR for this branch");
            return;
        };
        let number = info.number;
        if !crate::worktree::has_gh() {
            tmux::display_message("`gh` CLI not found");
            return;
        }
        std::thread::spawn(move || {
            let status = std::process::Command::new("gh")
                .args(["pr", "view", &number.to_string(), "--web"])
                .current_dir(&cwd)
                .status();
            if let Err(e) = status {
                tmux::display_message(&format!("gh pr view: {e}"));
            }
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
            self.state.cache_pane_for_session(&session_id, pane);
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
        let result = crate::state::hook_event_to_status("SessionStart", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::Idle)));
    }

    #[test]
    fn hook_user_prompt_submit_captures_prompt() {
        let mut s = mk_session();
        let p = HookPayload {
            prompt: Some("  hello world  ".into()),
            ..empty_payload()
        };
        let result = crate::state::hook_event_to_status("UserPromptSubmit", &p, &mut s);
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
        crate::state::hook_event_to_status("UserPromptSubmit", &p, &mut s);
        assert_eq!(s.last_prompt.as_deref(), Some("keep"));
    }

    #[test]
    fn hook_pre_tool_use_stores_tool() {
        let mut s = mk_session();
        let p = HookPayload {
            tool_name: Some("Bash".into()),
            ..empty_payload()
        };
        let result = crate::state::hook_event_to_status("PreToolUse", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::RunningTool(ref t)) if t == "Bash"));
        assert_eq!(s.last_tool.as_deref(), Some("Bash"));
    }

    #[test]
    fn hook_pre_tool_use_defaults_to_question_mark() {
        let mut s = mk_session();
        let p = empty_payload();
        let result = crate::state::hook_event_to_status("PreToolUse", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::RunningTool(ref t)) if t == "?"));
    }

    #[test]
    fn hook_post_tool_use_returns_running() {
        let mut s = mk_session();
        let result = crate::state::hook_event_to_status("PostToolUse", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Running)));
    }

    #[test]
    fn hook_session_end_marks_completed() {
        let mut s = mk_session();
        let result = crate::state::hook_event_to_status("SessionEnd", &empty_payload(), &mut s);
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
        let result = crate::state::hook_event_to_status("Notification", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::WaitingForUser(ref m)) if m == "allow edit?"));
    }

    #[test]
    fn hook_notification_other_type() {
        let mut s = mk_session();
        let p = HookPayload {
            notification_type: Some("info".into()),
            ..empty_payload()
        };
        let result = crate::state::hook_event_to_status("Notification", &p, &mut s);
        assert!(matches!(result, Some(SessionStatus::WaitingForInput)));
    }

    #[test]
    fn hook_unknown_event_returns_none() {
        let mut s = mk_session();
        let result = crate::state::hook_event_to_status("SomeNewEvent", &empty_payload(), &mut s);
        assert!(result.is_none());
    }

    #[test]
    fn hook_stop_returns_idle() {
        let mut s = mk_session();
        // Stop and SubagentStop both return Idle (transcript read will fail in test, that's ok)
        let result = crate::state::hook_event_to_status("Stop", &empty_payload(), &mut s);
        assert!(matches!(result, Some(SessionStatus::Idle)));
    }

    #[test]
    fn hook_subagent_stop_returns_idle() {
        let mut s = mk_session();
        let result = crate::state::hook_event_to_status("SubagentStop", &empty_payload(), &mut s);
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
