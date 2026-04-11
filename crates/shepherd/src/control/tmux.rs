use std::process::Command;

/// Switch the current tmux client to the target pane's session/window.
pub fn focus_pane(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["switch-client", "-t", pane_id])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux switch-client failed for pane {pane_id}");
    }
    Ok(())
}

/// Return the name of a tmux client that is NOT attached to shepherd-tray.
pub fn find_main_client() -> Option<String> {
    let output = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_name} #{session_name}"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(2, ' ');
        let client  = parts.next()?.to_string();
        let session = parts.next()?.trim();
        if session != "shepherd-tray" {
            return Some(client);
        }
    }
    None
}

/// Focus a pane (select it as active).
pub fn select_pane(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["select-pane", "-t", pane_id])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux select-pane failed");
    }
    Ok(())
}

/// Switch focus to the previously active pane in the current window.
pub fn select_last_pane() -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["select-pane", "-l"])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux select-pane -l failed");
    }
    Ok(())
}

/// Return the pane ID currently active in the main tmux client.
pub fn active_pane() -> Option<String> {
    let client = find_main_client()?;
    let output = Command::new("tmux")
        .args(["display-message", "-c", &client, "-p", "#{pane_id}"])
        .output()
        .ok()?;
    let pane = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if pane.is_empty() { None } else { Some(pane) }
}

/// Send keys to a tmux pane (for accepting/denying permission prompts).
/// Uses `-l` (literal) to send the key as typed text without interpreting
/// special key names, matching how a human would press the key.
pub fn send_keys(pane_id: &str, keys: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", keys])
        .status()?;

    if !status.success() {
        anyhow::bail!("tmux send-keys failed for pane {pane_id}");
    }
    Ok(())
}

/// Find the tmux pane that owns (or is an ancestor of) the given PID.
/// Queries `tmux list-panes -a` and walks the process tree upward.
pub fn find_pane_for_pid(target_pid: u32) -> Option<String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Build a map of pid -> pane_id from tmux output
    let pane_pids: Vec<(u32, &str)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, ' ');
            let pane_id = parts.next()?;
            let pid: u32 = parts.next()?.trim().parse().ok()?;
            Some((pid, pane_id))
        })
        .collect();

    // Walk up the process tree from target_pid looking for a tmux pane pid
    let mut current = target_pid;
    for _ in 0..10 {
        // Check if current pid matches any pane
        if let Some((_, pane_id)) = pane_pids.iter().find(|(p, _)| *p == current) {
            return Some(pane_id.to_string());
        }
        // Get parent pid
        current = parent_pid(current)?;
        if current <= 1 {
            break;
        }
    }
    None
}

/// Get the parent PID of a process on macOS.
fn parent_pid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()
}

/// Return all pane IDs currently active in tmux (across all sessions).
pub fn list_all_pane_ids() -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output();
    let Ok(output) = output else { return vec![] };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// A tmux pane that has a Claude Code process running in it.
#[derive(Debug)]
pub struct ClaudePaneInfo {
    pub pane_id: String,
    /// PID of the claude process itself (not the shell)
    pub claude_pid: u32,
    /// Working directory of the pane (from tmux #{pane_current_path})
    pub cwd: String,
}

/// Scan for running Claude Code processes and return the tmux panes they live in.
///
/// Strategy: find all processes named "claude" via pgrep, then map each one
/// back to a tmux pane using the existing find_pane_for_pid walk.
/// This avoids relying on #{pane_current_command} which shows the Claude version
/// string (e.g. "2.1.96") rather than "claude".
pub fn scan_claude_panes(own_pane: Option<&str>) -> Vec<ClaudePaneInfo> {
    // Build a map of pane_id → pane_current_path for fast lookup.
    let pane_paths = pane_path_map();

    // Find all processes whose name is exactly "claude".
    let Ok(output) = Command::new("pgrep").args(["-x", "claude"]).output() else {
        return vec![];
    };
    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();

    let mut result = Vec::new();
    for pid in pids {
        let Some(pane_id) = find_pane_for_pid(pid) else { continue };
        if own_pane == Some(pane_id.as_str()) {
            continue;
        }
        let cwd = pane_paths.get(&pane_id).cloned().unwrap_or_default();
        if !cwd.is_empty() {
            result.push(ClaudePaneInfo { pane_id, claude_pid: pid, cwd });
        }
    }
    result
}

// ── Session listing ─────────────────────────────────────────────────────────────

/// Sessions that shepherd creates internally — exclude from the Sessions tab.
fn is_internal_session(name: &str) -> bool {
    name == "shepherd-tray" || name.starts_with("floating-")
}

/// Switch-client to a specific window within a session.
pub fn focus_session_window(session: &str, window_index: u32) -> anyhow::Result<()> {
    let target = format!("{}:{}", session, window_index);
    let status = Command::new("tmux")
        .args(["switch-client", "-t", &target])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux switch-client failed for {target}");
    }
    Ok(())
}

/// List local tmux sessions, excluding shepherd-internal ones.
/// Returns (name, attached, last_activity_secs) per session.
pub fn list_tmux_sessions() -> Vec<crate::state::TmuxSession> {
    let Ok(output) = Command::new("tmux")
        .args(["list-sessions", "-F",
               "#{session_name}\x01#{session_attached}\x01#{session_activity}"])
        .output()
    else { return vec![] };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut sessions = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\x01');
        let Some(name) = parts.next() else { continue };
        let name = name.trim().to_string();
        if is_internal_session(&name) { continue; }

        let attached = parts.next().map(|s| s.trim() != "0").unwrap_or(false);
        let last_activity_secs: u64 = parts.next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        let windows = list_windows_for_session(&name);
        sessions.push(crate::state::TmuxSession {
            name,
            origin: crate::state::SessionOrigin::Local,
            attached,
            last_activity_secs,
            has_claude: false, // populated after by cross-referencing pane scan
            windows,
        });
    }
    sessions
}

/// List windows for a tmux session.
pub fn list_windows_for_session(session: &str) -> Vec<crate::state::TmuxWindow> {
    let Ok(output) = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F",
               "#{window_index}\x01#{window_name}\x01#{window_active}\x01#{window_panes}"])
        .output()
    else { return vec![] };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut p = line.splitn(4, '\x01');
            let index: u32 = p.next()?.trim().parse().ok()?;
            let name = p.next()?.trim().to_string();
            let active = p.next()?.trim() == "1";
            let pane_count: u32 = p.next()?.trim().parse().unwrap_or(1);
            Some(crate::state::TmuxWindow { index, name, active, pane_count })
        })
        .collect()
}

/// Return the session name that owns a given pane ID.
pub fn tmux_session_for_pane(pane_id: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{session_name}"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Return a map of pane_id → pane_current_path for all tmux panes.
fn pane_path_map() -> std::collections::HashMap<String, String> {
    let Ok(output) = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}\x01#{pane_current_path}"])
        .output()
    else {
        return Default::default();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut p = line.splitn(2, '\x01');
            let id  = p.next()?.trim().to_string();
            let path = p.next()?.trim().to_string();
            Some((id, path))
        })
        .collect()
}

/// Get the working directory of the active pane in a tmux session.
pub fn session_cwd(session_name: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", &format!("{session_name}:!"), "-p", "#{pane_current_path}"])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Create a new detached tmux session with the given name and working directory.
pub fn create_session(name: &str, cwd: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c", cwd])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux new-session failed for {name}");
    }
    Ok(())
}

/// Send Ctrl-C to a tmux pane (non-literal, so tmux interprets C-c as the real key).
pub fn send_ctrl_c(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-c"])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux send-keys C-c failed for pane {pane_id}");
    }
    Ok(())
}

/// Kill an entire tmux session by name.
pub fn kill_session(session_name: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux kill-session failed for {session_name}");
    }
    Ok(())
}

/// Send a command to a tmux target followed by Enter.
pub fn send_command(target: &str, command: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", target, command, "Enter"])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux send-keys failed for {target}");
    }
    Ok(())
}

