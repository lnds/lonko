use std::collections::HashMap;
use std::time::Instant;

use crate::sources::transcript::TranscriptInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Unknown,
    Idle,
    Running,
    RunningTool(String),
    WaitingForInput,          // session finished its turn, awaiting next prompt
    WaitingForUser(String),   // needs permission (urgent)
    Completed,
}

impl SessionStatus {
    pub fn glyph(&self) -> &'static str {
        match self {
            SessionStatus::Unknown => "?",
            SessionStatus::Idle => "◌",
            SessionStatus::Running | SessionStatus::RunningTool(_) => "◉",
            SessionStatus::WaitingForInput => "◐",
            SessionStatus::WaitingForUser(_) => "⚠",
            SessionStatus::Completed => "✓",
        }
    }

    pub fn label(&self) -> String {
        match self {
            SessionStatus::Unknown => "unknown".into(),
            SessionStatus::Idle => "idle".into(),
            SessionStatus::Running => "running".into(),
            SessionStatus::RunningTool(tool) => format!("⚙ {}", tool),
            SessionStatus::WaitingForInput => "waiting for input".into(),
            SessionStatus::WaitingForUser(msg) => {
                let truncated = if msg.chars().count() > 40 {
                    let s: String = msg.chars().take(40).collect();
                    format!("{s}…")
                } else {
                    msg.clone()
                };
                format!("▲ {}", truncated)
            }
            SessionStatus::Completed => "done".into(),
        }
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self, SessionStatus::WaitingForUser(_))
    }

    pub fn is_waiting_input(&self) -> bool {
        matches!(self, SessionStatus::WaitingForInput)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            SessionStatus::Running | SessionStatus::RunningTool(_)
        )
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub cwd: String,
    pub project_name: String,
    pub branch: Option<String>,
    pub status: SessionStatus,
    pub model: Option<String>,
    pub cost_usd: f64,
    pub context_used: u32,
    pub context_max: u32,
    pub last_activity: Instant,
    pub completed_at: Option<Instant>,
    pub tmux_pane: Option<String>,
    pub transcript_path: Option<String>,
    pub last_prompt: Option<String>,
    pub last_tool: Option<String>,
    pub parent_id: Option<String>,
    pub depth: u8,
    /// Canonical repo path shared across all worktrees of the same git repo.
    /// Populated by callers via `worktree::repo_common_root` — left `None`
    /// for non-git cwds. Used to group the agents list in the UI.
    pub repo_root: Option<String>,
}

/// Trunk branches that should float to the top of their repo group in the
/// agents list. Covers the two canonical git default branch names.
fn is_trunk_branch(branch: Option<&str>) -> bool {
    matches!(branch, Some("main") | Some("master"))
}

/// Return the context window size for a given model ID.
/// Falls back to 200_000 for unknown models.
pub fn context_max_for_model(model: &str) -> u32 {
    let m = model.to_lowercase();
    if m.contains("opus") {
        1_000_000
    } else {
        200_000
    }
}

impl Session {
    pub fn new(id: String, pid: u32, cwd: String) -> Self {
        let project_name = cwd
            .split('/')
            .next_back()
            .unwrap_or(&cwd)
            .to_string();

        Self {
            id,
            pid,
            cwd,
            project_name,
            branch: None,
            status: SessionStatus::Unknown,
            model: None,
            cost_usd: 0.0,
            context_used: 0,
            context_max: 200_000,
            last_activity: Instant::now(),
            completed_at: None,
            tmux_pane: None,
            transcript_path: None,
            last_prompt: None,
            last_tool: None,
            parent_id: None,
            depth: 0,
            repo_root: None,
        }
    }

    pub fn elapsed_label(&self) -> String {
        let secs = self.last_activity.elapsed().as_secs();
        if secs < 10 {
            "now".into()
        } else if secs < 60 {
            format!("{}s ago", secs)
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    }

    /// Merge transcript-derived fields into this session.
    /// Branch is updated only if present in `info` — callers that prefer the
    /// live git branch should set `info.branch` before calling this.
    pub fn apply_transcript_info(&mut self, info: TranscriptInfo) {
        if let Some(m) = info.model {
            self.context_max = context_max_for_model(&m);
            self.model = Some(m);
        }
        if info.last_prompt.is_some()  { self.last_prompt  = info.last_prompt; }
        if info.last_tool.is_some()    { self.last_tool    = info.last_tool; }
        if info.context_tokens > 0     { self.context_used = info.context_tokens as u32; }
        if info.branch.is_some()       { self.branch       = info.branch; }
    }

    pub fn context_pct(&self) -> f64 {
        if self.context_max == 0 {
            0.0
        } else {
            self.context_used as f64 / self.context_max as f64
        }
    }

    pub fn is_subagent(&self) -> bool {
        self.parent_id.is_some()
    }

    /// Human-readable label for this session's group (rendered in the
    /// agents list above a clustered repo). Uses the basename of
    /// `repo_root` when available, falling back to the cwd basename.
    pub fn group_label(&self) -> String {
        let src = self.repo_root.as_deref().unwrap_or(self.cwd.as_str());
        std::path::Path::new(src)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ungrouped".to_string())
    }
}

// ── Bookmark persistence ──────────────────────────────────────────────────────

fn bookmarks_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("lonko-bookmarks.json")
}

pub fn load_bookmarks() -> HashMap<String, String> {
    std::fs::read_to_string(bookmarks_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_bookmarks(bookmarks: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(bookmarks) {
        let _ = std::fs::write(bookmarks_path(), json);
    }
}

#[derive(Debug)]
pub struct AppState {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub tick: u64,
    pub active_tab: Tab,
    pub show_detail: bool,
    pub search_query: String,
    pub search_mode: bool,
    pub term_height: u16,
    pub term_width: u16,
    /// Pane ID to auto-select on startup (consumed once matched).
    pub focus_pane: Option<String>,
    pub focused: bool,
    /// Session ID of the currently active toki session (where the tmux cursor is).
    pub focused_session_id: Option<String>,
    /// Own tmux pane ID (from TMUX_PANE env var) — used to follow the user between windows.
    pub own_pane: Option<String>,
    /// Tmux sessions (local) for the Sessions tab.
    pub tmux_sessions: Vec<TmuxSession>,
    /// Selected index in the Sessions tab.
    pub tmux_selected: usize,
    /// Cursor within the selected session's window list (None = session-level navigation).
    pub tmux_window_cursor: Option<usize>,
    /// Whether the selected session card is expanded (showing the window list).
    pub tmux_expanded: bool,
    /// Worktree creation mode: user is typing a branch name.
    pub worktree_mode: bool,
    /// Branch name input for worktree creation.
    pub worktree_input: String,
    /// The cwd to create the worktree from (set when entering worktree mode).
    pub worktree_cwd: Option<String>,
    /// Bookmarks: cwd → note. Persisted to disk.
    pub bookmarks: HashMap<String, String>,
    /// Bookmark note input mode.
    pub bookmark_mode: bool,
    /// Bookmark note text being edited.
    pub bookmark_input: String,
    /// Whether the help popup is visible.
    pub show_help: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Continue,
    Quit,
}

// ── Tmux Sessions ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SessionOrigin {
    Local,
    Remote { host: String },
}

impl SessionOrigin {
    pub fn host_label(&self) -> &str {
        match self {
            SessionOrigin::Local => "local",
            SessionOrigin::Remote { host } => host.as_str(),
        }
    }
    pub fn is_remote(&self) -> bool {
        matches!(self, SessionOrigin::Remote { .. })
    }
}

#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub pane_count: u32,
}

#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub origin: SessionOrigin,
    pub windows: Vec<TmuxWindow>,
    pub last_activity_secs: u64,
    pub attached: bool,
    pub has_claude: bool,
}

impl TmuxSession {
    pub fn active_window(&self) -> Option<&TmuxWindow> {
        self.windows.iter().find(|w| w.active)
    }

    pub fn age_label(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let secs = now.saturating_sub(self.last_activity_secs);
        if secs < 60 { "now".into() }
        else if secs < 3600 { format!("{}m ago", secs / 60) }
        else if secs < 86400 { format!("{}h ago", secs / 3600) }
        else { format!("{}d ago", secs / 86400) }
    }
}

// ── Tabs ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Clone)]
pub enum Tab {
    #[default]
    Agents,
    Sessions,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sessions: vec![],
            selected: 0,
            tick: 0,
            active_tab: Tab::Agents,
            show_detail: false,
            search_query: String::new(),
            search_mode: false,
            term_height: 0,
            term_width: 0,
            focus_pane: None,
            focused: false,
            focused_session_id: None,
            own_pane: None,
            tmux_sessions: vec![],
            tmux_selected: 0,
            tmux_window_cursor: None,
            tmux_expanded: false,
            worktree_mode: false,
            worktree_input: String::new(),
            worktree_cwd: None,
            bookmarks: HashMap::new(),
            bookmark_mode: false,
            bookmark_input: String::new(),
            show_help: false,
        }
    }
}

impl AppState {
    /// Return sessions organized for rendering: main agents are clustered
    /// by `repo_root` (so worktrees of the same repo sit together), and
    /// each main is followed by its subagents. Groups appear in the order
    /// their first member was inserted; mains without a `repo_root` form a
    /// trailing "ungrouped" bucket. Orphaned subagents land at the end.
    pub fn visible_sessions(&self) -> Vec<&Session> {
        let filtered: Vec<&Session> = if self.search_query.is_empty() {
            self.sessions.iter().collect()
        } else {
            let q = self.search_query.to_lowercase();
            self.sessions
                .iter()
                .filter(|s| {
                    s.project_name.to_lowercase().contains(&q)
                        || s.parent_id.as_ref().is_some_and(|pid| {
                            self.sessions.iter().any(|p| {
                                p.id == *pid && p.project_name.to_lowercase().contains(&q)
                            })
                        })
                })
                .collect()
        };

        Self::sort_sessions(filtered)
    }

    /// All sessions in canonical display order (grouped by repo, trunk first)
    /// without any search filter applied. Used by `write_sessions_cache` so
    /// that `lonko focus N` numbers always match the UI ordering.
    pub fn ordered_sessions(&self) -> Vec<&Session> {
        Self::sort_sessions(self.sessions.iter().collect())
    }

    /// Sort sessions: group by repo root, float trunk branches to the top
    /// of each group, and nest subagents under their parent.
    fn sort_sessions(sessions: Vec<&Session>) -> Vec<&Session> {
        let mains: Vec<&Session> = sessions.iter().copied().filter(|s| s.depth == 0).collect();
        let subs: Vec<&Session> = sessions.iter().copied().filter(|s| s.depth > 0).collect();

        // Linear-scan grouping: preserves first-seen order of keys, and the
        // insertion order of mains within each key. `None` (non-git mains,
        // rare in practice because app.rs falls back to cwd) always lands
        // in the tail bucket so named repos stay on top.
        let mut named: Vec<(Option<&str>, Vec<&Session>)> = Vec::new();
        let mut ungrouped: Vec<&Session> = Vec::new();
        for m in &mains {
            match m.repo_root.as_deref() {
                None => ungrouped.push(*m),
                Some(key) => {
                    if let Some(entry) = named.iter_mut().find(|(k, _)| *k == Some(key)) {
                        entry.1.push(*m);
                    } else {
                        named.push((Some(key), vec![*m]));
                    }
                }
            }
        }
        if !ungrouped.is_empty() {
            named.push((None, ungrouped));
        }

        // Within each group, float trunk branches (`main`, `master`) to the
        // top so the canonical checkout sits above worktree branches.
        // Stable sort preserves insertion order between ties.
        for (_, group_mains) in named.iter_mut() {
            group_mains.sort_by_key(|s| if is_trunk_branch(s.branch.as_deref()) { 0 } else { 1 });
        }

        let mut result: Vec<&Session> = Vec::with_capacity(sessions.len());
        for (_, group_mains) in &named {
            for main in group_mains {
                result.push(main);
                let mut children: Vec<&Session> = subs
                    .iter()
                    .copied()
                    .filter(|s| s.parent_id.as_deref() == Some(main.id.as_str()))
                    .collect();
                // Most recent subagent first.
                children.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
                result.extend(children);
            }
        }

        // Orphaned subagents (parent not in the list)
        for sub in &subs {
            if !result.iter().any(|s| std::ptr::eq(*s, *sub)) {
                result.push(sub);
            }
        }

        result
    }

    pub fn visible_len(&self) -> usize {
        self.visible_sessions().len()
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.visible_sessions().into_iter().nth(self.selected)
    }

    pub fn waiting_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status.is_waiting())
            .count()
    }

    pub fn has_waiting(&self) -> bool {
        self.sessions.iter().any(|s| s.status.is_waiting())
    }

    pub fn active_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status.is_active() || s.status.is_waiting())
            .count()
    }

    pub fn running_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status.is_active())
            .count()
    }

    pub fn select_next(&mut self) {
        let len = self.visible_len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    pub fn select_prev(&mut self) {
        let len = self.visible_len();
        if len > 0 {
            if self.selected == 0 {
                self.selected = len - 1;
            } else {
                self.selected -= 1;
            }
        }
    }

    /// Clamp `selected` to remain within `sessions` after a mutation.
    fn clamp_selected(&mut self) {
        if self.sessions.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.sessions.len() - 1);
        }
    }

    /// Remove a session by PID. Returns true if one was removed.
    pub fn remove_session_by_pid(&mut self, pid: u32) -> bool {
        if let Some(pos) = self.sessions.iter().position(|s| s.pid == pid) {
            self.sessions.remove(pos);
            self.clamp_selected();
            true
        } else {
            false
        }
    }

    /// Handle a tmux pane going away. Running/waiting/idle sessions are marked
    /// completed so they fade out; already-completed sessions are removed.
    pub fn handle_pane_gone(&mut self, pane_id: &str) {
        let Some(pos) = self.sessions.iter().position(|s| {
            s.tmux_pane.as_deref() == Some(pane_id)
        }) else { return };
        let session = &mut self.sessions[pos];
        match session.status {
            SessionStatus::Running
            | SessionStatus::RunningTool(_)
            | SessionStatus::WaitingForUser(_)
            | SessionStatus::WaitingForInput
            | SessionStatus::Idle => {
                session.completed_at = Some(Instant::now());
                session.status = SessionStatus::Completed;
            }
            _ => {
                self.sessions.remove(pos);
                self.clamp_selected();
            }
        }
    }

    /// Drop sessions that have been completed for longer than `ttl_secs`.
    pub fn prune_completed(&mut self, ttl_secs: u64) {
        self.sessions.retain(|s| {
            s.completed_at
                .map(|t| t.elapsed().as_secs() < ttl_secs)
                .unwrap_or(true)
        });
        self.clamp_selected();
    }

    /// Apply a key to search mode. Returns `Quit` if Ctrl-C was pressed.
    pub fn apply_search_key(&mut self, code: crossterm::event::KeyCode, ctrl: bool) -> KeyOutcome {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_query.clear();
                self.selected = 0;
                self.tmux_selected = 0;
                self.tmux_window_cursor = None;
                self.tmux_expanded = false;
            }
            KeyCode::Enter => {
                self.search_mode = false;
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.selected = 0;
                self.tmux_selected = 0;
                self.tmux_window_cursor = None;
                self.tmux_expanded = false;
            }
            KeyCode::Char('c') if ctrl => return KeyOutcome::Quit,
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.selected = 0;
                self.tmux_selected = 0;
                self.tmux_window_cursor = None;
                self.tmux_expanded = false;
            }
            _ => {}
        }
        KeyOutcome::Continue
    }

    /// Apply a key to worktree mode. Returns the branch name on Enter, or None.
    pub fn apply_worktree_key(&mut self, code: crossterm::event::KeyCode, ctrl: bool) -> Option<String> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.worktree_mode = false;
                self.worktree_input.clear();
                self.worktree_cwd = None;
            }
            KeyCode::Enter => {
                let branch = self.worktree_input.trim().to_string();
                self.worktree_mode = false;
                self.worktree_input.clear();
                if !branch.is_empty() {
                    return Some(branch);
                }
                self.worktree_cwd = None;
            }
            KeyCode::Backspace => { self.worktree_input.pop(); }
            KeyCode::Char('c') if ctrl => {
                self.worktree_mode = false;
                self.worktree_input.clear();
                self.worktree_cwd = None;
            }
            KeyCode::Char(c) => { self.worktree_input.push(c); }
            _ => {}
        }
        None
    }

    /// Apply a key to bookmark mode. Returns `Some(note)` on Enter.
    /// An empty note signals "remove bookmark".
    pub fn apply_bookmark_key(&mut self, code: crossterm::event::KeyCode, ctrl: bool) -> Option<String> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.bookmark_mode = false;
                self.bookmark_input.clear();
            }
            KeyCode::Enter => {
                let note = self.bookmark_input.trim().to_string();
                self.bookmark_mode = false;
                self.bookmark_input.clear();
                return Some(note);
            }
            KeyCode::Backspace => { self.bookmark_input.pop(); }
            KeyCode::Char('c') if ctrl => {
                self.bookmark_mode = false;
                self.bookmark_input.clear();
            }
            KeyCode::Char(c) => { self.bookmark_input.push(c); }
            _ => {}
        }
        None
    }

    /// Return tmux sessions filtered by the current search query.
    /// Matches the session name and any window name (case-insensitive substring).
    pub fn visible_tmux_sessions(&self) -> Vec<&TmuxSession> {
        if self.search_query.is_empty() {
            return self.tmux_sessions.iter().collect();
        }
        let q = self.search_query.to_lowercase();
        self.tmux_sessions
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q)
                    || s.windows.iter().any(|w| w.name.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// The currently selected tmux session (respecting the search filter).
    /// Returns `None` when the filter hides everything or the list is empty.
    pub fn selected_tmux_session(&self) -> Option<&TmuxSession> {
        self.visible_tmux_sessions().get(self.tmux_selected).copied()
    }

    /// Navigate the tmux session list by `delta` (+1 = down, -1 = up).
    pub fn navigate_tmux_session(&mut self, delta: isize) {
        let max = self.visible_tmux_sessions().len().saturating_sub(1);
        if delta > 0 {
            self.tmux_selected = (self.tmux_selected + 1).min(max);
        } else {
            self.tmux_selected = self.tmux_selected.saturating_sub(1);
        }
        self.tmux_window_cursor = None;
    }

    /// Navigate the window cursor within the currently expanded tmux session by `delta`.
    pub fn navigate_tmux_window(&mut self, delta: isize) {
        let visible = self.visible_tmux_sessions();
        let n = visible
            .get(self.tmux_selected)
            .map(|s| s.windows.len())
            .unwrap_or(0);
        if n == 0 { return; }
        let cur = self.tmux_window_cursor.unwrap_or_else(|| {
            visible[self.tmux_selected]
                .windows.iter().position(|w| w.active).unwrap_or(0)
        });
        self.tmux_window_cursor = Some(if delta > 0 {
            (cur + 1) % n
        } else if cur == 0 {
            n - 1
        } else {
            cur - 1
        });
    }

    /// Resolve or create a session for an incoming hook event.
    /// Tries to promote a provisional tmux-scan session, evicts stale pane entries,
    /// or creates a new session. Returns true if the session now exists.
    pub fn resolve_hook_session(
        &mut self,
        session_id: &str,
        hook_pane: Option<&str>,
        hook_cwd: Option<&str>,
        transcript_path: Option<&str>,
        git_branch: Option<String>,
    ) -> bool {
        if self.sessions.iter().any(|s| s.id == session_id) {
            return true;
        }

        // Try to promote a provisional session discovered by tmux scanner.
        let promoted = if let Some(pane) = hook_pane {
            self.sessions.iter_mut().find(|s| {
                s.tmux_pane.as_deref() == Some(pane) && s.id.starts_with("tmux:")
            })
        } else {
            None
        };

        if let Some(s) = promoted {
            s.id = session_id.to_string();
        } else {
            // Evict any stale session for the same pane.
            if let Some(pane) = hook_pane {
                self.sessions.retain(|s| {
                    s.tmux_pane.as_deref() != Some(pane) || s.id == session_id
                });
            }
            // Create a new session entry.
            let cwd = hook_cwd.unwrap_or_default().to_string();
            if cwd.is_empty() {
                return false;
            }
            let mut session = Session::new(session_id.to_string(), 0, cwd);
            session.status = SessionStatus::Idle;
            if let Some(pane) = hook_pane {
                session.tmux_pane = Some(pane.to_string());
            }
            if let Some(tp) = transcript_path {
                if !tp.is_empty() { session.transcript_path = Some(tp.to_string()); }
            }
            session.branch = git_branch;
            self.sessions.push(session);
        }
        true
    }

    /// If focused_session_id is None, check if the last session matches the active pane.
    /// `active_pane` and `own_pane` are passed in to avoid tmux calls inside state logic.
    pub fn try_focus_active_pane(&mut self, active_pane: Option<&str>) {
        if self.focused_session_id.is_some() { return; }
        let Some(active) = active_pane else { return; };
        let is_own = self.own_pane.as_deref() == Some(active);
        if is_own { return; }

        let last = self.sessions.last();
        if let Some(s) = last {
            if s.tmux_pane.as_deref() == Some(active) {
                self.focused_session_id = Some(s.id.clone());
            }
        }
    }

    /// Auto-select the last session if its pane matches the focus_pane hint.
    /// Consumes `focus_pane` on match. `resolved_pane` is the pane for the last session
    /// (may differ from stored tmux_pane if discovered via pid lookup).
    pub fn try_apply_focus_hint(&mut self, resolved_pane: Option<&str>) {
        let target = match &self.focus_pane {
            Some(t) => t.clone(),
            None => return,
        };
        let Some(resolved) = resolved_pane else { return; };
        if resolved != target { return; }

        let idx = self.sessions.len().saturating_sub(1);
        if let Some(s) = self.sessions.get_mut(idx) {
            s.tmux_pane = Some(resolved.to_string());
        }
        self.selected = idx;
        self.focus_pane = None;
        self.focused_session_id = self.sessions.get(idx).map(|s| s.id.clone());
    }

    pub fn toggle_tab(&mut self) {
        self.active_tab = match self.active_tab {
            Tab::Agents => Tab::Sessions,
            Tab::Sessions => Tab::Agents,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_session() -> Session {
        Session::new("s1".into(), 100, "/tmp/proj".into())
    }

    fn mk_info() -> TranscriptInfo {
        TranscriptInfo {
            model: None,
            branch: None,
            last_prompt: None,
            last_tool: None,
            context_tokens: 0,
        }
    }

    #[test]
    fn apply_transcript_info_merges_present_fields() {
        let mut s = mk_session();
        s.model = Some("old".into());
        s.last_prompt = Some("old prompt".into());
        let info = TranscriptInfo {
            model: Some("claude-opus-4-6".into()),
            branch: Some("feature".into()),
            last_prompt: Some("new prompt".into()),
            last_tool: Some("Bash".into()),
            context_tokens: 1_234,
        };
        s.apply_transcript_info(info);
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(s.context_max, 1_000_000); // opus → 1M
        assert_eq!(s.branch.as_deref(), Some("feature"));
        assert_eq!(s.last_prompt.as_deref(), Some("new prompt"));
        assert_eq!(s.last_tool.as_deref(), Some("Bash"));
        assert_eq!(s.context_used, 1_234);
    }

    #[test]
    fn apply_transcript_info_preserves_existing_when_none() {
        let mut s = mk_session();
        s.model = Some("keep".into());
        s.last_prompt = Some("keep prompt".into());
        s.branch = Some("keep-branch".into());
        s.context_used = 999;
        s.apply_transcript_info(mk_info());
        assert_eq!(s.model.as_deref(), Some("keep"));
        assert_eq!(s.last_prompt.as_deref(), Some("keep prompt"));
        assert_eq!(s.branch.as_deref(), Some("keep-branch"));
        assert_eq!(s.context_used, 999);
    }

    #[test]
    fn apply_transcript_info_zero_tokens_does_not_overwrite() {
        let mut s = mk_session();
        s.context_used = 500;
        s.apply_transcript_info(mk_info());
        assert_eq!(s.context_used, 500);
    }

    fn session_with(id: &str, pid: u32, pane: Option<&str>) -> Session {
        let mut s = Session::new(id.into(), pid, "/tmp".into());
        s.tmux_pane = pane.map(String::from);
        s
    }

    fn main_with_repo(id: &str, repo: Option<&str>) -> Session {
        let mut s = Session::new(id.into(), 0, format!("/tmp/{id}"));
        s.repo_root = repo.map(String::from);
        s
    }

    fn main_with_repo_branch(id: &str, repo: &str, branch: &str) -> Session {
        let mut s = main_with_repo(id, Some(repo));
        s.branch = Some(branch.into());
        s
    }

    #[test]
    fn visible_sessions_floats_trunk_branch_to_top_of_group() {
        let mut state = AppState::default();
        // Two worktrees of the same repo: a feature branch inserted first,
        // then the main-branch checkout. Despite insertion order, the main
        // branch should render first within the group.
        state.sessions = vec![
            main_with_repo_branch("feat", "/r/alpha", "lonko-6"),
            main_with_repo_branch("trunk", "/r/alpha", "main"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["trunk", "feat"]);
    }

    #[test]
    fn visible_sessions_trunk_sort_is_stable_for_ties() {
        let mut state = AppState::default();
        state.sessions = vec![
            main_with_repo_branch("feat1", "/r/alpha", "feat-a"),
            main_with_repo_branch("feat2", "/r/alpha", "feat-b"),
            main_with_repo_branch("feat3", "/r/alpha", "feat-c"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        // No trunk present: insertion order preserved.
        assert_eq!(ids, vec!["feat1", "feat2", "feat3"]);
    }

    #[test]
    fn visible_sessions_master_also_counts_as_trunk() {
        let mut state = AppState::default();
        state.sessions = vec![
            main_with_repo_branch("feat", "/r/alpha", "wip"),
            main_with_repo_branch("trunk", "/r/alpha", "master"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["trunk", "feat"]);
    }

    #[test]
    fn visible_sessions_clusters_by_repo_root() {
        let mut state = AppState::default();
        // Interleaved insertion order: A1, B1, A2, B2. After grouping the
        // two A-repo mains should sit together, followed by the two B-repo
        // mains — each preserving their relative insertion order.
        state.sessions = vec![
            main_with_repo("a1", Some("/r/alpha")),
            main_with_repo("b1", Some("/r/beta")),
            main_with_repo("a2", Some("/r/alpha")),
            main_with_repo("b2", Some("/r/beta")),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a1", "a2", "b1", "b2"]);
    }

    #[test]
    fn visible_sessions_ungrouped_bucket_goes_last() {
        let mut state = AppState::default();
        // `None` mains are inserted before a grouped main, but should drop
        // to the end so named groups stay on top.
        state.sessions = vec![
            main_with_repo("n1", None),
            main_with_repo("a1", Some("/r/alpha")),
            main_with_repo("n2", None),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a1", "n1", "n2"]);
    }

    #[test]
    fn visible_sessions_subagents_stay_under_main_across_groups() {
        let mut state = AppState::default();
        let mut a1 = main_with_repo("a1", Some("/r/alpha"));
        a1.last_activity = Instant::now();
        let mut b1 = main_with_repo("b1", Some("/r/beta"));
        b1.last_activity = Instant::now();
        let mut sub = Session::new("s1".into(), 0, "/tmp/a1".into());
        sub.parent_id = Some("a1".into());
        sub.depth = 1;
        sub.repo_root = Some("/r/alpha".into());
        // Insert in a scrambled order
        state.sessions = vec![b1, sub, a1];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        // Visible order of groups is first-seen: b1 was inserted before
        // a1, so beta comes first — but a1 still owns its subagent.
        assert_eq!(ids, vec!["b1", "a1", "s1"]);
    }

    #[test]
    fn ordered_sessions_matches_visible_without_search() {
        let mut state = AppState::default();
        state.sessions = vec![
            main_with_repo_branch("feat", "/r/alpha", "lonko-6"),
            main_with_repo_branch("trunk", "/r/alpha", "main"),
            main_with_repo("b1", Some("/r/beta")),
        ];
        let visible: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        let ordered: Vec<&str> = state.ordered_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(visible, ordered);
        // trunk floated to top of alpha group
        assert_eq!(ordered, vec!["trunk", "feat", "b1"]);
    }

    #[test]
    fn ordered_sessions_ignores_search_filter() {
        let mut state = AppState::default();
        state.sessions = vec![
            main_with_repo_branch("feat", "/r/alpha", "lonko-6"),
            main_with_repo_branch("trunk", "/r/alpha", "main"),
        ];
        state.search_query = "feat".into();
        // visible_sessions respects the filter
        let visible: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(visible, vec!["feat"]);
        // ordered_sessions ignores it
        let ordered: Vec<&str> = state.ordered_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ordered, vec!["trunk", "feat"]);
    }

    #[test]
    fn remove_session_by_pid_clamps_selection() {
        let mut state = AppState::default();
        state.sessions = vec![
            session_with("a", 1, None),
            session_with("b", 2, None),
            session_with("c", 3, None),
        ];
        state.selected = 2;
        assert!(state.remove_session_by_pid(3));
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.selected, 1); // clamped
    }

    #[test]
    fn remove_session_by_pid_missing_returns_false() {
        let mut state = AppState::default();
        state.sessions = vec![session_with("a", 1, None)];
        assert!(!state.remove_session_by_pid(999));
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn handle_pane_gone_marks_running_as_completed() {
        let mut state = AppState::default();
        let mut s = session_with("a", 1, Some("%1"));
        s.status = SessionStatus::Running;
        state.sessions.push(s);

        state.handle_pane_gone("%1");
        assert_eq!(state.sessions.len(), 1);
        assert!(matches!(state.sessions[0].status, SessionStatus::Completed));
        assert!(state.sessions[0].completed_at.is_some());
    }

    #[test]
    fn handle_pane_gone_removes_already_completed() {
        let mut state = AppState::default();
        let mut s = session_with("a", 1, Some("%1"));
        s.status = SessionStatus::Completed;
        state.sessions.push(s);
        state.sessions.push(session_with("b", 2, Some("%2")));

        state.handle_pane_gone("%1");
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "b");
    }

    #[test]
    fn handle_pane_gone_unknown_pane_is_noop() {
        let mut state = AppState::default();
        state.sessions.push(session_with("a", 1, Some("%1")));
        state.handle_pane_gone("%nope");
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn apply_search_key_typing_builds_query() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.search_mode = true;
        state.selected = 5;
        assert_eq!(state.apply_search_key(KeyCode::Char('s'), false), KeyOutcome::Continue);
        state.apply_search_key(KeyCode::Char('h'), false);
        assert_eq!(state.search_query, "sh");
        assert_eq!(state.selected, 0); // reset on each keystroke
    }

    #[test]
    fn apply_search_key_backspace_pops() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.search_mode = true;
        state.search_query = "abc".into();
        state.apply_search_key(KeyCode::Backspace, false);
        assert_eq!(state.search_query, "ab");
    }

    #[test]
    fn apply_search_key_esc_clears_and_exits() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.search_mode = true;
        state.search_query = "abc".into();
        state.apply_search_key(KeyCode::Esc, false);
        assert!(!state.search_mode);
        assert_eq!(state.search_query, "");
    }

    #[test]
    fn apply_search_key_enter_keeps_query_exits_mode() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.search_mode = true;
        state.search_query = "abc".into();
        state.apply_search_key(KeyCode::Enter, false);
        assert!(!state.search_mode);
        assert_eq!(state.search_query, "abc");
    }

    #[test]
    fn apply_search_key_ctrl_c_returns_quit() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.search_mode = true;
        assert_eq!(state.apply_search_key(KeyCode::Char('c'), true), KeyOutcome::Quit);
    }

    #[test]
    fn toggle_tab_cycles() {
        let mut state = AppState::default();
        assert_eq!(state.active_tab, Tab::Agents);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Sessions);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Agents);
    }

    #[test]
    fn context_max_for_model_defaults_to_200k() {
        assert_eq!(context_max_for_model("sonnet-4-6"), 200_000);
        assert_eq!(context_max_for_model("unknown"), 200_000);
        assert_eq!(context_max_for_model("claude-opus-4-6"), 1_000_000);
    }

    // ── navigate_tmux_session ──────────────────────────────────────────────

    fn mk_tmux_session(name: &str, n_windows: u32) -> TmuxSession {
        let windows: Vec<TmuxWindow> = (0..n_windows)
            .map(|i| TmuxWindow {
                index: i,
                name: format!("win{i}"),
                active: i == 0,
                pane_count: 1,
            })
            .collect();
        TmuxSession {
            name: name.into(),
            origin: SessionOrigin::Local,
            windows,
            last_activity_secs: 0,
            attached: false,
            has_claude: false,
        }
    }

    #[test]
    fn navigate_tmux_session_down() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("a", 2),
            mk_tmux_session("b", 2),
            mk_tmux_session("c", 2),
        ];
        state.tmux_selected = 0;
        state.navigate_tmux_session(1);
        assert_eq!(state.tmux_selected, 1);
        assert!(state.tmux_window_cursor.is_none());
    }

    #[test]
    fn navigate_tmux_session_clamps_at_end() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 1), mk_tmux_session("b", 1)];
        state.tmux_selected = 1;
        state.navigate_tmux_session(1);
        assert_eq!(state.tmux_selected, 1); // stays at max
    }

    #[test]
    fn navigate_tmux_session_up() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 1), mk_tmux_session("b", 1)];
        state.tmux_selected = 1;
        state.navigate_tmux_session(-1);
        assert_eq!(state.tmux_selected, 0);
    }

    #[test]
    fn navigate_tmux_session_clamps_at_start() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 1)];
        state.tmux_selected = 0;
        state.navigate_tmux_session(-1);
        assert_eq!(state.tmux_selected, 0);
    }

    #[test]
    fn navigate_tmux_session_clears_window_cursor() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 3), mk_tmux_session("b", 2)];
        state.tmux_selected = 0;
        state.tmux_window_cursor = Some(2);
        state.navigate_tmux_session(1);
        assert!(state.tmux_window_cursor.is_none());
    }

    // ── navigate_tmux_window ───────────────────────────────────────────────

    #[test]
    fn navigate_tmux_window_forward_wraps() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 3)];
        state.tmux_selected = 0;
        state.tmux_window_cursor = Some(2);
        state.navigate_tmux_window(1);
        assert_eq!(state.tmux_window_cursor, Some(0)); // wraps
    }

    #[test]
    fn navigate_tmux_window_backward_wraps() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 3)];
        state.tmux_selected = 0;
        state.tmux_window_cursor = Some(0);
        state.navigate_tmux_window(-1);
        assert_eq!(state.tmux_window_cursor, Some(2)); // wraps to last
    }

    #[test]
    fn navigate_tmux_window_starts_from_active() {
        let mut state = AppState::default();
        let mut ts = mk_tmux_session("a", 3);
        ts.windows[0].active = false;
        ts.windows[1].active = true;
        state.tmux_sessions = vec![ts];
        state.tmux_selected = 0;
        state.tmux_window_cursor = None; // no cursor yet
        state.navigate_tmux_window(1);
        // starts from active window (idx=1) → 1+1=2
        assert_eq!(state.tmux_window_cursor, Some(2));
    }

    #[test]
    fn navigate_tmux_window_empty_session_is_noop() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("a", 0)];
        state.tmux_selected = 0;
        state.tmux_window_cursor = None;
        state.navigate_tmux_window(1);
        assert!(state.tmux_window_cursor.is_none());
    }

    #[test]
    fn navigate_tmux_window_no_session_is_noop() {
        let mut state = AppState::default();
        state.tmux_selected = 5; // out of bounds
        state.navigate_tmux_window(1);
        assert!(state.tmux_window_cursor.is_none());
    }

    // ── visible_tmux_sessions (search filter) ───────────────────────────────

    #[test]
    fn visible_tmux_sessions_empty_query_returns_all() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 1),
            mk_tmux_session("bravo", 1),
        ];
        assert_eq!(state.visible_tmux_sessions().len(), 2);
    }

    #[test]
    fn visible_tmux_sessions_filters_by_session_name() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 1),
            mk_tmux_session("bravo", 1),
            mk_tmux_session("alphabet", 1),
        ];
        state.search_query = "alpha".into();
        let visible = state.visible_tmux_sessions();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].name, "alpha");
        assert_eq!(visible[1].name, "alphabet");
    }

    #[test]
    fn visible_tmux_sessions_filter_is_case_insensitive() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![mk_tmux_session("AlphaFoo", 1)];
        state.search_query = "ALPHA".into();
        assert_eq!(state.visible_tmux_sessions().len(), 1);
    }

    #[test]
    fn visible_tmux_sessions_filters_by_window_name() {
        let mut state = AppState::default();
        // mk_tmux_session creates windows named "win0", "win1", ...
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 2), // has "win0", "win1"
            mk_tmux_session("bravo", 0),
        ];
        state.search_query = "win1".into();
        let visible = state.visible_tmux_sessions();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "alpha");
    }

    #[test]
    fn selected_tmux_session_returns_from_filtered_list() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 1),
            mk_tmux_session("bravo", 1),
            mk_tmux_session("charlie", 1),
        ];
        state.search_query = "r".into(); // matches bravo, charlie
        state.tmux_selected = 0;
        assert_eq!(state.selected_tmux_session().unwrap().name, "bravo");
        state.tmux_selected = 1;
        assert_eq!(state.selected_tmux_session().unwrap().name, "charlie");
    }

    #[test]
    fn navigate_tmux_session_clamps_against_filtered_list() {
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 1),
            mk_tmux_session("bravo", 1),
            mk_tmux_session("charlie", 1),
        ];
        state.search_query = "alpha".into(); // only 1 match
        state.tmux_selected = 0;
        state.navigate_tmux_session(1);
        // Max is 0 under the filter — must stay put, not advance into filtered-out territory.
        assert_eq!(state.tmux_selected, 0);
    }

    #[test]
    fn apply_search_key_resets_tmux_selected() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.tmux_sessions = vec![
            mk_tmux_session("alpha", 1),
            mk_tmux_session("bravo", 1),
        ];
        state.tmux_selected = 1;
        state.tmux_expanded = true;
        state.search_mode = true;
        state.apply_search_key(KeyCode::Char('a'), false);
        assert_eq!(state.tmux_selected, 0);
        assert!(!state.tmux_expanded);
        assert!(state.tmux_window_cursor.is_none());
    }

    // ── resolve_hook_session ───────────────────────────────────────────────

    #[test]
    fn resolve_hook_session_already_exists() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 100, Some("%1")));
        assert!(state.resolve_hook_session("s1", None, None, None, None));
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn resolve_hook_session_promotes_provisional() {
        let mut state = AppState::default();
        let mut s = session_with("tmux:1", 50, Some("%1"));
        s.status = SessionStatus::Idle;
        state.sessions.push(s);

        assert!(state.resolve_hook_session("real-id", Some("%1"), Some("/tmp"), None, None));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "real-id");
    }

    #[test]
    fn resolve_hook_session_evicts_stale_pane() {
        let mut state = AppState::default();
        state.sessions.push(session_with("old-id", 1, Some("%5")));
        state.sessions.push(session_with("keep", 2, Some("%6")));

        assert!(state.resolve_hook_session("new-id", Some("%5"), Some("/proj"), None, None));
        // old-id evicted, keep preserved, new-id created
        assert_eq!(state.sessions.len(), 2);
        assert!(state.sessions.iter().any(|s| s.id == "keep"));
        assert!(state.sessions.iter().any(|s| s.id == "new-id"));
    }

    #[test]
    fn resolve_hook_session_creates_new() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", Some("%1"), Some("/proj"), Some("/t/file"), Some("main".into())));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "s1");
        assert_eq!(state.sessions[0].tmux_pane.as_deref(), Some("%1"));
        assert_eq!(state.sessions[0].transcript_path.as_deref(), Some("/t/file"));
        assert_eq!(state.sessions[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn resolve_hook_session_empty_cwd_returns_false() {
        let mut state = AppState::default();
        assert!(!state.resolve_hook_session("s1", None, None, None, None));
        assert!(!state.resolve_hook_session("s1", None, Some(""), None, None));
        assert_eq!(state.sessions.len(), 0);
    }

    #[test]
    fn resolve_hook_session_no_pane_creates_without_tmux_pane() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", None, Some("/proj"), None, None));
        assert_eq!(state.sessions[0].tmux_pane, None);
    }

    #[test]
    fn resolve_hook_session_empty_transcript_ignored() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", None, Some("/proj"), Some(""), None));
        assert_eq!(state.sessions[0].transcript_path, None);
    }

    // ── try_focus_active_pane ──────────────────────────────────────────────

    #[test]
    fn try_focus_active_pane_sets_focused_id() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.focused_session_id = None;

        state.try_focus_active_pane(Some("%1"));
        assert_eq!(state.focused_session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn try_focus_active_pane_skips_if_already_focused() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.focused_session_id = Some("other".into());

        state.try_focus_active_pane(Some("%1"));
        assert_eq!(state.focused_session_id.as_deref(), Some("other"));
    }

    #[test]
    fn try_focus_active_pane_skips_own_pane() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.own_pane = Some("%1".into());
        state.focused_session_id = None;

        state.try_focus_active_pane(Some("%1"));
        assert!(state.focused_session_id.is_none());
    }

    #[test]
    fn try_focus_active_pane_no_match() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.focused_session_id = None;

        state.try_focus_active_pane(Some("%99"));
        assert!(state.focused_session_id.is_none());
    }

    #[test]
    fn try_focus_active_pane_none_active() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.focused_session_id = None;

        state.try_focus_active_pane(None);
        assert!(state.focused_session_id.is_none());
    }

    // ── try_apply_focus_hint ───────────────────────────────────────────────

    #[test]
    fn try_apply_focus_hint_matches_and_consumes() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, None));
        state.focus_pane = Some("%5".into());

        state.try_apply_focus_hint(Some("%5"));
        assert!(state.focus_pane.is_none()); // consumed
        assert_eq!(state.selected, 0);
        assert_eq!(state.focused_session_id.as_deref(), Some("s1"));
        assert_eq!(state.sessions[0].tmux_pane.as_deref(), Some("%5"));
    }

    #[test]
    fn try_apply_focus_hint_no_match() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, None));
        state.focus_pane = Some("%5".into());

        state.try_apply_focus_hint(Some("%99"));
        assert_eq!(state.focus_pane.as_deref(), Some("%5")); // not consumed
    }

    #[test]
    fn try_apply_focus_hint_no_hint_is_noop() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, Some("%1")));
        state.focus_pane = None;

        state.try_apply_focus_hint(Some("%1"));
        assert!(state.focus_pane.is_none());
        assert!(state.focused_session_id.is_none());
    }

    #[test]
    fn try_apply_focus_hint_none_pane_is_noop() {
        let mut state = AppState::default();
        state.sessions.push(session_with("s1", 1, None));
        state.focus_pane = Some("%5".into());

        state.try_apply_focus_hint(None);
        assert_eq!(state.focus_pane.as_deref(), Some("%5")); // not consumed
    }
}
