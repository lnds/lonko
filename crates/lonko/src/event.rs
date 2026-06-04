use crossterm::event::{KeyEvent, MouseEvent};
use crate::sources::hooks::HookPayload;
use crate::sources::lifecycle::SessionFile;

#[derive(Debug)]
pub enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    /// A new session file appeared in ~/.claude/sessions/
    SessionDiscovered(SessionFile),
    /// A session file was removed (session ended)
    SessionRemoved(u32),
    /// A Claude Code hook event received via Unix socket
    Hook(Box<HookPayload>),
    /// A tmux pane with a running Claude process was discovered by the scanner
    TmuxPaneDiscovered { pane_id: String, claude_pid: u32, cwd: String },
    /// A tmux pane that had Claude is gone (process exited)
    TmuxPaneGone { pane_id: String },
    /// A permission response received via the control socket (y/n/w → 1/2/3)
    PermissionResponse(String),
    /// Refreshed list of local tmux sessions, computed off the main
    /// thread by a `spawn_blocking` task. Replaces the synchronous
    /// in-tick refresh that was forking ~80 tmux processes every 2s.
    TmuxSessionsRefreshed(Vec<crate::state::TmuxSession>),
    /// Result of the per-second `tmux::active_pane()` poll, computed
    /// off the main thread. Pane ID may be `None` when tmux is between
    /// states (server restart, no clients attached).
    ActivePaneRefreshed(Option<String>),
    /// Deferred result of a `transcript::read_latest` + `git_branch`
    /// pair, computed on `spawn_blocking`. Used by the Stop hook
    /// handler and the detail-view navigation refresh so the JSONL
    /// parse and the `git rev-parse` fork don't block the event loop.
    TranscriptInfoLoaded {
        session_id: String,
        info: Option<crate::sources::transcript::TranscriptInfo>,
        branch: Option<String>,
    },
    /// A snapshot of tmux sessions from a remote Tailnet host
    RemoteSnapshot(crate::sources::remote_tmux::RemoteSnapshot),
    /// The set of hostnames that were online during the latest Tailnet poll.
    /// Used to prune stale hosts that are no longer in the peer list.
    RemotePeersOnline(Vec<String>),
    /// Result of a blocking `RemoteBridge::start` call for `host`. The
    /// bridge is delivered on the success path so it can be inserted
    /// into `App::remote_bridges` back on the main task; the string
    /// carries the error message on failure.
    RemoteBridgeStarted {
        host: String,
        result: Result<crate::sources::remote_bridge::RemoteBridge, String>,
    },
    /// Result of the background `gh pr list` call kicked off when the user
    /// opens the PR picker. The picker stays in "loading…" until this event
    /// lands; on error we stash the message in `AppState::pr_picker.error`.
    PrPickerLoaded {
        cwd: String,
        result: Result<Vec<crate::state::PrPickItem>, String>,
    },
    /// Result of the background `wt list --format json` call kicked off when
    /// the user opens the worktree picker. The picker stays in "loading…"
    /// until this event lands; on error we stash the message in
    /// `AppState::worktree_picker.error`.
    WorktreePickerLoaded {
        cwd: String,
        result: Result<Vec<crate::state::WtPickItem>, String>,
    },
    /// Periodic refresh of PR info for one repo. Fired ~every 30 s per
    /// unique local `repo_root`; populates the cache used to badge agent
    /// cards with `#NNNN` (plus a blinking `M` once merged). Errors are
    /// dropped (logged), so the cache simply stays stale instead of
    /// clearing on transient `gh` failures.
    PrsByRepoRefreshed {
        repo_root: String,
        items: Vec<(String, crate::state::PrInfo)>,
    },
    // ── Local plugin socket (lonko-channel on THIS host) ──────────────
    // These carry the raw `ppid` the plugin announces; `app.rs` translates
    // ppid→session_id before touching host-aware chat state and fanning the
    // event out to connected peers.
    /// A `lonko-channel` plugin connected and announced its agent.
    /// `ppid` matches the Claude Code session's PID we already track.
    PluginOnline { ppid: u32 },
    /// The `lonko-channel` plugin closed its connection.
    PluginOffline { ppid: u32 },
    /// Claude (running on the agent side) called the channel's `reply`
    /// tool to send a message back to the lonko TUI. `agent_id` is the
    /// ppid stringified.
    PluginReply {
        agent_id: String,
        text: String,
        in_reply_to: String,
    },
    /// The plugin acknowledged a `chat.send` frame the daemon emitted.
    PluginAck { msg_id: String, status: String },

    // ── Host-aware chat (keyed by session_id; host = None for local) ───
    // Emitted both by `app.rs` after translating the local plugin events
    // above, and by the chat-link stdout reader for a remote host (with
    // `host = Some(<peer>)`). They drive `chat_online`/`chat_logs`.
    ChatOnline { host: Option<String>, session_id: String },
    ChatOffline { host: Option<String>, session_id: String },
    ChatReply {
        host: Option<String>,
        session_id: String,
        text: String,
        in_reply_to: String,
    },
    ChatAck {
        host: Option<String>,
        session_id: String,
        msg_id: String,
        status: String,
    },

    // ── Peer transport (cross-host chat over SSH) ─────────────────────
    /// A peer (another lonko, connected to this host's chat-peer socket)
    /// asked us to deliver a message to one of OUR local agents. `app.rs`
    /// translates `session_id`→ppid and pushes it to the local plugin.
    PeerSend { session_id: String, msg_id: String, text: String },
    /// A new peer connected to this host's chat-peer socket. `app.rs`
    /// replays the current local online snapshot so the peer's TUI lights
    /// up chat-capable agents even if their plugin announced before the
    /// peer link existed.
    PeerConnected,
}
