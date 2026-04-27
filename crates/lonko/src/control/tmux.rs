use std::process::{Command, Stdio};

use crate::agents::claude;

/// Switch the target tmux client to the target pane's session/window.
/// When `find_main_client()` returns a client name we pin the switch
/// with `-c <client>` so that on multi-client setups (e.g. several
/// Ghostty tabs each with its own tmux attach) tmux moves the client
/// the user is actually on, not whatever "best" one tmux happens to
/// pick. Without `-c`, tmux can switch the wrong client and leave the
/// user's terminal stuck on a 1-window session, which then breaks
/// `prefix N` window switching ("can't find window 2").
///
/// Swallows stderr so tmux's "can't find pane" (hit on remote pane IDs
/// that do not exist on this server) does not bleed onto the TUI's
/// alternate screen buffer.
pub fn focus_pane(pane_id: &str) -> anyhow::Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.arg("switch-client");
    if let Some(client) = find_main_client() {
        cmd.args(["-c", &client]);
    }
    cmd.args(["-t", pane_id]).stderr(Stdio::null());
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("tmux switch-client failed for pane {pane_id}");
    }
    Ok(())
}

/// Return the name of the most-recently-active tmux client that is NOT
/// attached to `lonko-tray`. Activity is `client_activity` (a Unix
/// timestamp tmux updates when the client sends input), so this picks
/// the client the user is currently interacting with — the right
/// target for `switch-client -c <client>` from inside lonko.
pub fn find_main_client() -> Option<String> {
    let output = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_name}\x01#{session_name}\x01#{client_activity}"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut best: Option<(String, u64)> = None;
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\x01');
        let client = parts.next()?.trim().to_string();
        let session = parts.next()?.trim();
        let activity: u64 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if session == "lonko-tray" {
            continue;
        }
        match &best {
            None => best = Some((client, activity)),
            Some((_, prev)) if activity > *prev => best = Some((client, activity)),
            _ => {}
        }
    }
    best.map(|(c, _)| c)
}

/// Focus a pane (select it as active). Silences stderr — see `focus_pane`.
pub fn select_pane(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["select-pane", "-t", pane_id])
        .stderr(Stdio::null())
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
        .stderr(Stdio::null())
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
        .stderr(Stdio::null())
        .status()?;

    if !status.success() {
        anyhow::bail!("tmux send-keys failed for pane {pane_id}");
    }
    Ok(())
}

/// Send keys to a tmux pane on a remote host over SSH.
///
/// Used when a permission response must reach the tmux server on the host
/// where the Claude session actually lives (LONKO-49). The local `ssh`
/// invocation builds the argv itself (no remote shell), which keeps
/// pane IDs and key strings from being re-parsed by any shell.
pub fn send_keys_remote(host: &str, pane_id: &str, keys: &str) -> anyhow::Result<()> {
    let status = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "LogLevel=ERROR",
            host,
            "tmux", "send-keys", "-t", pane_id, "-l", keys,
        ])
        .stderr(Stdio::null())
        .status()?;

    if !status.success() {
        anyhow::bail!("remote tmux send-keys failed on {host} for pane {pane_id}");
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

/// List all pane IDs in a specific tmux session (across all its windows).
pub fn list_pane_ids_in_session(session_name: &str) -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-s", "-t", session_name, "-F", "#{pane_id}"])
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

    // Find all processes whose name is exactly the agent binary (e.g. "claude").
    let Ok(output) = Command::new("pgrep").args(["-x", claude::BINARY_NAME]).output() else {
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

/// Sessions that lonko creates internally — exclude from the Sessions tab.
fn is_internal_session(name: &str) -> bool {
    name == "lonko-tray" || name.starts_with("floating-")
}

/// Switch-client to a specific window within a session. Pinned to the
/// most-recently-active client (`-c <client>`) for the same reason
/// `focus_pane` is — multi-client setups otherwise leave the wrong
/// terminal stranded on a 1-window session.
pub fn focus_session_window(session: &str, window_index: u32) -> anyhow::Result<()> {
    let target = format!("{}:{}", session, window_index);
    let mut cmd = Command::new("tmux");
    cmd.arg("switch-client");
    if let Some(client) = find_main_client() {
        cmd.args(["-c", &client]);
    }
    cmd.args(["-t", &target]).stderr(Stdio::null());
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("tmux switch-client failed for {target}");
    }
    Ok(())
}

/// List local tmux sessions, excluding lonko-internal ones.
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

/// One-shot map of `session_name → set of pane IDs` for every pane on
/// the local tmux server. Replaces per-pane `tmux_session_for_pane`
/// shell-outs in the Sessions-tab refresh: a single fork instead of
/// one per (session, pane) pair (used to be ~80 forks every 2s).
pub fn session_pane_map() -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let Ok(output) = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name}\x01#{pane_id}"])
        .output()
    else {
        return Default::default();
    };
    let mut map: std::collections::HashMap<String, std::collections::HashSet<String>> =
        Default::default();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut p = line.splitn(2, '\x01');
        if let (Some(s), Some(pane)) = (p.next(), p.next()) {
            map.entry(s.trim().to_string())
                .or_default()
                .insert(pane.trim().to_string());
        }
    }
    map
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

/// Return the tmux window ID (e.g. `@12`) that contains a given pane.
/// Window IDs are globally unique across all tmux sessions.
pub fn tmux_window_for_pane(pane_id: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{window_id}"])
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id.is_empty() { None } else { Some(id) }
}

/// Move an existing pane into a target window as a full-height right-hand
/// column at the given percentage. `-d` leaves focus where it was so this
/// doesn't steal the user's cursor. Mirrors what `lonko-follow.sh` does
/// when the hook fires, but lets the caller trigger the move proactively.
pub fn join_pane_right(src_pane: &str, target_window: &str, width_pct: u8) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args([
            "join-pane",
            "-d",
            "-h", "-f",
            "-l", &format!("{width_pct}%"),
            "-s", src_pane,
            "-t", target_window,
        ])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux join-pane failed for src={src_pane} dst={target_window}");
    }
    Ok(())
}

/// `true` when `window_id` already contains at least one pane whose
/// `pane_current_command` is `lonko`. Used by the focus path to avoid
/// stacking a second lonko sidebar next to one that's already visible
/// (e.g. an ssh pane attached to a remote whose tmux carries its own
/// lonko).
pub fn window_has_lonko_pane(window_id: &str) -> bool {
    let Ok(output) = Command::new("tmux")
        .args(["list-panes", "-t", window_id, "-F", "#{pane_current_command}"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|l| l.trim() == "lonko")
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
/// Silences stderr — see `focus_pane` for the rationale.
pub fn create_session(name: &str, cwd: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c", cwd])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux new-session failed for {name}");
    }
    Ok(())
}

/// Send Ctrl-C to a tmux pane (non-literal, so tmux interprets C-c as the real key).
/// Silences stderr — see `focus_pane` for the rationale.
pub fn send_ctrl_c(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-c"])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux send-keys C-c failed for pane {pane_id}");
    }
    Ok(())
}

/// Kill an entire tmux session by name.
/// Silences stderr — see `focus_pane` for the rationale.
pub fn kill_session(session_name: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux kill-session failed for {session_name}");
    }
    Ok(())
}

/// Kill the tmux window that contains `pane_id`.
/// Silences stderr — see `focus_pane` for the rationale.
pub fn kill_window(pane_id: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["kill-window", "-t", pane_id])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux kill-window failed for pane {pane_id}");
    }
    Ok(())
}

/// Show a message in the tmux status line. Fire-and-forget: ignores errors.
/// Silences stderr — see `focus_pane` for the rationale.
pub fn display_message(msg: &str) {
    let _ = Command::new("tmux")
        .args(["display-message", msg])
        .stderr(Stdio::null())
        .status();
}

/// Send a command to a tmux target followed by Enter.
/// Silences stderr — see `focus_pane` for the rationale.
pub fn send_command(target: &str, command: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", target, command, "Enter"])
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux send-keys failed for {target}");
    }
    Ok(())
}

