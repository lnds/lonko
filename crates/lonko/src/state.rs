use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use crate::sources::hooks::HookPayload;
use crate::sources::transcript::{self, TranscriptInfo};

/// Map a hook event name to a `SessionStatus` update, mutating
/// per-event session fields (last_prompt, last_tool, completed_at)
/// along the way. Returns `None` for unknown events — the caller
/// should leave the status unchanged.
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
                // Skip `<<autonomous-loop-dynamic>>` and similar runtime
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
            // Status flips to Idle synchronously so the UI updates without
            // waiting on the (potentially expensive) transcript parse and
            // git_branch fork. The caller schedules a deferred transcript
            // read; the late `apply_transcript_info` runs against the
            // already-Idle session, so its `!is_active()` guard accepts
            // the fresh prompt.
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

/// What `App::handle_hook` needs to know after `AppState::apply_hook`
/// has finished mutating state. Carries the values the orchestration
/// layer (notifications, panel auto-show, deferred transcript load)
/// needs from the now-mutated session, captured up front so the
/// `&mut self` borrow on `AppState` can end before any `&self`
/// follow-up calls.
#[derive(Debug)]
pub struct HookEffect {
    /// Display name of the affected session (for desktop notifications).
    pub display_name: String,
    /// Status the session ended up in after applying the hook.
    pub status: SessionStatus,
    /// True when this hook transitioned the session into
    /// `WaitingForUser`; used to decide whether to auto-show the panel.
    pub is_now_waiting: bool,
    /// Seed for the deferred `spawn_transcript_load` call. Set only on
    /// `Stop`/`SubagentStop` hooks, where the transcript may have new
    /// model/cost/last_prompt to surface.
    pub transcript_seed: Option<TranscriptSeed>,
}

#[derive(Debug)]
pub struct TranscriptSeed {
    pub session_id: String,
    pub path: PathBuf,
    pub cwd: String,
}

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
    /// Tailnet host this session lives on. `None` for local sessions;
    /// `Some(hostname)` when the session was discovered through a remote
    /// hook event stamped by `lonko-hook --remote-tag` (LONKO-48). Drives
    /// SSH-routing of operations that target the session's tmux pane.
    pub host: Option<String>,
}

/// Trunk branches that should float to the top of their repo group in the
/// agents list. Covers the two canonical git default branch names.
fn is_trunk_branch(branch: Option<&str>) -> bool {
    matches!(branch, Some("main") | Some("master"))
}

/// Composite ordering key used within a repo group. Trunk floats first,
/// then branch name, then cwd, then tmux pane as a unique last resort.
/// Returning owned strings keeps the key self-contained so `sort_by` can
/// compare without lifetime gymnastics.
fn group_sort_key(s: &Session) -> (bool, String, String, String) {
    (
        !is_trunk_branch(s.branch.as_deref()),
        s.branch.as_deref().unwrap_or("").to_ascii_lowercase(),
        s.cwd.clone(),
        s.tmux_pane.clone().unwrap_or_default(),
    )
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
            host: None,
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
        // Preserve the hook-set last_prompt while the session is actively
        // running. The UserPromptSubmit hook fires *before* Claude writes
        // the new prompt to the transcript, so a transcript read during
        // that window returns the previous prompt and silently rolls the
        // card backwards. Once the session goes Idle/Completed, the
        // transcript is authoritative again.
        if info.last_prompt.is_some() && !self.status.is_active() {
            self.last_prompt = info.last_prompt;
        }
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

    /// Display name for this session. For grouped worktree agents (those
    /// with a `repo_root`), derives a short name from the branch:
    ///   - if the branch is trunk (`main`/`master`), uses the repo/group
    ///     name instead — "main" alone is not informative when multiple
    ///     repos are visible in the list
    ///   - otherwise takes the last `/`-separated segment of the branch
    ///     name and strips the repo group prefix (+ hyphen) if present
    ///
    /// Falls back to `project_name` when there is no branch or repo_root.
    ///
    /// Examples (group = "lonko"):
    ///   "feat/toki-24/new-agent" → "new-agent"
    ///   "lonko-3-new-agent"      → "3-new-agent"
    ///   "main"                   → "lonko"
    pub fn display_name(&self) -> &str {
        if let (Some(branch), Some(repo_root)) = (&self.branch, &self.repo_root) {
            let tail = branch.rsplit('/').next().unwrap_or(branch);
            let group = std::path::Path::new(repo_root.as_str())
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            // Trunk branches ("main"/"master") are not informative on their
            // own — show the repo/group name instead so multiple repos on
            // trunk remain distinguishable in the agents list.
            if !group.is_empty() && is_trunk_branch(Some(tail)) {
                return group;
            }
            // Strip "<group>-" prefix from the tail if present.
            let prefix_len = group.len() + 1; // +1 for '-'
            let tail_start = branch.len() - tail.len();
            if tail.len() > prefix_len
                && tail.starts_with(group)
                && tail.as_bytes()[group.len()] == b'-'
            {
                &branch[tail_start + prefix_len..]
            } else {
                &branch[tail_start..]
            }
        } else {
            &self.project_name
        }
    }
}

/// `$HOME/.cache` — matches what the tmux helper scripts
/// (`lonko-follow.sh`, `lonko-panel.sh`) hardcode. `dirs::cache_dir()`
/// diverges on macOS (`~/Library/Caches`), which broke the
/// `lonko-no-follow` sentinel handshake when lonko and the shell
/// scripts disagreed on the path. Keep this one source of truth.
pub fn lonko_cache_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".cache")
}

// ── Bookmark persistence ──────────────────────────────────────────────────────

fn bookmarks_path() -> std::path::PathBuf {
    lonko_cache_dir().join("lonko-bookmarks.json")
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

/// Modal-feature state groups. Each one bundles the boolean "this mode
/// is active" flag with the input buffers and cached values that only
/// matter while that mode is engaged. Splitting these out of `AppState`
/// keeps the parent struct readable and makes `Default::default()` /
/// `clear_*` patterns trivial — drop the whole substruct, get a clean
/// slate, no per-field reset.

#[derive(Debug, Default)]
pub struct WorktreeModeState {
    /// User is typing a branch name for a new worktree.
    pub mode: bool,
    /// Branch name buffer.
    pub input: String,
    /// The cwd to create the worktree from (set when entering the mode).
    pub cwd: Option<String>,
}

#[derive(Debug, Default)]
pub struct BookmarkModeState {
    /// User is typing a bookmark note.
    pub mode: bool,
    /// Note text being edited.
    pub input: String,
    /// The cwd whose bookmark is being edited. Captured when the modal
    /// opens so concurrent list reordering or session removal between
    /// keystrokes cannot misroute the saved note.
    pub cwd: Option<String>,
}

#[derive(Debug, Default)]
pub struct NewAgentState {
    /// User is typing the initial prompt for a new agent.
    pub mode: bool,
    /// Prompt text buffer.
    pub input: String,
    /// Editable cwd buffer (the user can override the auto-resolved cwd).
    pub cwd_input: String,
    /// The auto-resolved cwd captured when entering the mode. Used to
    /// expand `.` at submit time even if the user edited `cwd_input`.
    pub resolved_cwd: String,
    /// Which field has focus in the popup.
    pub focus: NewAgentField,
}

#[derive(Debug, Default)]
pub struct PrPickerState {
    /// PR picker modal is open (triggered by `p` in the Agents tab).
    pub mode: bool,
    /// Filter query applied to the PR list (substring, case-insensitive,
    /// matched against number, title, author and branch).
    pub query: String,
    /// Whether the background `gh pr list` call is still in flight.
    pub loading: bool,
    /// Error message from the last `gh pr list` call, if any.
    pub error: Option<String>,
    /// The cwd used for the current picker fetch (so the worktree
    /// creation routes to the same repo).
    pub cwd: Option<String>,
    /// Open PRs returned by `gh`, in the order they came back.
    pub prs: Vec<PrPickItem>,
    /// Selected index in the **filtered** picker list.
    pub selected: usize,
}

#[derive(Debug, Default)]
pub struct WtPickerState {
    /// Worktree picker modal is open (triggered by `u` in the Agents tab).
    pub mode: bool,
    /// Filter query applied to the worktree list (substring,
    /// case-insensitive, matched against branch and path).
    pub query: String,
    /// Whether the background `wt list` call is still in flight.
    pub loading: bool,
    /// Error message from the last `wt list` call, if any.
    pub error: Option<String>,
    /// The repo cwd used for the current picker fetch (so the resume
    /// routes to the same repo).
    pub cwd: Option<String>,
    /// Worktrees returned by `wt`, in the order they came back.
    pub items: Vec<WtPickItem>,
    /// Selected index in the **filtered** picker list.
    pub selected: usize,
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
    /// Worktree creation modal state.
    pub worktree: WorktreeModeState,
    /// Bookmarks: cwd → note. Persisted to disk.
    pub bookmarks: HashMap<String, String>,
    /// Bookmark editing modal state.
    pub bookmark: BookmarkModeState,
    /// Whether the help popup is visible.
    pub show_help: bool,
    /// New-agent creation modal state.
    pub new_agent: NewAgentState,
    /// Repo-root keys of groups that are collapsed in the Agents tab.
    /// When collapsed, only a summary header is shown instead of all cards.
    pub collapsed_groups: HashSet<String>,
    /// Parent session IDs whose subagents are currently expanded inline.
    /// Ephemeral — not persisted across restarts. Toggled by `e`.
    pub expanded_subagents: HashSet<String>,
    /// Remote Tailnet hosts and their tmux sessions (for the Remote tab).
    pub remote_hosts: Vec<RemoteHost>,
    /// Selected index in the flattened Remote tab list.
    pub remote_selected: usize,
    /// Hostnames excluded from remote polling (persisted to config).
    pub excluded_hosts: HashSet<String>,
    /// Whether the Remote tab is enabled (from config).
    pub remote_enabled: bool,
    /// Remote poll interval in ticks (poll_interval_secs * 10).
    pub remote_poll_ticks: u64,
    /// PR picker modal (triggered by `p` in the Agents tab): lists open
    /// PRs for the repo of the selected agent so the user can pick one
    /// to review in a fresh worktree.
    pub pr_picker: PrPickerState,
    /// Worktree picker modal (triggered by `u` in the Agents tab): lists the
    /// linked worktrees of the selected agent's repo so the user can resume
    /// Claude in one of them via `claude --continue`.
    pub worktree_picker: WtPickerState,
    /// PR info per repo, keyed by `repo_root` → branch → PrInfo.
    /// Refreshed in the background every ~30s via `gh pr list` per unique
    /// local `repo_root`, including both open and recently-merged PRs.
    /// Drives the `#NNNN` badge on agent cards; merged PRs additionally
    /// render a blinking `M` underneath. Absence means "no PR for that
    /// branch" (or `gh` unavailable).
    pub pr_infos_by_repo: HashMap<String, HashMap<String, PrInfo>>,
    /// Live chat logs per agent, keyed by `ChatKey = (host, session_id)`.
    /// `host == None` for local agents; `Some(<peer>)` for agents reached
    /// over a cross-host chat-link. Populated by `chat.reply`/`peer.reply`
    /// frames and by local `chat.send` operations from the TUI.
    pub chat_logs: HashMap<ChatKey, ChatLog>,
    /// Set of agents whose `lonko-channel` plugin is currently connected
    /// (local) or announced online by a peer host (remote). The TUI uses
    /// this to gate chat affordances (the chat view only opens for online
    /// agents).
    pub chat_online: HashSet<ChatKey>,
    /// Active chat overlay state (None when no chat view is open).
    pub chat_view: Option<ChatView>,
}

/// Identity of a chat-capable agent across hosts: `(host, session_id)`.
/// `host == None` means a local agent; `Some(hostname)` a remote one
/// reached over its chat-link. `session_id` is the stable Claude Code
/// session UUID (`Session::id`), not the pid.
pub type ChatKey = (Option<String>, String);

/// One entry in an agent's chat log. `msg_id` and `at` are populated
/// for every message but the UI doesn't read them yet — they exist so
/// later iterations (delivery indicators, timestamp rendering) can
/// hook in without re-shaping the struct.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Local UUID for outbound messages (matches `msg_id` in the wire
    /// `chat.send` frame); empty for inbound replies that didn't carry one.
    pub msg_id: String,
    pub direction: ChatDirection,
    pub text: String,
    pub at: std::time::SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDirection {
    /// Sent by the lonko TUI user, delivered to Claude via the channel.
    Out,
    /// Reply from Claude (`reply` tool call) routed back through lonko-channel.
    In,
}

#[derive(Debug, Default, Clone)]
pub struct ChatLog {
    pub messages: Vec<ChatMessage>,
    pub unread: u32,
}

/// State for the chat overlay shown when the user activates an agent's
/// chat view. `key` is the `(host, session_id)` identity of the agent.
/// Lives in `AppState` rather than App so render code can read it freely.
#[derive(Debug, Clone)]
pub struct ChatView {
    pub key: ChatKey,
    pub input: String,
    /// Number of messages to skip from the end when rendering. `0` means
    /// pin to the latest message (the common case); incremented by the
    /// scrollback keys (PgUp).
    pub scroll: u16,
}

/// Merge state of a PR as last observed by the periodic `gh pr list` poll.
/// `Closed` is folded into `Merged = false` and never lands in the cache,
/// so the only two states the UI cares about are open and merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrMergeStatus {
    Open,
    Merged,
}

/// PR badge data attached to an agent card. The number stays visible after
/// merge so the user can confirm the merge happened; the status flips the
/// rendering (a blinking `M` appears below the `#NNNN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u32,
    pub status: PrMergeStatus,
}

/// One row in the PR picker list. Mirrors the subset of `gh pr list --json`
/// fields we render. `updated_at` is the raw ISO-8601 string from gh; the UI
/// converts it to a relative label at render time.
#[derive(Debug, Clone)]
pub struct PrPickItem {
    pub number: u32,
    pub title: String,
    pub branch: String,
    pub author: String,
    pub updated_at: String,
}

/// Everything the caller needs to spawn a worktree after the user confirms
/// a PR in the picker. Returning this struct (rather than a bare number)
/// keeps the spawn site decoupled from `AppState` once the picker has been
/// cleared.
#[derive(Debug, Clone)]
pub struct PrPickerSubmit {
    pub cwd: String,
    pub number: u32,
    pub title: String,
}

/// One worktree row in the resume picker. Populated from `wt list --format
/// json`. The main/trunk worktree is filtered out before this list is built
/// — the picker only offers the linked worktrees you can resume into.
#[derive(Debug, Clone)]
pub struct WtPickItem {
    /// Branch checked out in the worktree (empty when detached).
    pub branch: String,
    /// Absolute path to the worktree directory.
    pub path: String,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
    /// Whether a tmux session for this worktree is currently alive.
    pub live: bool,
}

/// Everything the caller needs to resume a worktree after the user confirms
/// a row in the picker. Returning this struct (rather than borrowing the
/// item) keeps the spawn site decoupled from `AppState` once the picker has
/// been cleared.
#[derive(Debug, Clone)]
pub struct WtPickerSubmit {
    pub path: String,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum NewAgentField {
    #[default]
    Cwd,
    Prompt,
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

// ── Remote hosts ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStatus {
    Online,
    Unreachable,
}

/// Granular health of a registered remote peer. Variants are ordered
/// from most to least degraded so comparison operators work correctly
/// (Unreachable < ChatDead < … < Healthy).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostHealth {
    Unreachable,
    PluginMissing,
    ChatDead,
    VersionSkew { remote: String, local: String },
    Healthy,
}

/// Cached health probe results for a registered peer.
#[derive(Debug, Clone, Default)]
pub struct HealthCache {
    /// Output of `lonko --version` on the remote, or None if not yet checked.
    pub remote_version: Option<String>,
    /// Whether `~/.claude/lonko-channel/dist/index.js` exists on the remote.
    pub plugin_built: Option<bool>,
    /// Derived health level. None means "not yet checked".
    pub health: Option<HostHealth>,
    /// Monotonic instant when the last probe ran.
    pub checked_at: Option<Instant>,
    /// Consecutive version-check failures (for backoff).
    pub probe_fail_count: u32,
    /// Tick at which the next background probe is due.
    pub next_probe_tick: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteHost {
    pub hostname: String,
    pub status: HostStatus,
    pub sessions: Vec<TmuxSession>,
    /// Consecutive poll failures (reset on success).
    pub fail_count: u32,
    /// Tick number at which this host becomes eligible for polling again.
    pub next_poll_tick: u64,
    /// Version / plugin / chat health beyond mere SSH reachability.
    pub health: HealthCache,
}

/// Resolve the effective health of a host, blending SSH reachability
/// (already polled by the existing remote bridge) with the cached
/// version/plugin probe result.
///
/// Returns `None` when no probe has run yet AND the host is reachable —
/// caller should render an "unknown" placeholder. Reachability beats
/// every probed value: an Unreachable host is always Unreachable, even
/// if a stale probe says Healthy.
pub fn effective_health(host: &RemoteHost) -> Option<HostHealth> {
    if host.status == HostStatus::Unreachable {
        return Some(HostHealth::Unreachable);
    }
    host.health.health.clone()
}

#[derive(Debug, Default, PartialEq, Clone)]
pub enum Tab {
    #[default]
    Agents,
    Sessions,
    Remote,
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
            worktree: WorktreeModeState::default(),
            bookmarks: HashMap::new(),
            bookmark: BookmarkModeState::default(),
            show_help: false,
            new_agent: NewAgentState::default(),
            collapsed_groups: HashSet::new(),
            expanded_subagents: HashSet::new(),
            remote_hosts: vec![],
            remote_selected: 0,
            excluded_hosts: HashSet::new(),
            remote_enabled: false,
            remote_poll_ticks: 300, // 30s default; matches RemoteConfig::default
            pr_picker: PrPickerState::default(),
            worktree_picker: WtPickerState::default(),
            pr_infos_by_repo: HashMap::new(),
            chat_logs: HashMap::new(),
            chat_online: HashSet::new(),
            chat_view: None,
        }
    }
}

impl AppState {
    pub fn on_chat_online(&mut self, key: ChatKey) {
        self.chat_online.insert(key);
    }

    pub fn on_chat_offline(&mut self, key: &ChatKey) {
        self.chat_online.remove(key);
    }

    pub fn on_chat_reply(&mut self, key: ChatKey, text: String, _in_reply_to: String) {
        let viewing = self.chat_view.as_ref().is_some_and(|v| v.key == key);
        let log = self.chat_logs.entry(key).or_default();
        log.messages.push(ChatMessage {
            msg_id: String::new(),
            direction: ChatDirection::In,
            text,
            at: std::time::SystemTime::now(),
        });
        if !viewing {
            log.unread = log.unread.saturating_add(1);
        }
    }

    pub fn on_chat_ack(&mut self, _key: &ChatKey, _msg_id: &str, _status: &str) {
        // v1: no per-message UI state to update yet. Hook is in place
        // for v2 to flip a "delivered" indicator on the outbound bubble.
    }

    /// Append an outbound message to the chat log for `key`. Returns the
    /// freshly-minted msg_id so callers can hand it to the writer task and
    /// later match the `chat.ack` frame.
    pub fn record_chat_send(&mut self, key: ChatKey, text: String) -> String {
        let msg_id = format!("m{}", self.tick);
        let log = self.chat_logs.entry(key).or_default();
        log.messages.push(ChatMessage {
            msg_id: msg_id.clone(),
            direction: ChatDirection::Out,
            text,
            at: std::time::SystemTime::now(),
        });
        msg_id
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
                    s.display_name().to_lowercase().contains(&q)
                        || s.project_name.to_lowercase().contains(&q)
                })
                .collect()
        };

        let sorted = Self::sort_sessions(filtered);

        // Collapsed groups: keep only the first session as a placeholder
        // so the header still renders and is selectable for toggling.
        let after_groups: Vec<&Session> = if self.collapsed_groups.is_empty() {
            sorted
        } else {
            let mut seen: HashSet<&str> = HashSet::new();
            sorted
                .into_iter()
                .filter(|s| {
                    if let Some(repo) = s.repo_root.as_deref()
                        && self.collapsed_groups.contains(repo) {
                            return seen.insert(repo);
                        }
                    true
                })
                .collect()
        };

        // Inline-expand subagents of parents the user asked to open with `e`.
        // Keeps mains in the sorted order and splices their subs right after
        // so the visual hierarchy stays obvious.
        if self.expanded_subagents.is_empty() {
            return after_groups;
        }
        let mut result: Vec<&Session> = Vec::with_capacity(after_groups.len());
        for s in after_groups {
            result.push(s);
            if !self.expanded_subagents.contains(&s.id) { continue; }
            for sub in self.sessions.iter()
                .filter(|sub| sub.parent_id.as_deref() == Some(s.id.as_str()))
            {
                result.push(sub);
            }
        }
        result
    }

    /// All sessions in canonical display order (grouped by repo, trunk first)
    /// without any search filter applied. Used by `write_sessions_cache` so
    /// that `lonko focus N` numbers always match the UI ordering.
    pub fn ordered_sessions(&self) -> Vec<&Session> {
        Self::sort_sessions(self.sessions.iter().collect())
    }

    /// Count how many subagents have `parent_id == parent` in the full
    /// session list (irrespective of visibility / search filter).
    pub fn subagent_count_for(&self, parent_id: &str) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.parent_id.as_deref() == Some(parent_id))
            .count()
    }

    /// Sort sessions: drop subagents, group remaining main agents by
    /// `repo_root`, and float trunk branches to the top of each group.
    /// Subagents are intentionally excluded from the rendered list — they
    /// surface as a count badge on their parent card instead, since per-sub
    /// cards added too much noise to the agents list (LONKO-26).
    ///
    /// Local mains (host = None) come first in first-seen repo-root order;
    /// remote mains (host = Some) follow, ordered deterministically by
    /// (host, repo_root) so that reconnecting a tailnet peer or re-seeding
    /// a provisional remote agent doesn't shuffle the section.
    fn sort_sessions(sessions: Vec<&Session>) -> Vec<&Session> {
        let (local_mains, remote_mains): (Vec<&Session>, Vec<&Session>) = sessions
            .iter()
            .copied()
            .filter(|s| s.depth == 0)
            .partition(|s| s.host.is_none());

        let mut result: Vec<&Session> = Vec::with_capacity(local_mains.len() + remote_mains.len());
        result.extend(Self::group_locals(local_mains));
        result.extend(Self::group_remotes(remote_mains));
        result
    }

    /// Cluster local mains by `repo_root`, preserving first-seen order of
    /// keys (so the list doesn't jitter as hooks arrive). Within each
    /// group, sort by the composite key (trunk first, then branch, cwd,
    /// pane). Mains with no `repo_root` fall into a trailing bucket.
    fn group_locals<'a>(mains: Vec<&'a Session>) -> Vec<&'a Session> {
        let mut named: Vec<(Option<&str>, Vec<&'a Session>)> = Vec::new();
        let mut ungrouped: Vec<&'a Session> = Vec::new();
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
        for (_, group_mains) in named.iter_mut() {
            group_mains.sort_by_key(|s| group_sort_key(s));
        }
        let mut result: Vec<&Session> = Vec::with_capacity(mains.len());
        for (_, group_mains) in &named {
            result.extend(group_mains.iter().copied());
        }
        result
    }

    /// Cluster remote mains by `(host, repo_root)` in alphabetical order
    /// so the remote section stays stable across reconnects and re-seeds.
    /// Within each group the composite key still applies.
    fn group_remotes<'a>(mains: Vec<&'a Session>) -> Vec<&'a Session> {
        type RemoteGroup<'a> = (&'a str, Option<&'a str>, Vec<&'a Session>);
        let mut named: Vec<RemoteGroup<'a>> = Vec::new();
        for m in &mains {
            let host = m.host.as_deref().unwrap_or("");
            let repo = m.repo_root.as_deref();
            if let Some(entry) = named.iter_mut().find(|(h, r, _)| *h == host && *r == repo) {
                entry.2.push(*m);
            } else {
                named.push((host, repo, vec![*m]));
            }
        }
        named.sort_by(|a, b| {
            a.0.to_ascii_lowercase()
                .cmp(&b.0.to_ascii_lowercase())
                .then_with(|| a.1.unwrap_or("").cmp(b.1.unwrap_or("")))
        });
        for (_, _, group_mains) in named.iter_mut() {
            group_mains.sort_by_key(|s| group_sort_key(s));
        }
        let mut result: Vec<&Session> = Vec::with_capacity(mains.len());
        for (_, _, group_mains) in &named {
            result.extend(group_mains.iter().copied());
        }
        result
    }

    /// Toggle collapse state for a repo-root group.
    pub fn toggle_group_collapse(&mut self, repo_root: &str) {
        if !self.collapsed_groups.remove(repo_root) {
            self.collapsed_groups.insert(repo_root.to_string());
        }
    }

    /// Toggle inline expansion of a parent session's subagents.
    pub fn toggle_subagent_expand(&mut self, parent_id: &str) {
        if !self.expanded_subagents.remove(parent_id) {
            self.expanded_subagents.insert(parent_id.to_string());
        }
    }

    /// Whether a repo-root group is currently collapsed.
    pub fn is_group_collapsed(&self, repo_root: &str) -> bool {
        self.collapsed_groups.contains(repo_root)
    }

    /// Count main agents (depth 0) sharing a given repo_root across all
    /// sessions (ignoring search filter and collapse state).
    pub fn group_agent_count(&self, repo_root: &str) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.depth == 0 && s.repo_root.as_deref() == Some(repo_root))
            .count()
    }

    /// Look up the PR info for a `(repo_root, branch)` pair, if the
    /// background refresh has seen one. Returns `None` when either side is
    /// missing, the repo hasn't been polled yet, or no PR exists.
    pub fn pr_info_for(&self, repo_root: Option<&str>, branch: Option<&str>) -> Option<PrInfo> {
        let root = repo_root?;
        let branch = branch?;
        self.pr_infos_by_repo.get(root)?.get(branch).copied()
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

    /// Attach `pid` to a pidless local provisional whose `tmux_pane`
    /// matches `pane`. Returns `true` on a hit. Used by
    /// `on_session_discovered` as a fallback when the id-based match
    /// misses because the hook's stamped `session_id` diverged from the
    /// transcript's (common after `/clear`). Without this, the
    /// provisional would stay at `pid == 0` and `reap_dead_local_sessions`
    /// would never touch it.
    ///
    /// The provisional's `id` is intentionally left alone: future hooks
    /// from that Claude still carry the hook-original id, and the
    /// transcript-resolved id can point at a different Claude sharing
    /// the same cwd.
    pub fn promote_pidless_by_pane(&mut self, pid: u32, pane: &str) -> bool {
        let Some(s) = self.sessions.iter_mut().find(|s| {
            s.pid == 0 && s.host.is_none() && s.tmux_pane.as_deref() == Some(pane)
        }) else { return false };
        s.pid = pid;
        true
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

    /// Reconcile the agent list against a fresh snapshot of panes on a
    /// remote host. Any session whose `host` matches and whose `tmux_pane`
    /// is *not* in `live_panes` is treated the same way `handle_pane_gone`
    /// treats a local pane that vanished: active sessions are marked
    /// `Completed` so they fade out, already-completed ones are evicted.
    ///
    /// Without this step, provisional agents seeded by an earlier snapshot
    /// would linger forever after the user killed the remote Claude pane,
    /// producing phantom cards like the triple `shinkansen-monorail`
    /// screenshot in LONKO-??.
    pub fn reconcile_remote_panes(
        &mut self,
        host: &str,
        live_panes: &std::collections::HashSet<&str>,
    ) {
        let dead: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                if s.host.as_deref() != Some(host) {
                    return None;
                }
                let pane = s.tmux_pane.as_deref()?;
                if live_panes.contains(pane) {
                    return None;
                }
                Some(i)
            })
            .collect();
        for i in dead.into_iter().rev() {
            let session = &mut self.sessions[i];
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
                    self.sessions.remove(i);
                }
            }
        }
        self.clamp_selected();
    }

    /// Local counterpart to `reconcile_remote_panes`: reap agents whose
    /// owning Claude process has exited. Catches phantoms left over when
    /// SessionEnd never fired (kill -9, crash, lonko restart after the
    /// process died) and `lifecycle` never saw the file disappear.
    ///
    /// Conservative on purpose — only acts when the PID is clearly dead.
    /// Panes whose Claude process has crashed but whose PID got reused by
    /// an unrelated process are *not* reaped here; `TmuxPaneGone` still
    /// handles the "pane went away" case separately.
    pub fn reap_dead_local_sessions(&mut self, is_alive: impl Fn(u32) -> bool) {
        let dead: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                if s.host.is_some() { return None; }
                if s.pid == 0 { return None; }
                if is_alive(s.pid) { return None; }
                Some(i)
            })
            .collect();
        for i in dead.into_iter().rev() {
            let session = &mut self.sessions[i];
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
                    self.sessions.remove(i);
                }
            }
        }
        self.clamp_selected();
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
                self.worktree.mode = false;
                self.worktree.input.clear();
                self.worktree.cwd = None;
            }
            KeyCode::Enter => {
                let branch = self.worktree.input.trim().to_string();
                self.worktree.mode = false;
                self.worktree.input.clear();
                if !branch.is_empty() {
                    return Some(branch);
                }
                self.worktree.cwd = None;
            }
            KeyCode::Backspace => { self.worktree.input.pop(); }
            KeyCode::Char('c') if ctrl => {
                self.worktree.mode = false;
                self.worktree.input.clear();
                self.worktree.cwd = None;
            }
            KeyCode::Char(c) => { self.worktree.input.push(c); }
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
                self.bookmark.mode = false;
                self.bookmark.input.clear();
                self.bookmark.cwd = None;
            }
            KeyCode::Enter => {
                let note = self.bookmark.input.trim().to_string();
                self.bookmark.mode = false;
                self.bookmark.input.clear();
                return Some(note);
            }
            KeyCode::Backspace => { self.bookmark.input.pop(); }
            KeyCode::Char('c') if ctrl => {
                self.bookmark.mode = false;
                self.bookmark.input.clear();
                self.bookmark.cwd = None;
            }
            KeyCode::Char(c) => { self.bookmark.input.push(c); }
            _ => {}
        }
        None
    }

    /// Apply a key to new-agent mode. Returns `Some((prompt, cwd))` on Enter
    /// in the Prompt field when both are non-empty. Enter in the Cwd field
    /// jumps focus to Prompt. Tab toggles between fields.
    pub fn apply_new_agent_key(&mut self, code: crossterm::event::KeyCode, ctrl: bool) -> Option<(String, String)> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.clear_new_agent();
            }
            KeyCode::Char('c') if ctrl => {
                self.clear_new_agent();
            }
            KeyCode::Tab => match self.new_agent.focus {
                NewAgentField::Cwd => {
                    self.new_agent.cwd_input =
                        crate::new_agent::complete_path(&self.new_agent.cwd_input);
                }
                NewAgentField::Prompt => {
                    self.new_agent.focus = NewAgentField::Cwd;
                }
            }
            KeyCode::Enter => match self.new_agent.focus {
                NewAgentField::Cwd => {
                    self.new_agent.focus = NewAgentField::Prompt;
                }
                NewAgentField::Prompt => {
                    let prompt = self.new_agent.input.trim().to_string();
                    let raw_cwd = self.new_agent.cwd_input.trim().to_string();
                    // Resolve `.` to the auto-detected cwd.
                    let cwd = if raw_cwd == "." {
                        self.new_agent.resolved_cwd.clone()
                    } else {
                        raw_cwd.clone()
                    };
                    if cwd.is_empty() {
                        self.new_agent.focus = NewAgentField::Cwd;
                    } else if prompt.is_empty() {
                        // Nothing to submit — stay in prompt field.
                    } else {
                        self.clear_new_agent();
                        return Some((prompt, cwd));
                    }
                }
            }
            KeyCode::Backspace => match self.new_agent.focus {
                NewAgentField::Cwd => { self.new_agent.cwd_input.pop(); }
                NewAgentField::Prompt => { self.new_agent.input.pop(); }
            }
            KeyCode::Char(c) if c != '\n' && c != '\r' => match self.new_agent.focus {
                NewAgentField::Cwd => { self.new_agent.cwd_input.push(c); }
                NewAgentField::Prompt => { self.new_agent.input.push(c); }
            }
            _ => {}
        }
        None
    }

    /// Apply a key to the PR picker. Returns `Some(PrPickerSubmit)` when the
    /// user pressed Enter on a valid row — the caller is expected to kick
    /// off the worktree creation for that PR. The picker state is cleared
    /// before returning so callers don't have to. Navigation, filter edits
    /// and cancels are resolved in-place on `AppState`.
    pub fn apply_pr_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        ctrl: bool,
    ) -> Option<PrPickerSubmit> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.clear_pr_picker();
            }
            KeyCode::Char('c') if ctrl => {
                self.clear_pr_picker();
            }
            KeyCode::Enter => {
                if let Some(pr) = self.selected_pr_picker_item() {
                    let submit = PrPickerSubmit {
                        cwd: self.pr_picker.cwd.clone().unwrap_or_default(),
                        number: pr.number,
                        title: pr.title.clone(),
                    };
                    self.clear_pr_picker();
                    return Some(submit);
                }
            }
            KeyCode::Up => { self.navigate_pr_picker(-1); }
            KeyCode::Down => { self.navigate_pr_picker(1); }
            KeyCode::Char('p') if ctrl => { self.navigate_pr_picker(-1); }
            KeyCode::Char('n') if ctrl => { self.navigate_pr_picker(1); }
            KeyCode::Backspace => {
                self.pr_picker.query.pop();
                self.pr_picker.selected = 0;
            }
            KeyCode::Char(c) => {
                self.pr_picker.query.push(c);
                self.pr_picker.selected = 0;
            }
            _ => {}
        }
        None
    }

    /// Reset picker state back to closed.
    pub fn clear_pr_picker(&mut self) {
        self.pr_picker.mode = false;
        self.pr_picker.query.clear();
        self.pr_picker.loading = false;
        self.pr_picker.error = None;
        self.pr_picker.cwd = None;
        self.pr_picker.prs.clear();
        self.pr_picker.selected = 0;
    }

    /// Substring-match the query against each PR's number, title, branch
    /// and author. Empty query returns all PRs in insertion order.
    pub fn filtered_pr_picker(&self) -> Vec<&PrPickItem> {
        if self.pr_picker.query.is_empty() {
            return self.pr_picker.prs.iter().collect();
        }
        let q = self.pr_picker.query.to_lowercase();
        self.pr_picker.prs
            .iter()
            .filter(|p| {
                p.number.to_string().contains(&q)
                    || p.title.to_lowercase().contains(&q)
                    || p.branch.to_lowercase().contains(&q)
                    || p.author.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Clamp and move the picker selection cursor by `delta`. The cursor
    /// indexes into the **filtered** list so it stays consistent as the
    /// user narrows the query.
    pub fn navigate_pr_picker(&mut self, delta: isize) {
        let len = self.filtered_pr_picker().len();
        if len == 0 {
            self.pr_picker.selected = 0;
            return;
        }
        let max = len - 1;
        if delta > 0 {
            self.pr_picker.selected = (self.pr_picker.selected + 1).min(max);
        } else {
            self.pr_picker.selected = self.pr_picker.selected.saturating_sub(1);
        }
    }

    /// Returns the currently selected PR in the filtered list, or `None`
    /// when the list is empty.
    pub fn selected_pr_picker_item(&self) -> Option<&PrPickItem> {
        self.filtered_pr_picker().into_iter().nth(self.pr_picker.selected)
    }

    /// Apply a key to the worktree picker. Returns `Some(WtPickerSubmit)`
    /// when the user pressed Enter on a valid row — the caller is expected
    /// to resume Claude in that worktree. The picker state is cleared before
    /// returning. Navigation, filter edits and cancels resolve in-place.
    pub fn apply_worktree_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        ctrl: bool,
    ) -> Option<WtPickerSubmit> {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.clear_worktree_picker();
            }
            KeyCode::Char('c') if ctrl => {
                self.clear_worktree_picker();
            }
            KeyCode::Enter => {
                if let Some(item) = self.selected_worktree_picker_item() {
                    let submit = WtPickerSubmit { path: item.path.clone() };
                    self.clear_worktree_picker();
                    return Some(submit);
                }
            }
            KeyCode::Up => { self.navigate_worktree_picker(-1); }
            KeyCode::Down => { self.navigate_worktree_picker(1); }
            KeyCode::Char('p') if ctrl => { self.navigate_worktree_picker(-1); }
            KeyCode::Char('n') if ctrl => { self.navigate_worktree_picker(1); }
            KeyCode::Backspace => {
                self.worktree_picker.query.pop();
                self.worktree_picker.selected = 0;
            }
            KeyCode::Char(c) => {
                self.worktree_picker.query.push(c);
                self.worktree_picker.selected = 0;
            }
            _ => {}
        }
        None
    }

    /// Reset worktree picker state back to closed.
    pub fn clear_worktree_picker(&mut self) {
        self.worktree_picker.mode = false;
        self.worktree_picker.query.clear();
        self.worktree_picker.loading = false;
        self.worktree_picker.error = None;
        self.worktree_picker.cwd = None;
        self.worktree_picker.items.clear();
        self.worktree_picker.selected = 0;
    }

    /// Substring-match the query against each worktree's branch and path.
    /// Empty query returns all worktrees in insertion order.
    pub fn filtered_worktree_picker(&self) -> Vec<&WtPickItem> {
        if self.worktree_picker.query.is_empty() {
            return self.worktree_picker.items.iter().collect();
        }
        let q = self.worktree_picker.query.to_lowercase();
        self.worktree_picker.items
            .iter()
            .filter(|w| {
                w.branch.to_lowercase().contains(&q)
                    || w.path.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Clamp and move the worktree picker selection cursor by `delta`. The
    /// cursor indexes into the **filtered** list so it stays consistent as
    /// the user narrows the query.
    pub fn navigate_worktree_picker(&mut self, delta: isize) {
        let len = self.filtered_worktree_picker().len();
        if len == 0 {
            self.worktree_picker.selected = 0;
            return;
        }
        let max = len - 1;
        if delta > 0 {
            self.worktree_picker.selected = (self.worktree_picker.selected + 1).min(max);
        } else {
            self.worktree_picker.selected = self.worktree_picker.selected.saturating_sub(1);
        }
    }

    /// Returns the currently selected worktree in the filtered list, or
    /// `None` when the list is empty.
    pub fn selected_worktree_picker_item(&self) -> Option<&WtPickItem> {
        self.filtered_worktree_picker().into_iter().nth(self.worktree_picker.selected)
    }

    /// Open the new-agent popup. If `cwd` is non-empty, the Dir field
    /// shows `.` (shorthand for "same directory") and the resolved path
    /// is stored for expansion at submit time. An empty `cwd` leaves
    /// the Dir field empty so the user must type a path.
    pub fn open_new_agent(&mut self, cwd: String) {
        self.new_agent.resolved_cwd = cwd.clone();
        self.new_agent.cwd_input = if cwd.is_empty() {
            String::new()
        } else {
            ".".to_string()
        };
        self.new_agent.input.clear();
        self.new_agent.focus = if cwd.is_empty() {
            NewAgentField::Cwd
        } else {
            NewAgentField::Prompt
        };
        self.new_agent.mode = true;
    }

    fn clear_new_agent(&mut self) {
        self.new_agent.mode = false;
        self.new_agent.input.clear();
        self.new_agent.cwd_input.clear();
        self.new_agent.resolved_cwd.clear();
        self.new_agent.focus = NewAgentField::default();
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

    /// Cache the tmux pane id of a session by id. Used after the pane
    /// has been discovered via `find_pane_for_pid` or first reported
    /// via a hook, so subsequent operations (focus, kill, permission
    /// send) can take the fast path without re-walking the process
    /// tree. Silently ignores unknown ids.
    pub fn cache_pane_for_session(&mut self, session_id: &str, pane: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            s.tmux_pane = Some(pane.to_string());
        }
    }

    /// Apply a hook payload to the state: resolve the session, mutate
    /// its fields, compute the new status, and return a `HookEffect`
    /// that carries the values `App::handle_hook` needs to orchestrate
    /// notifications and the deferred transcript fetch.
    ///
    /// `live_branch` is computed by the caller (synchronous git fork)
    /// and threaded through to `resolve_hook_session`. Keeping the
    /// fork outside `state.rs` lets the state layer stay I/O-free
    /// and unit-testable.
    ///
    /// Returns `None` when the payload is dropped (no session_id, or
    /// subagent without an agent_id, or unknown event name) — the
    /// caller should bail out of the rest of `handle_hook`.
    pub fn apply_hook(
        &mut self,
        payload: &HookPayload,
        live_branch: Option<String>,
    ) -> Option<HookEffect> {
        let parent_session_id = payload.session_id.as_deref().filter(|s| !s.is_empty())?;
        let is_subagent = payload.agent_type.as_ref().is_some_and(|t| !t.is_empty());
        let effective_id: String = if is_subagent {
            let id = payload.agent_id.as_deref().filter(|s| !s.is_empty())?;
            id.to_string()
        } else {
            parent_session_id.to_string()
        };

        let hook_pane = payload.tmux_pane.as_deref().filter(|p| !p.is_empty());
        let hook_cwd = payload.cwd.as_deref().filter(|c| !c.is_empty());

        if is_subagent {
            // Create the subagent's session entry on first hook if it
            // doesn't already exist. Inherits the parent's group key
            // and depth so it clusters under the parent in the list.
            if !self.sessions.iter().any(|s| s.id == effective_id) {
                let cwd = hook_cwd.unwrap_or_default().to_string();
                if cwd.is_empty() { return None; }
                let parent_id_owned = parent_session_id.to_string();
                let (parent_depth, parent_repo_root) = self.sessions.iter()
                    .find(|s| s.id == parent_id_owned)
                    .map(|s| (s.depth, s.repo_root.clone()))
                    .unwrap_or((0, None));
                let agent_type = payload.agent_type.as_deref().unwrap_or("sub");
                let mut session = Session::new(effective_id.clone(), 0, cwd);
                session.status = SessionStatus::Running;
                session.parent_id = Some(parent_id_owned);
                session.depth = (parent_depth + 1).min(2);
                session.project_name = agent_type.to_string();
                session.repo_root = parent_repo_root;
                if let Some(pane) = hook_pane {
                    session.tmux_pane = Some(pane.to_string());
                }
                if let Some(tp) = payload.agent_transcript_path.as_deref().filter(|t| !t.is_empty()) {
                    session.transcript_path = Some(tp.to_string());
                }
                self.sessions.push(session);
            }
        } else {
            if !self.resolve_hook_session(
                &effective_id,
                hook_pane,
                hook_cwd,
                payload.transcript_path.as_deref(),
                live_branch,
                payload.host.as_deref(),
            ) {
                return None;
            }
            // Fill in the group key for brand-new sessions; the cwd fallback
            // ensures non-git sessions never re-trigger the shell call on
            // subsequent hook events.
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == effective_id)
                && s.repo_root.is_none()
                && !s.cwd.is_empty()
            {
                s.repo_root = Some(
                    crate::worktree::repo_common_root(&s.cwd).unwrap_or_else(|| s.cwd.clone()),
                );
            }
        }

        let session = self.sessions.iter_mut().find(|s| s.id == effective_id)?;

        // Update tmux pane if available.
        if let Some(pane) = &payload.tmux_pane
            && !pane.is_empty()
        {
            session.tmux_pane = Some(pane.clone());
        }

        // Cache transcript path (prefer agent_transcript_path for subagents).
        if is_subagent {
            if let Some(tp) = &payload.agent_transcript_path
                && !tp.is_empty()
            {
                session.transcript_path = Some(tp.clone());
            }
        } else if let Some(tp) = &payload.transcript_path
            && !tp.is_empty()
        {
            session.transcript_path = Some(tp.clone());
        }

        // Update cwd if available (skip for subagents — they share the parent's cwd).
        if !is_subagent
            && let Some(cwd) = &payload.cwd
            && !cwd.is_empty()
            && session.cwd != *cwd
        {
            session.cwd = cwd.clone();
            session.project_name = cwd.split('/').next_back().unwrap_or(cwd).to_string();
        }

        // Stamp the originating host so later operations can route to the
        // right tmux server. Only overwrite when the incoming payload
        // asserts a host: a later local-only hook should not clobber a
        // session that belongs to a remote machine.
        if payload.host.is_some() {
            session.host = payload.host.clone();
        }

        session.last_activity = std::time::Instant::now();

        let event_name = payload.hook_event_name.as_deref().unwrap_or("");

        // SubagentStop for a subagent means it's done.
        if is_subagent && event_name == "SubagentStop" {
            session.completed_at = Some(std::time::Instant::now());
            session.status = SessionStatus::Completed;
        } else {
            let new_status = hook_event_to_status(event_name, payload, session)?;
            session.status = new_status;
        }

        // Snapshot the values the caller needs before returning, while
        // we still hold the &mut Session. After this the borrow ends.
        let is_now_waiting = matches!(session.status, SessionStatus::WaitingForUser(_));
        let display_name = session.display_name().to_string();
        let status = session.status.clone();
        let transcript_seed = if matches!(event_name, "Stop" | "SubagentStop") {
            let path = session.transcript_path.clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| transcript::transcript_path(&session.cwd, &session.id));
            Some(TranscriptSeed {
                session_id: session.id.clone(),
                path,
                cwd: session.cwd.clone(),
            })
        } else {
            None
        };

        Some(HookEffect {
            display_name,
            status,
            is_now_waiting,
            transcript_seed,
        })
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
        hook_host: Option<&str>,
    ) -> bool {
        if self.sessions.iter().any(|s| s.id == session_id) {
            return true;
        }

        // Try to promote a provisional session discovered by one of the
        // scanners. Matching is per-origin: a local hook promotes a
        // `tmux:<pane>` entry; a remote hook promotes a
        // `remote:<host>:<pane>` entry for the same host. This keeps
        // identical pane ids on different tmux servers from colliding.
        let promoted = if let Some(pane) = hook_pane {
            match hook_host {
                Some(host) => self.sessions.iter_mut().find(|s| {
                    s.tmux_pane.as_deref() == Some(pane)
                        && s.host.as_deref() == Some(host)
                        && s.id.starts_with("remote:")
                }),
                None => self.sessions.iter_mut().find(|s| {
                    s.tmux_pane.as_deref() == Some(pane)
                        && s.host.is_none()
                        && (s.id.starts_with("tmux:") || s.id.starts_with("lifecycle:"))
                }),
            }
        } else {
            None
        };

        if let Some(s) = promoted {
            s.id = session_id.to_string();
        } else {
            // Evict any stale session for the same pane — but only
            // within the same origin (local↔local, remote-host↔same
            // remote-host).
            if let Some(pane) = hook_pane {
                self.sessions.retain(|s| {
                    if s.tmux_pane.as_deref() != Some(pane) { return true; }
                    if s.id == session_id { return true; }
                    match (s.host.as_deref(), hook_host) {
                        (None, None) => false,
                        (Some(a), Some(b)) if a == b => false,
                        _ => true,
                    }
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
            if let Some(tp) = transcript_path
                && !tp.is_empty() { session.transcript_path = Some(tp.to_string()); }
            session.branch = git_branch;
            if let Some(h) = hook_host {
                session.host = Some(h.to_string());
            }
            self.sessions.push(session);
        }
        true
    }

    /// Decide what to do with an incoming lifecycle event for `(session_id, pid, tmux_pane)`.
    ///
    /// Returns `None` when the event maps to a session lonko already tracks
    /// (matched by pid, by pane, or by id-on-an-unclaimed-provisional). The
    /// caller should drop the event.
    ///
    /// Returns `Some(id)` when a new entry should be inserted, where `id` is
    /// either the original `session_id` or a synthetic `lifecycle:<pid>` if
    /// another agent already claimed that id. The collision case shows up
    /// when N>1 Claudes share a cwd and `most_recent_transcript_session`
    /// resolves every lifecycle file to the same id.
    pub fn lifecycle_session_id(
        &self,
        session_id: &str,
        pid: u32,
        tmux_pane: Option<&str>,
    ) -> Option<String> {
        let exists = self.sessions.iter().any(|s| {
            if s.pid == pid { return true; }
            if let Some(p) = tmux_pane
                && s.tmux_pane.as_deref() == Some(p)
            {
                return true;
            }
            // An unclaimed provisional with the same id was likely
            // pre-created by an earlier hook for *this* Claude.
            s.id == session_id && s.pid == 0
        });
        if exists {
            return None;
        }
        if self.sessions.iter().any(|s| s.id == session_id) {
            return Some(format!("lifecycle:{pid}"));
        }
        Some(session_id.to_string())
    }

    /// If focused_session_id is None, check if the last session matches the active pane.
    /// `active_pane` and `own_pane` are passed in to avoid tmux calls inside state logic.
    pub fn try_focus_active_pane(&mut self, active_pane: Option<&str>) {
        if self.focused_session_id.is_some() { return; }
        let Some(active) = active_pane else { return; };
        let is_own = self.own_pane.as_deref() == Some(active);
        if is_own { return; }

        let last = self.sessions.last();
        if let Some(s) = last
            && s.tmux_pane.as_deref() == Some(active) {
                self.focused_session_id = Some(s.id.clone());
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
            Tab::Sessions => {
                if self.remote_enabled { Tab::Remote } else { Tab::Agents }
            }
            Tab::Remote => Tab::Agents,
        };
    }

    /// Total number of items in the Remote tab (for clamping selection).
    pub fn remote_item_count(&self) -> usize {
        self.remote_hosts.iter().map(|h| h.sessions.len()).sum()
    }

    /// Navigate the remote session list by `delta` (+1 down, -1 up).
    pub fn navigate_remote(&mut self, delta: isize) {
        let count = self.remote_item_count();
        if count == 0 { return; }
        if delta > 0 {
            self.remote_selected = (self.remote_selected + 1).min(count - 1);
        } else {
            self.remote_selected = self.remote_selected.saturating_sub(1);
        }
    }

    /// Return the hostname of the currently selected remote item.
    pub fn selected_remote_host(&self) -> Option<&str> {
        let mut idx = 0;
        for host in &self.remote_hosts {
            let count = host.sessions.len();
            if count > 0 && self.remote_selected < idx + count {
                return Some(&host.hostname);
            }
            idx += count;
        }
        None
    }

    /// Return the (hostname, session_name) for the currently selected remote item.
    pub fn selected_remote_session(&self) -> Option<(&str, &str)> {
        let mut idx = 0;
        for host in &self.remote_hosts {
            for session in &host.sessions {
                if idx == self.remote_selected {
                    return Some((&host.hostname, &session.name));
                }
                idx += 1;
            }
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
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

    #[test]
    fn apply_transcript_info_keeps_last_prompt_when_running() {
        // Guards the race where UserPromptSubmit has set last_prompt to the
        // newest prompt but Claude hasn't yet flushed it to the transcript.
        let mut s = mk_session();
        s.status = SessionStatus::Running;
        s.last_prompt = Some("new hook prompt".into());
        let mut info = mk_info();
        info.last_prompt = Some("stale transcript prompt".into());
        info.context_tokens = 42; // other fields should still merge
        s.apply_transcript_info(info);
        assert_eq!(s.last_prompt.as_deref(), Some("new hook prompt"));
        assert_eq!(s.context_used, 42);
    }

    #[test]
    fn apply_transcript_info_updates_last_prompt_when_idle() {
        let mut s = mk_session();
        s.status = SessionStatus::Idle;
        s.last_prompt = Some("old".into());
        let mut info = mk_info();
        info.last_prompt = Some("transcript-latest".into());
        s.apply_transcript_info(info);
        assert_eq!(s.last_prompt.as_deref(), Some("transcript-latest"));
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
    fn visible_sessions_orders_non_trunk_alphabetically_by_branch() {
        let mut state = AppState::default();
        // Insert in reverse-alphabetical branch order to prove the sort
        // drives the final position rather than insertion order.
        state.sessions = vec![
            main_with_repo_branch("feat3", "/r/alpha", "feat-c"),
            main_with_repo_branch("feat1", "/r/alpha", "feat-a"),
            main_with_repo_branch("feat2", "/r/alpha", "feat-b"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["feat1", "feat2", "feat3"]);
    }

    #[test]
    fn visible_sessions_tie_breaks_by_cwd_then_pane() {
        let mut state = AppState::default();
        // Two agents in the same worktree on the same branch — pane ID
        // is the final, unique tie-breaker so the order never flips.
        let mut a = main_with_repo_branch("a", "/r/alpha", "feat");
        a.cwd = "/tmp/alpha".into();
        a.tmux_pane = Some("%20".into());
        let mut b = main_with_repo_branch("b", "/r/alpha", "feat");
        b.cwd = "/tmp/alpha".into();
        b.tmux_pane = Some("%10".into());
        // Insert in the "wrong" order to prove the composite key wins.
        state.sessions = vec![a, b];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn visible_sessions_order_survives_branch_flip() {
        let mut state = AppState::default();
        // Simulate a transcript re-read swapping two worktrees' branch
        // labels: with the old insertion-order sort, the positions would
        // shuffle; with the composite key, the alphabetical branch order
        // alone determines the outcome.
        state.sessions = vec![
            main_with_repo_branch("x", "/r/alpha", "feat-z"),
            main_with_repo_branch("y", "/r/alpha", "feat-a"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["y", "x"]);

        // Flip the branches and confirm the order follows the new labels.
        state.sessions[0].branch = Some("feat-a".into());
        state.sessions[1].branch = Some("feat-z".into());
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["x", "y"]);
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

    fn remote_main(id: &str, host: &str, repo: &str, branch: &str) -> Session {
        let mut s = main_with_repo_branch(id, repo, branch);
        s.host = Some(host.into());
        s
    }

    #[test]
    fn visible_sessions_places_remote_agents_after_local() {
        let mut state = AppState::default();
        // Remote inserted before local should still land at the bottom.
        state.sessions = vec![
            remote_main("r1", "nyx", "/remote/alpha", "main"),
            main_with_repo_branch("local1", "/r/alpha", "main"),
            main_with_repo_branch("local2", "/r/alpha", "feat-a"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["local1", "local2", "r1"]);
    }

    #[test]
    fn visible_sessions_orders_remotes_deterministically_by_host() {
        let mut state = AppState::default();
        // Insert in non-alphabetical host order; result must come back sorted.
        state.sessions = vec![
            remote_main("r_zeus_a", "zeus", "/remote/foo", "main"),
            remote_main("r_nyx_a", "nyx", "/remote/foo", "main"),
            remote_main("r_apollo_a", "apollo", "/remote/foo", "main"),
        ];
        let ids: Vec<&str> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, vec!["r_apollo_a", "r_nyx_a", "r_zeus_a"]);
    }

    #[test]
    fn visible_sessions_remote_order_stable_on_reinsertion() {
        // Simulates a provisional remote agent being re-seeded: the insertion
        // order changes but the final order must not.
        let mut state = AppState::default();
        state.sessions = vec![
            remote_main("a", "nyx", "/remote/a", "main"),
            remote_main("b", "zeus", "/remote/b", "main"),
        ];
        let ids1: Vec<String> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        state.sessions = vec![
            remote_main("b", "zeus", "/remote/b", "main"),
            remote_main("a", "nyx", "/remote/a", "main"),
        ];
        let ids2: Vec<String> = state
            .visible_sessions()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(ids1, ids2);
        assert_eq!(ids1, vec!["a".to_string(), "b".to_string()]);
    }

    fn mk_pr(n: u32, title: &str, branch: &str, author: &str) -> PrPickItem {
        PrPickItem {
            number: n,
            title: title.into(),
            branch: branch.into(),
            author: author.into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn remote_session(id: &str, host: &str, pane: &str, status: SessionStatus) -> Session {
        let mut s = Session::new(id.into(), 0, format!("/remote/{host}/{id}"));
        s.host = Some(host.into());
        s.tmux_pane = Some(pane.into());
        s.status = status;
        s
    }

    #[test]
    fn reconcile_remote_panes_removes_idle_sessions_from_dead_panes() {
        let mut state = AppState::default();
        state.sessions = vec![
            remote_session("remote:nyx:%1", "nyx", "%1", SessionStatus::Idle),
            remote_session("remote:nyx:%2", "nyx", "%2", SessionStatus::Idle),
        ];
        let mut live = std::collections::HashSet::new();
        live.insert("%2");
        state.reconcile_remote_panes("nyx", &live);

        // The pane that died fades to Completed first (so the card can flash
        // away on the next tick) instead of being evicted immediately.
        assert_eq!(state.sessions.len(), 2);
        let gone = state.sessions.iter().find(|s| s.tmux_pane.as_deref() == Some("%1")).unwrap();
        assert_eq!(gone.status, SessionStatus::Completed);
        assert!(gone.completed_at.is_some());
    }

    #[test]
    fn reconcile_remote_panes_evicts_already_completed_sessions() {
        // Simulate the second tick after a pane went away: the session is
        // already Completed from the previous reconcile — now it should go.
        let mut state = AppState::default();
        state.sessions = vec![
            remote_session("remote:nyx:%1", "nyx", "%1", SessionStatus::Completed),
        ];
        let live: std::collections::HashSet<&str> = std::collections::HashSet::new();
        state.reconcile_remote_panes("nyx", &live);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn reconcile_remote_panes_leaves_other_hosts_untouched() {
        let mut state = AppState::default();
        state.sessions = vec![
            remote_session("remote:nyx:%1", "nyx", "%1", SessionStatus::Idle),
            remote_session("remote:zeus:%1", "zeus", "%1", SessionStatus::Idle),
        ];
        let live: std::collections::HashSet<&str> = std::collections::HashSet::new();
        state.reconcile_remote_panes("nyx", &live);
        // The zeus session must survive — we reconciled only against nyx.
        assert!(state.sessions.iter().any(|s| s.host.as_deref() == Some("zeus")));
    }

    #[test]
    fn reconcile_remote_panes_never_touches_local_sessions() {
        let mut state = AppState::default();
        let mut local = Session::new("local-1".into(), 0, "/tmp/a".into());
        local.tmux_pane = Some("%1".into()); // same pane id as a remote one
        local.status = SessionStatus::Idle;
        state.sessions = vec![
            local,
            remote_session("remote:nyx:%1", "nyx", "%1", SessionStatus::Idle),
        ];
        let live: std::collections::HashSet<&str> = std::collections::HashSet::new();
        state.reconcile_remote_panes("nyx", &live);
        // Local session sharing the pane id stays; remote gets faded.
        assert!(state.sessions.iter().any(|s| s.host.is_none()));
    }

    #[test]
    fn reap_dead_local_sessions_fades_idle_whose_pid_is_dead() {
        let mut state = AppState::default();
        let mut alive = Session::new("alive".into(), 100, "/tmp/a".into());
        alive.status = SessionStatus::Idle;
        alive.tmux_pane = Some("%1".into());
        let mut dead = Session::new("dead".into(), 200, "/tmp/b".into());
        dead.status = SessionStatus::Idle;
        state.sessions = vec![alive, dead];

        state.reap_dead_local_sessions(|pid| pid == 100);

        let a = state.sessions.iter().find(|s| s.id == "alive").unwrap();
        assert_eq!(a.status, SessionStatus::Idle);
        let d = state.sessions.iter().find(|s| s.id == "dead").unwrap();
        assert_eq!(d.status, SessionStatus::Completed);
        assert!(d.completed_at.is_some());
    }

    #[test]
    fn reap_dead_local_sessions_evicts_already_completed() {
        let mut state = AppState::default();
        let mut s = Session::new("dead".into(), 200, "/tmp/b".into());
        s.status = SessionStatus::Completed;
        state.sessions = vec![s];
        state.reap_dead_local_sessions(|_| false);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn reap_dead_local_sessions_never_touches_remote_or_pidless() {
        let mut state = AppState::default();
        let remote = remote_session("remote:nyx:%1", "nyx", "%1", SessionStatus::Idle);
        let mut pidless = Session::new("seeded".into(), 0, "/tmp/c".into());
        pidless.status = SessionStatus::Idle;
        state.sessions = vec![remote, pidless];
        state.reap_dead_local_sessions(|_| false);
        assert_eq!(state.sessions.len(), 2);
        assert!(state.sessions.iter().all(|s| s.status == SessionStatus::Idle));
    }

    #[test]
    fn promote_pidless_by_pane_attaches_pid_and_preserves_id() {
        let mut state = AppState::default();
        let mut p = Session::new("hook-original-id".into(), 0, "/tmp/x".into());
        p.tmux_pane = Some("%7".into());
        p.status = SessionStatus::Idle;
        state.sessions = vec![p];

        assert!(state.promote_pidless_by_pane(42, "%7"));

        let s = &state.sessions[0];
        assert_eq!(s.pid, 42);
        assert_eq!(s.id, "hook-original-id", "hook id must not be rewritten");
    }

    #[test]
    fn promote_pidless_by_pane_ignores_remote_and_pidded() {
        let mut state = AppState::default();
        let remote = remote_session("remote:nyx:%7", "nyx", "%7", SessionStatus::Idle);
        let mut pidded = Session::new("already-live".into(), 100, "/tmp/p".into());
        pidded.tmux_pane = Some("%7".into());
        pidded.status = SessionStatus::Idle;
        state.sessions = vec![remote, pidded];

        assert!(!state.promote_pidless_by_pane(42, "%7"));
        assert_eq!(state.sessions[0].pid, 0);
        assert_eq!(state.sessions[1].pid, 100);
    }

    #[test]
    fn promote_pidless_by_pane_returns_false_when_no_provisional_on_pane() {
        let mut state = AppState::default();
        let mut p = Session::new("elsewhere".into(), 0, "/tmp/x".into());
        p.tmux_pane = Some("%3".into());
        state.sessions = vec![p];

        assert!(!state.promote_pidless_by_pane(42, "%7"));
        assert_eq!(state.sessions[0].pid, 0);
    }

    #[test]
    fn pr_picker_filter_matches_title_substring_case_insensitive() {
        let mut state = AppState::default();
        state.pr_picker.prs = vec![
            mk_pr(1, "Add caching layer", "feat/cache", "alice"),
            mk_pr(2, "Fix flaky test", "fix/flaky", "bob"),
            mk_pr(3, "Refactor router", "refactor/router", "alice"),
        ];
        state.pr_picker.query = "CACHE".into();
        let nums: Vec<u32> = state.filtered_pr_picker().iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![1]);
    }

    #[test]
    fn pr_picker_filter_matches_number_prefix() {
        let mut state = AppState::default();
        state.pr_picker.prs = vec![
            mk_pr(42, "One", "a", "x"),
            mk_pr(123, "Two", "b", "y"),
            mk_pr(1234, "Three", "c", "z"),
        ];
        state.pr_picker.query = "123".into();
        let nums: Vec<u32> = state.filtered_pr_picker().iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![123, 1234]);
    }

    #[test]
    fn pr_picker_filter_matches_author_or_branch() {
        let mut state = AppState::default();
        state.pr_picker.prs = vec![
            mk_pr(1, "A", "feat/x", "alice"),
            mk_pr(2, "B", "bugfix/y", "bob"),
        ];
        state.pr_picker.query = "bob".into();
        let nums: Vec<u32> = state.filtered_pr_picker().iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![2]);

        state.pr_picker.query = "bugfix".into();
        let nums: Vec<u32> = state.filtered_pr_picker().iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![2]);
    }

    #[test]
    fn pr_picker_navigate_clamps_to_filtered_bounds() {
        let mut state = AppState::default();
        state.pr_picker.prs = vec![
            mk_pr(1, "A", "a", "x"),
            mk_pr(2, "B", "b", "y"),
            mk_pr(3, "C", "c", "z"),
        ];
        state.pr_picker.selected = 0;
        state.navigate_pr_picker(1);
        assert_eq!(state.pr_picker.selected, 1);
        state.navigate_pr_picker(1);
        assert_eq!(state.pr_picker.selected, 2);
        state.navigate_pr_picker(1);
        assert_eq!(state.pr_picker.selected, 2); // clamped
        state.navigate_pr_picker(-1);
        state.navigate_pr_picker(-1);
        state.navigate_pr_picker(-1);
        assert_eq!(state.pr_picker.selected, 0); // clamped
    }

    #[test]
    fn pr_picker_enter_returns_submit_and_clears_state() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.pr_picker.mode = true;
        state.pr_picker.cwd = Some("/tmp/repo".into());
        state.pr_picker.prs = vec![mk_pr(7, "Ship it", "feat/x", "alice")];
        state.pr_picker.selected = 0;

        let submit = state.apply_pr_picker_key(KeyCode::Enter, false);
        let submit = submit.expect("Enter on a valid row should return a submission");
        assert_eq!(submit.number, 7);
        assert_eq!(submit.title, "Ship it");
        assert_eq!(submit.cwd, "/tmp/repo");
        assert!(!state.pr_picker.mode);
        assert!(state.pr_picker.prs.is_empty());
    }

    #[test]
    fn pr_picker_esc_closes_without_submission() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.pr_picker.mode = true;
        state.pr_picker.prs = vec![mk_pr(1, "A", "a", "x")];
        assert!(state.apply_pr_picker_key(KeyCode::Esc, false).is_none());
        assert!(!state.pr_picker.mode);
        assert!(state.pr_picker.prs.is_empty());
    }

    fn mk_wt(branch: &str, path: &str) -> WtPickItem {
        WtPickItem {
            branch: branch.into(),
            path: path.into(),
            dirty: false,
            live: false,
        }
    }

    #[test]
    fn worktree_picker_filter_matches_branch_or_path() {
        let mut state = AppState::default();
        state.worktree_picker.items = vec![
            mk_wt("feat-login", "/repo/feat-login"),
            mk_wt("fix-chat", "/repo/fix-chat"),
        ];
        state.worktree_picker.query = "LOGIN".into();
        let got: Vec<&str> = state
            .filtered_worktree_picker()
            .iter()
            .map(|w| w.branch.as_str())
            .collect();
        assert_eq!(got, vec!["feat-login"]);

        state.worktree_picker.query = "fix-chat".into();
        let got: Vec<&str> = state
            .filtered_worktree_picker()
            .iter()
            .map(|w| w.path.as_str())
            .collect();
        assert_eq!(got, vec!["/repo/fix-chat"]);
    }

    #[test]
    fn worktree_picker_navigate_clamps_to_filtered_bounds() {
        let mut state = AppState::default();
        state.worktree_picker.items = vec![
            mk_wt("a", "/r/a"),
            mk_wt("b", "/r/b"),
            mk_wt("c", "/r/c"),
        ];
        state.worktree_picker.selected = 0;
        state.navigate_worktree_picker(1);
        state.navigate_worktree_picker(1);
        state.navigate_worktree_picker(1);
        assert_eq!(state.worktree_picker.selected, 2); // clamped at top
        state.navigate_worktree_picker(-1);
        state.navigate_worktree_picker(-1);
        state.navigate_worktree_picker(-1);
        assert_eq!(state.worktree_picker.selected, 0); // clamped at bottom
    }

    #[test]
    fn worktree_picker_enter_returns_submit_and_clears_state() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.worktree_picker.mode = true;
        state.worktree_picker.items = vec![mk_wt("feat-x", "/repo/feat-x")];
        state.worktree_picker.selected = 0;

        let submit = state.apply_worktree_picker_key(KeyCode::Enter, false);
        assert_eq!(submit.map(|s| s.path), Some("/repo/feat-x".to_string()));
        assert!(!state.worktree_picker.mode);
        assert!(state.worktree_picker.items.is_empty());
    }

    #[test]
    fn worktree_picker_esc_closes_without_submission() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.worktree_picker.mode = true;
        state.worktree_picker.items = vec![mk_wt("a", "/r/a")];
        assert!(state.apply_worktree_picker_key(KeyCode::Esc, false).is_none());
        assert!(!state.worktree_picker.mode);
        assert!(state.worktree_picker.items.is_empty());
    }

    #[test]
    fn visible_sessions_excludes_subagents() {
        // Subagents do not appear in the visible list — they surface as a
        // count badge on their parent (LONKO-26). The parent's subagent
        // count is reported by `subagent_count_for`.
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
        assert_eq!(ids, vec!["b1", "a1"]);
        assert_eq!(state.subagent_count_for("a1"), 1);
        assert_eq!(state.subagent_count_for("b1"), 0);
    }

    #[test]
    fn visible_sessions_inlines_subagents_when_parent_expanded() {
        let mut state = AppState::default();
        let mut a1 = main_with_repo("a1", Some("/r/alpha"));
        a1.last_activity = Instant::now();
        let mut b1 = main_with_repo("b1", Some("/r/beta"));
        b1.last_activity = Instant::now();
        let mut sub_x = Session::new("sx".into(), 0, "/tmp/a1".into());
        sub_x.parent_id = Some("a1".into());
        sub_x.depth = 1;
        sub_x.repo_root = Some("/r/alpha".into());
        let mut sub_y = Session::new("sy".into(), 0, "/tmp/a1".into());
        sub_y.parent_id = Some("a1".into());
        sub_y.depth = 1;
        sub_y.repo_root = Some("/r/alpha".into());
        state.sessions = vec![b1, sub_y, a1, sub_x];

        // Nothing expanded → subagents hidden, only mains visible.
        let ids: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b1", "a1"]);

        // Expanding a1 splices its subagents right after it.
        state.toggle_subagent_expand("a1");
        let ids: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b1", "a1", "sy", "sx"]);

        // Expanding b1 has no effect (it has no subagents).
        state.toggle_subagent_expand("b1");
        let ids: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b1", "a1", "sy", "sx"]);

        // Toggling a1 back collapses the subagents.
        state.toggle_subagent_expand("a1");
        let ids: Vec<&str> = state.visible_sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b1", "a1"]);
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
    fn toggle_tab_cycles_without_remote() {
        let mut state = AppState::default();
        assert!(!state.remote_enabled);
        assert_eq!(state.active_tab, Tab::Agents);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Sessions);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Agents); // skips Remote
    }

    #[test]
    fn toggle_tab_cycles_with_remote() {
        let mut state = AppState::default();
        state.remote_enabled = true;
        assert_eq!(state.active_tab, Tab::Agents);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Sessions);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Remote);
        state.toggle_tab();
        assert_eq!(state.active_tab, Tab::Agents);
    }

    #[test]
    fn context_max_for_model_defaults_to_200k() {
        assert_eq!(context_max_for_model("sonnet-4-6"), 200_000);
        assert_eq!(context_max_for_model("unknown"), 200_000);
        assert_eq!(context_max_for_model("claude-opus-4-6"), 1_000_000);
    }

    // ── apply_new_agent_key ─────────────────────────────────────────────────

    #[test]
    fn new_agent_open_with_cwd_starts_on_prompt() {
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        assert_eq!(state.new_agent.focus, NewAgentField::Prompt);
        assert_eq!(state.new_agent.cwd_input, ".");
        assert_eq!(state.new_agent.resolved_cwd, "/tmp");
    }

    #[test]
    fn new_agent_open_without_cwd_starts_on_cwd() {
        let mut state = AppState::default();
        state.open_new_agent(String::new());
        assert_eq!(state.new_agent.focus, NewAgentField::Cwd);
        assert!(state.new_agent.cwd_input.is_empty());
    }

    #[test]
    fn new_agent_tab_in_prompt_switches_to_cwd() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        // focus starts on Prompt (non-empty cwd)
        state.apply_new_agent_key(KeyCode::Tab, false);
        assert_eq!(state.new_agent.focus, NewAgentField::Cwd);
    }

    #[test]
    fn new_agent_tab_in_cwd_completes_path() {
        use crossterm::event::KeyCode;
        // Use a real temp dir with a known child so the test is deterministic.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("unique-child")).unwrap();
        let partial = format!("{}/uni", tmp.path().display());

        let mut state = AppState::default();
        // Directly set the cwd_input to a partial path for completion testing.
        state.new_agent.mode = true;
        state.new_agent.focus = NewAgentField::Cwd;
        state.new_agent.cwd_input = partial;
        state.apply_new_agent_key(KeyCode::Tab, false);
        assert!(state.new_agent.cwd_input.contains("unique-child"),
            "expected completion, got: {}", state.new_agent.cwd_input);
        assert_eq!(state.new_agent.focus, NewAgentField::Cwd);
    }

    #[test]
    fn new_agent_enter_on_cwd_moves_to_prompt() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        // open_new_agent with a non-empty cwd starts focus on Prompt;
        // switch to Cwd to test the Enter-on-Cwd path.
        state.new_agent.focus = NewAgentField::Cwd;
        let result = state.apply_new_agent_key(KeyCode::Enter, false);
        assert!(result.is_none());
        assert!(state.new_agent.mode); // still open
        assert_eq!(state.new_agent.focus, NewAgentField::Prompt);
    }

    #[test]
    fn new_agent_enter_on_prompt_submits_when_both_filled() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp/proj".into());
        // open_new_agent sets cwd_input="." and focus=Prompt
        state.new_agent.input = "build a thing".into();
        let result = state.apply_new_agent_key(KeyCode::Enter, false);
        // "." resolves to the stored resolved_cwd
        assert_eq!(result, Some(("build a thing".into(), "/tmp/proj".into())));
        assert!(!state.new_agent.mode);
    }

    #[test]
    fn new_agent_enter_on_prompt_with_empty_cwd_nudges_to_cwd() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent(String::new());
        state.new_agent.focus = NewAgentField::Prompt;
        state.new_agent.input = "build a thing".into();
        let result = state.apply_new_agent_key(KeyCode::Enter, false);
        assert!(result.is_none());
        assert!(state.new_agent.mode); // still open
        assert_eq!(state.new_agent.focus, NewAgentField::Cwd); // nudged
    }

    #[test]
    fn new_agent_enter_on_prompt_with_empty_prompt_stays() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        state.new_agent.focus = NewAgentField::Prompt;
        // prompt is empty
        let result = state.apply_new_agent_key(KeyCode::Enter, false);
        assert!(result.is_none());
        assert!(state.new_agent.mode); // still open
        assert_eq!(state.new_agent.focus, NewAgentField::Prompt); // stays
    }

    #[test]
    fn new_agent_esc_clears_everything() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        state.new_agent.input = "hello".into();
        state.apply_new_agent_key(KeyCode::Esc, false);
        assert!(!state.new_agent.mode);
        assert!(state.new_agent.input.is_empty());
        assert!(state.new_agent.cwd_input.is_empty());
    }

    #[test]
    fn new_agent_ctrl_c_clears_everything() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent("/tmp".into());
        state.new_agent.input = "hello".into();
        state.apply_new_agent_key(KeyCode::Char('c'), true);
        assert!(!state.new_agent.mode);
        assert!(state.new_agent.input.is_empty());
    }

    #[test]
    fn new_agent_char_routes_to_focused_field() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent(String::new());
        // Focus starts on Cwd
        state.apply_new_agent_key(KeyCode::Char('a'), false);
        assert_eq!(state.new_agent.cwd_input, "a");
        assert!(state.new_agent.input.is_empty());

        state.new_agent.focus = NewAgentField::Prompt;
        state.apply_new_agent_key(KeyCode::Char('b'), false);
        assert_eq!(state.new_agent.input, "b");
        assert_eq!(state.new_agent.cwd_input, "a"); // unchanged
    }

    #[test]
    fn new_agent_rejects_newlines() {
        use crossterm::event::KeyCode;
        let mut state = AppState::default();
        state.open_new_agent(String::new());
        state.apply_new_agent_key(KeyCode::Char('\n'), false);
        state.apply_new_agent_key(KeyCode::Char('\r'), false);
        assert!(state.new_agent.cwd_input.is_empty());
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
        assert!(state.resolve_hook_session("s1", None, None, None, None, None));
        assert_eq!(state.sessions.len(), 1);
    }

    #[test]
    fn resolve_hook_session_promotes_provisional() {
        let mut state = AppState::default();
        let mut s = session_with("tmux:1", 50, Some("%1"));
        s.status = SessionStatus::Idle;
        state.sessions.push(s);

        assert!(state.resolve_hook_session("real-id", Some("%1"), Some("/tmp"), None, None, None));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "real-id");
    }

    #[test]
    fn resolve_hook_session_evicts_stale_pane() {
        let mut state = AppState::default();
        state.sessions.push(session_with("old-id", 1, Some("%5")));
        state.sessions.push(session_with("keep", 2, Some("%6")));

        assert!(state.resolve_hook_session("new-id", Some("%5"), Some("/proj"), None, None, None));
        // old-id evicted, keep preserved, new-id created
        assert_eq!(state.sessions.len(), 2);
        assert!(state.sessions.iter().any(|s| s.id == "keep"));
        assert!(state.sessions.iter().any(|s| s.id == "new-id"));
    }

    #[test]
    fn resolve_hook_session_creates_new() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", Some("%1"), Some("/proj"), Some("/t/file"), Some("main".into()), None));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "s1");
        assert_eq!(state.sessions[0].tmux_pane.as_deref(), Some("%1"));
        assert_eq!(state.sessions[0].transcript_path.as_deref(), Some("/t/file"));
        assert_eq!(state.sessions[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn resolve_hook_session_empty_cwd_returns_false() {
        let mut state = AppState::default();
        assert!(!state.resolve_hook_session("s1", None, None, None, None, None));
        assert!(!state.resolve_hook_session("s1", None, Some(""), None, None, None));
        assert_eq!(state.sessions.len(), 0);
    }

    #[test]
    fn resolve_hook_session_no_pane_creates_without_tmux_pane() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", None, Some("/proj"), None, None, None));
        assert_eq!(state.sessions[0].tmux_pane, None);
    }

    #[test]
    fn resolve_hook_session_empty_transcript_ignored() {
        let mut state = AppState::default();
        assert!(state.resolve_hook_session("s1", None, Some("/proj"), Some(""), None, None));
        assert_eq!(state.sessions[0].transcript_path, None);
    }

    #[test]
    fn resolve_hook_session_promotes_lifecycle_provisional() {
        // A `lifecycle:<pid>` provisional (created when two Claudes in the
        // same cwd collide on session_id) must be promoted to the real
        // hook id by pane just like a `tmux:` provisional.
        let mut state = AppState::default();
        state.sessions.push(session_with("lifecycle:9999", 9999, Some("%2")));

        assert!(state.resolve_hook_session("real-id", Some("%2"), Some("/proj"), None, None, None));
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].id, "real-id");
        assert_eq!(state.sessions[0].pid, 9999, "pid should be preserved across promotion");
    }

    // ── lifecycle_session_id ───────────────────────────────────────────────

    #[test]
    fn lifecycle_session_id_returns_id_when_no_collision() {
        let state = AppState::default();
        assert_eq!(
            state.lifecycle_session_id("Sx", 100, Some("%1")),
            Some("Sx".into()),
        );
    }

    #[test]
    fn lifecycle_session_id_skips_when_pid_already_tracked() {
        let mut state = AppState::default();
        state.sessions.push(session_with("Sx", 100, Some("%1")));
        // Same pid → already tracked, drop the lifecycle event.
        assert_eq!(state.lifecycle_session_id("Sother", 100, Some("%2")), None);
    }

    #[test]
    fn lifecycle_session_id_skips_when_pane_already_tracked() {
        let mut state = AppState::default();
        state.sessions.push(session_with("Sx", 100, Some("%1")));
        // Different pid but same pane → already tracked.
        assert_eq!(state.lifecycle_session_id("Sother", 200, Some("%1")), None);
    }

    #[test]
    fn lifecycle_session_id_skips_unclaimed_provisional_with_same_id() {
        // A hook pre-created an `Sx` provisional with pid=0; this lifecycle
        // is presumably the matching pid for it. Skip — the caller has
        // already updated the pid via the earlier `find(s.id == ... && pid == 0)`
        // path; we don't want to also create a duplicate here.
        let mut state = AppState::default();
        let mut provisional = session_with("Sx", 0, Some("%1"));
        provisional.status = SessionStatus::Idle;
        state.sessions.push(provisional);

        assert_eq!(state.lifecycle_session_id("Sx", 100, Some("%2")), None);
    }

    #[test]
    fn lifecycle_session_id_creates_synthetic_id_on_collision() {
        // Two Claudes in the same cwd both fall back to the most-recent
        // transcript and resolve to the same `Sx`. The first one took
        // `Sx`; the second must NOT be dropped — it gets `lifecycle:<pid>`
        // until the hook arrives with the real id.
        let mut state = AppState::default();
        state.sessions.push(session_with("Sx", 100, Some("%1")));

        // Different pid, different pane, but id collision.
        assert_eq!(
            state.lifecycle_session_id("Sx", 200, Some("%2")),
            Some("lifecycle:200".into()),
        );
    }

    #[test]
    fn lifecycle_session_id_no_pane_still_skips_pid_dup() {
        // Pane lookup may fail (find_pane_for_pid returns None). The pid
        // check still guards against duplicates from re-fired lifecycle.
        let mut state = AppState::default();
        state.sessions.push(session_with("Sx", 100, None));
        assert_eq!(state.lifecycle_session_id("Sother", 100, None), None);
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

    // ── display_name tests ───────────────────────────────────────────────────

    #[test]
    fn display_name_falls_back_to_project_name_without_repo_root() {
        let s = Session::new("id".into(), 0, "/home/user/my-app".into());
        assert_eq!(s.display_name(), "my-app");
    }

    #[test]
    fn display_name_falls_back_to_project_name_without_branch() {
        let mut s = Session::new("id".into(), 0, "/home/user/my-app".into());
        s.repo_root = Some("/home/user/my-app".into());
        assert_eq!(s.display_name(), "my-app");
    }

    #[test]
    fn display_name_uses_branch_tail_for_grouped_session() {
        let s = main_with_repo_branch("id", "/r/lonko", "feat/toki-24/new-agent");
        assert_eq!(s.display_name(), "new-agent");
    }

    #[test]
    fn display_name_strips_group_prefix_from_branch() {
        let s = main_with_repo_branch("id", "/r/lonko", "lonko-3-new-agent");
        assert_eq!(s.display_name(), "3-new-agent");
    }

    #[test]
    fn display_name_shows_repo_name_for_trunk_branch() {
        let s = main_with_repo_branch("id", "/r/lonko", "main");
        assert_eq!(s.display_name(), "lonko");
    }

    #[test]
    fn display_name_shows_repo_name_for_master_branch() {
        let s = main_with_repo_branch("id", "/r/my-app", "master");
        assert_eq!(s.display_name(), "my-app");
    }

    #[test]
    fn display_name_no_strip_when_prefix_is_entire_tail() {
        // Branch = "lonko" and group = "lonko" — should NOT strip because
        // there's nothing after the prefix+dash.
        let s = main_with_repo_branch("id", "/r/lonko", "lonko");
        assert_eq!(s.display_name(), "lonko");
    }

    #[test]
    fn display_name_slashed_branch_with_group_prefix() {
        let s = main_with_repo_branch("id", "/r/lonko", "feat/lonko-42-fix");
        assert_eq!(s.display_name(), "42-fix");
    }

    // ── apply_hook ─────────────────────────────────────────────────────────────
    //
    // The deferred-side-effects layer in `App::handle_hook` depends on
    // `apply_hook` returning the right `HookEffect` for each combination
    // of inbound hook event and prior session state. These tests pin
    // down the contract: which payloads are dropped, when an effect is
    // produced, and which fields the caller can rely on.

    fn empty_hook_payload() -> HookPayload {
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

    fn hook_for(event: &str, session_id: &str, cwd: &str) -> HookPayload {
        HookPayload {
            hook_event_name: Some(event.into()),
            session_id: Some(session_id.into()),
            cwd: Some(cwd.into()),
            ..empty_hook_payload()
        }
    }

    #[test]
    fn apply_hook_drops_payload_without_session_id() {
        let mut state = AppState::default();
        let payload = HookPayload {
            hook_event_name: Some("UserPromptSubmit".into()),
            ..empty_hook_payload()
        };
        assert!(state.apply_hook(&payload, None).is_none());
        assert!(state.sessions.is_empty(), "no session created");
    }

    #[test]
    fn apply_hook_drops_subagent_without_agent_id() {
        let mut state = AppState::default();
        let payload = HookPayload {
            hook_event_name: Some("Notification".into()),
            session_id: Some("parent".into()),
            agent_type: Some("explorer".into()),
            // agent_id missing → drop
            ..empty_hook_payload()
        };
        assert!(state.apply_hook(&payload, None).is_none());
    }

    #[test]
    fn apply_hook_drops_unknown_event() {
        let mut state = AppState::default();
        let payload = hook_for("WeirdEvent", "s1", "/tmp/proj");
        // Seed the session so resolve_hook_session creates it, then the
        // unknown event drops in `hook_event_to_status`.
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        assert!(state.apply_hook(&payload, None).is_none());
    }

    #[test]
    fn apply_hook_user_prompt_returns_running_effect() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        let payload = HookPayload {
            prompt: Some("hello world".into()),
            ..hook_for("UserPromptSubmit", "s1", "/tmp/proj")
        };
        let effect = state.apply_hook(&payload, None).expect("effect");
        assert!(matches!(effect.status, SessionStatus::Running));
        assert!(!effect.is_now_waiting);
        assert!(effect.transcript_seed.is_none(), "non-Stop hooks omit the transcript seed");
        let s = state.sessions.iter().find(|s| s.id == "s1").unwrap();
        assert_eq!(s.last_prompt.as_deref(), Some("hello world"));
    }

    #[test]
    fn apply_hook_notification_permission_marks_waiting() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        let payload = HookPayload {
            message: Some("approve cargo build?".into()),
            notification_type: Some("permission_prompt".into()),
            ..hook_for("Notification", "s1", "/tmp/proj")
        };
        let effect = state.apply_hook(&payload, None).expect("effect");
        assert!(matches!(effect.status, SessionStatus::WaitingForUser(ref m) if m == "approve cargo build?"));
        assert!(effect.is_now_waiting, "is_now_waiting drives auto_show_panel");
    }

    #[test]
    fn apply_hook_stop_emits_transcript_seed() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        let payload = hook_for("Stop", "s1", "/tmp/proj");
        let effect = state.apply_hook(&payload, None).expect("effect");
        let seed = effect.transcript_seed.expect("Stop produces a seed");
        assert_eq!(seed.session_id, "s1");
        assert_eq!(seed.cwd, "/tmp/proj");
    }

    #[test]
    fn apply_hook_subagent_creates_inheriting_session() {
        let mut state = AppState::default();
        let mut parent = Session::new("parent".into(), 100, "/tmp/proj".into());
        parent.repo_root = Some("/tmp/proj".into());
        parent.depth = 0;
        state.sessions.push(parent);

        let payload = HookPayload {
            hook_event_name: Some("UserPromptSubmit".into()),
            session_id: Some("parent".into()),
            agent_id: Some("sub-1".into()),
            agent_type: Some("explorer".into()),
            cwd: Some("/tmp/proj".into()),
            prompt: Some("dig in".into()),
            ..empty_hook_payload()
        };
        let effect = state.apply_hook(&payload, None).expect("effect");
        assert!(matches!(effect.status, SessionStatus::Running));

        let sub = state.sessions.iter().find(|s| s.id == "sub-1").expect("subagent created");
        assert_eq!(sub.parent_id.as_deref(), Some("parent"));
        assert_eq!(sub.depth, 1, "depth = parent + 1");
        assert_eq!(sub.repo_root.as_deref(), Some("/tmp/proj"), "subagent inherits repo_root");
        assert_eq!(sub.project_name, "explorer");
    }

    #[test]
    fn apply_hook_subagent_stop_marks_completed() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("parent".into(), 100, "/tmp/proj".into()));
        // Pre-seed the subagent so the SubagentStop hits the
        // is_subagent && SubagentStop short-circuit cleanly.
        let mut sub = Session::new("sub-1".into(), 0, "/tmp/proj".into());
        sub.parent_id = Some("parent".into());
        state.sessions.push(sub);

        let payload = HookPayload {
            hook_event_name: Some("SubagentStop".into()),
            session_id: Some("parent".into()),
            agent_id: Some("sub-1".into()),
            agent_type: Some("explorer".into()),
            cwd: Some("/tmp/proj".into()),
            ..empty_hook_payload()
        };
        let effect = state.apply_hook(&payload, None).expect("effect");
        assert!(matches!(effect.status, SessionStatus::Completed));
        let sub = state.sessions.iter().find(|s| s.id == "sub-1").unwrap();
        assert!(sub.completed_at.is_some());
    }

    #[test]
    fn apply_hook_updates_cwd_and_tmux_pane() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        let payload = HookPayload {
            tmux_pane: Some("%42".into()),
            cwd: Some("/tmp/proj/sub".into()),
            ..hook_for("PostToolUse", "s1", "/tmp/proj/sub")
        };
        state.apply_hook(&payload, None).expect("effect");
        let s = state.sessions.iter().find(|s| s.id == "s1").unwrap();
        assert_eq!(s.tmux_pane.as_deref(), Some("%42"));
        assert_eq!(s.cwd, "/tmp/proj/sub");
        assert_eq!(s.project_name, "sub");
    }

    #[test]
    fn apply_hook_stamps_host_when_payload_carries_one() {
        let mut state = AppState::default();
        state.sessions.push(Session::new("s1".into(), 100, "/tmp/proj".into()));
        let payload = HookPayload {
            host: Some("kayshon".into()),
            ..hook_for("PostToolUse", "s1", "/tmp/proj")
        };
        state.apply_hook(&payload, None).expect("effect");
        let s = state.sessions.iter().find(|s| s.id == "s1").unwrap();
        assert_eq!(s.host.as_deref(), Some("kayshon"));
    }

    #[test]
    fn apply_hook_does_not_clobber_existing_host_with_local_hook() {
        let mut state = AppState::default();
        let mut s = Session::new("s1".into(), 100, "/tmp/proj".into());
        s.host = Some("kayshon".into());
        state.sessions.push(s);
        // Subsequent local-only hook (no host field) must NOT erase the
        // remote stamp — that would misroute future permission sends to
        // the local tmux server.
        let payload = hook_for("PostToolUse", "s1", "/tmp/proj");
        state.apply_hook(&payload, None).expect("effect");
        let s = state.sessions.iter().find(|s| s.id == "s1").unwrap();
        assert_eq!(s.host.as_deref(), Some("kayshon"));
    }

    #[test]
    fn pr_info_for_returns_known_pair() {
        let mut state = AppState::default();
        let mut prs = HashMap::new();
        prs.insert("feat/badge".to_string(), PrInfo { number: 42, status: PrMergeStatus::Open });
        prs.insert("fix/oops".to_string(), PrInfo { number: 7, status: PrMergeStatus::Merged });
        state.pr_infos_by_repo.insert("/repo".to_string(), prs);
        assert_eq!(
            state.pr_info_for(Some("/repo"), Some("feat/badge")),
            Some(PrInfo { number: 42, status: PrMergeStatus::Open }),
        );
        assert_eq!(
            state.pr_info_for(Some("/repo"), Some("fix/oops")),
            Some(PrInfo { number: 7, status: PrMergeStatus::Merged }),
        );
    }

    #[test]
    fn pr_info_for_is_none_when_inputs_missing() {
        let mut state = AppState::default();
        state.pr_infos_by_repo.insert(
            "/repo".to_string(),
            HashMap::from([("main".to_string(), PrInfo { number: 1, status: PrMergeStatus::Open })]),
        );
        assert_eq!(state.pr_info_for(None, Some("main")), None);
        assert_eq!(state.pr_info_for(Some("/repo"), None), None);
        assert_eq!(state.pr_info_for(None, None), None);
    }

    #[test]
    fn pr_info_for_is_none_for_unknown_repo_or_branch() {
        let mut state = AppState::default();
        state.pr_infos_by_repo.insert(
            "/repo".to_string(),
            HashMap::from([("main".to_string(), PrInfo { number: 1, status: PrMergeStatus::Open })]),
        );
        assert_eq!(state.pr_info_for(Some("/other"), Some("main")), None);
        assert_eq!(state.pr_info_for(Some("/repo"), Some("nonexistent")), None);
    }

    #[test]
    fn chat_online_offline_toggles_membership() {
        let mut state = AppState::default();
        let local: ChatKey = (None, "uuid-1".into());
        state.on_chat_online(local.clone());
        assert!(state.chat_online.contains(&local));
        // A remote agent keys independently by host.
        let remote: ChatKey = (Some("kayshon".into()), "uuid-1".into());
        state.on_chat_online(remote.clone());
        assert!(state.chat_online.contains(&remote));
        state.on_chat_offline(&local);
        assert!(!state.chat_online.contains(&local));
        assert!(state.chat_online.contains(&remote));
    }

    #[test]
    fn chat_reply_appended_increments_unread_when_view_closed() {
        let mut state = AppState::default();
        let key: ChatKey = (None, "uuid-1".into());
        state.on_chat_reply(key.clone(), "hello".into(), String::new());
        let log = state.chat_logs.get(&key).expect("log created");
        assert_eq!(log.messages.len(), 1);
        assert_eq!(log.messages[0].direction, ChatDirection::In);
        assert_eq!(log.unread, 1);
    }

    #[test]
    fn chat_reply_does_not_increment_unread_when_view_open_for_agent() {
        let mut state = AppState::default();
        let key: ChatKey = (None, "uuid-1".into());
        state.chat_view = Some(ChatView {
            key: key.clone(),
            input: String::new(),
            scroll: 0,
        });
        state.on_chat_reply(key.clone(), "hi".into(), String::new());
        assert_eq!(state.chat_logs.get(&key).unwrap().unread, 0);
        // A reply to a different agent must still bump that agent's unread.
        let other: ChatKey = (None, "uuid-2".into());
        state.on_chat_reply(other.clone(), "hi".into(), String::new());
        assert_eq!(state.chat_logs.get(&other).unwrap().unread, 1);
    }

    #[test]
    fn record_chat_send_appends_outbound_with_unique_msg_id() {
        let mut state = AppState::default();
        let key: ChatKey = (None, "uuid-1".into());
        state.tick = 5;
        let id1 = state.record_chat_send(key.clone(), "first".into());
        state.tick = 6;
        let id2 = state.record_chat_send(key.clone(), "second".into());
        assert_ne!(id1, id2);
        let log = state.chat_logs.get(&key).unwrap();
        assert_eq!(log.messages.len(), 2);
        assert!(log.messages.iter().all(|m| m.direction == ChatDirection::Out));
    }

    fn mk_remote_host(status: HostStatus, cached: Option<HostHealth>) -> RemoteHost {
        RemoteHost {
            hostname: "kayshon".into(),
            status,
            sessions: vec![],
            fail_count: 0,
            next_poll_tick: 0,
            health: HealthCache {
                health: cached,
                ..HealthCache::default()
            },
        }
    }

    #[test]
    fn effective_health_unprobed_online_is_none() {
        let host = mk_remote_host(HostStatus::Online, None);
        assert_eq!(effective_health(&host), None);
    }

    #[test]
    fn effective_health_unreachable_overrides_cache() {
        // Even a cached Healthy result is wrong when the host is unreachable.
        let host = mk_remote_host(HostStatus::Unreachable, Some(HostHealth::Healthy));
        assert_eq!(effective_health(&host), Some(HostHealth::Unreachable));
    }

    #[test]
    fn effective_health_unreachable_without_cache() {
        let host = mk_remote_host(HostStatus::Unreachable, None);
        assert_eq!(effective_health(&host), Some(HostHealth::Unreachable));
    }

    #[test]
    fn effective_health_online_returns_cached_value() {
        let host = mk_remote_host(HostStatus::Online, Some(HostHealth::Healthy));
        assert_eq!(effective_health(&host), Some(HostHealth::Healthy));

        let host = mk_remote_host(HostStatus::Online, Some(HostHealth::PluginMissing));
        assert_eq!(effective_health(&host), Some(HostHealth::PluginMissing));
    }

    #[test]
    fn host_health_ordering_matches_severity() {
        // The Ord derive must keep Unreachable < ... < Healthy so callers
        // can use min/max for "worst of two hosts" rendering decisions.
        assert!(HostHealth::Unreachable < HostHealth::PluginMissing);
        assert!(HostHealth::PluginMissing < HostHealth::ChatDead);
        assert!(
            HostHealth::ChatDead
                < HostHealth::VersionSkew {
                    remote: "0.25.0".into(),
                    local: "0.26.0".into(),
                }
        );
        assert!(
            HostHealth::VersionSkew {
                remote: "0.25.0".into(),
                local: "0.26.0".into(),
            } < HostHealth::Healthy
        );
    }
}
