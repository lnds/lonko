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
}
