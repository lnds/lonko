// shepherd focus <n>
// Switches the tmux client to the Nth Claude session tracked by shepherd.
// The session order is written by the running shepherd instance to
// ~/.cache/shepherd-sessions (one pane_id per line, 1-indexed).

use std::process::Command;

fn sessions_cache_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("shepherd-sessions")
}

fn tmux_msg(msg: &str) {
    let _ = Command::new("tmux")
        .args(["display-message", msg])
        .status();
}

pub fn run(n: usize) -> anyhow::Result<()> {
    if n == 0 {
        tmux_msg("shepherd: position must be 1-9");
        anyhow::bail!("position must be 1-9");
    }

    let path = sessions_cache_path();

    let content = match std::fs::read_to_string(&path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => {
            tmux_msg("shepherd: not running or no sessions");
            anyhow::bail!("shepherd sessions cache not available");
        }
    };

    let line = content.lines().nth(n - 1).map(str::trim).unwrap_or("");
    let pane_id = if line.is_empty() {
        tmux_msg(&format!("shepherd: session {n} not ready yet"));
        anyhow::bail!("session {n} has no pane yet");
    } else {
        line.to_string()
    };

    // switch-client changes the active tmux session, but if we're already in
    // that session it's a no-op and won't switch windows. select-pane always
    // focuses the exact pane (and its window) regardless of current session.
    let switch = Command::new("tmux")
        .args(["switch-client", "-t", &pane_id])
        .status();
    let select = Command::new("tmux")
        .args(["select-pane", "-t", &pane_id])
        .status();

    match (switch, select) {
        (Ok(s), _) if s.success() => Ok(()),
        (_, Ok(s)) if s.success() => Ok(()),
        _ => {
            tmux_msg(&format!("shepherd: session {n} pane gone"));
            anyhow::bail!("tmux switch-client failed for pane {pane_id}");
        }
    }
}

/// Path where shepherd writes the ordered session list.
pub fn cache_path() -> std::path::PathBuf {
    sessions_cache_path()
}
