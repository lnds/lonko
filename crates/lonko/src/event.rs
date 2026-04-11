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
    Hook(HookPayload),
    /// A tmux pane with a running Claude process was discovered by the scanner
    TmuxPaneDiscovered { pane_id: String, claude_pid: u32, cwd: String },
    /// A tmux pane that had Claude is gone (process exited)
    TmuxPaneGone { pane_id: String },
    /// A permission response received via the control socket (y/n/w → 1/2/3)
    PermissionResponse(String),
}
