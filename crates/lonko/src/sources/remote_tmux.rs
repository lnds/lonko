// Polls tmux sessions on remote Tailnet hosts over SSH.
//
// Each poll runs a single compound SSH command that collects sessions,
// windows, pane PIDs, and the process table in one round-trip.  The
// output is parsed locally to build `TmuxSession` structs and detect
// which sessions are running Claude.

use std::process::Command;

use anyhow::{Context, Result};

use crate::agents::claude;
use crate::state::{SessionOrigin, TmuxSession, TmuxWindow};

/// Everything we learn about a remote host in a single poll.
#[derive(Debug, Clone)]
pub struct RemoteSnapshot {
    pub host: String,
    pub sessions: Vec<TmuxSession>,
    /// One entry per tmux pane on the host that is currently running a
    /// Claude Code process. Used to pre-populate provisional Agent
    /// cards so remote sessions show up in the Agents tab immediately,
    /// without having to wait for the first hook event.
    pub claude_panes: Vec<RemoteClaudePane>,
    pub is_error: bool,
}

/// Subset of a remote tmux pane that's enough to seed a provisional
/// remote Session entry. `cwd` is `pane_current_path` at poll time —
/// it's what the shell (or claude) has `cd`'d into on the host, which
/// matches what a hook payload's `cwd` field would carry.
#[derive(Debug, Clone)]
pub struct RemoteClaudePane {
    pub pane_id: String,
    pub cwd: String,
}

/// Poll a single remote host over SSH and return its tmux sessions.
///
/// Runs a compound shell command over a single SSH connection.
/// We force `ControlMaster=auto` with a private socket path so polls
/// reuse a single TCP/auth handshake even when the user's
/// `~/.ssh/config` does not configure ControlMaster. Each fresh
/// handshake otherwise adds Tailscale Network-Extension churn that
/// adds up quickly across multiple peers.
pub fn poll_host(host: &str) -> Result<RemoteSnapshot> {
    let script = concat!(
        "tmux list-sessions -F '#{session_name}\x01#{session_attached}\x01#{session_activity}' 2>/dev/null;",
        "echo '---WINDOWS---';",
        "tmux list-windows -a -F '#{session_name}\x01#{window_index}\x01#{window_name}\x01#{window_active}\x01#{window_panes}' 2>/dev/null;",
        "echo '---PANES---';",
        "tmux list-panes -a -F '#{session_name}\x01#{pane_id}\x01#{pane_pid}\x01#{pane_current_path}' 2>/dev/null;",
        "echo '---PROCS---';",
        "ps -eo pid,ppid,comm 2>/dev/null",
    );

    // Per-user socket dir; overridable via $TMPDIR. `%h` lets one master
    // serve all polls for the same host, while `%p` keeps non-default
    // ports on their own socket.
    let mux_dir = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    let mux_dir = mux_dir.trim_end_matches('/');
    let control_path = format!("{mux_dir}/lonko-ssh-mux-%r@%h:%p");

    let output = Command::new("ssh")
        .args([
            "-o", "ConnectTimeout=5",
            "-o", "BatchMode=yes",
            "-o", "LogLevel=ERROR",
            "-o", "ControlMaster=auto",
            "-o", &format!("ControlPath={control_path}"),
            "-o", "ControlPersist=600",
            host,
            script,
        ])
        .output()
        .with_context(|| format!("failed to ssh to {host}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "ssh to {host} failed: {}",
            String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("unknown error")
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_poll_output(host, &stdout))
}

// ── Parsing ─────────────────────────────────────────────────────────────────────

fn parse_poll_output(host: &str, output: &str) -> RemoteSnapshot {
    let mut sessions_block = String::new();
    let mut windows_block = String::new();
    let mut panes_block = String::new();
    let mut procs_block = String::new();

    let mut section = 0; // 0=sessions, 1=windows, 2=panes, 3=procs
    for line in output.lines() {
        match line.trim() {
            "---WINDOWS---" => { section = 1; continue; }
            "---PANES---"   => { section = 2; continue; }
            "---PROCS---"   => { section = 3; continue; }
            _ => {}
        }
        let block = match section {
            0 => &mut sessions_block,
            1 => &mut windows_block,
            2 => &mut panes_block,
            _ => &mut procs_block,
        };
        block.push_str(line);
        block.push('\n');
    }

    let origin = SessionOrigin::Remote { host: host.to_string() };

    // 1. Parse sessions
    let mut sessions: Vec<TmuxSession> = sessions_block
        .lines()
        .filter_map(|line| {
            let mut p = line.splitn(3, '\x01');
            let name = p.next()?.trim().to_string();
            if name.is_empty() { return None; }
            let attached = p.next().is_some_and(|s| s.trim() != "0");
            let last_activity_secs: u64 = p.next()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            Some(TmuxSession {
                name,
                origin: origin.clone(),
                attached,
                last_activity_secs,
                has_claude: false,
                windows: vec![],
            })
        })
        .collect();

    // 2. Parse windows and attach to sessions
    for line in windows_block.lines() {
        let mut p = line.splitn(5, '\x01');
        let Some(sess_name) = p.next().map(|s| s.trim()) else { continue };
        let Some(index) = p.next().and_then(|s| s.trim().parse::<u32>().ok()) else { continue };
        let name = p.next().unwrap_or("").trim().to_string();
        let active = p.next().is_some_and(|s| s.trim() == "1");
        let pane_count: u32 = p.next().and_then(|s| s.trim().parse().ok()).unwrap_or(1);

        if let Some(session) = sessions.iter_mut().find(|s| s.name == sess_name) {
            session.windows.push(TmuxWindow { index, name, active, pane_count });
        }
    }

    // 3. Parse pane rows (session_name, pane_id, pane_pid, pane_current_path).
    let panes: Vec<PaneRow> = panes_block
        .lines()
        .filter_map(|line| {
            let mut p = line.splitn(4, '\x01');
            let session_name = p.next()?.trim().to_string();
            let pane_id = p.next()?.trim().to_string();
            let pane_pid: u32 = p.next()?.trim().parse().ok()?;
            let cwd = p.next().unwrap_or("").trim().to_string();
            if pane_id.is_empty() {
                return None;
            }
            Some(PaneRow { session_name, pane_id, pane_pid, cwd })
        })
        .collect();

    // 4. Walk the process table to map each Claude PID back to the pane
    // that owns it. Sessions with a matching pane are tagged `has_claude`;
    // the matching panes themselves surface as `RemoteClaudePane` entries
    // so the caller can seed provisional Agent cards from them.
    let procs = parse_process_table(&procs_block);
    let claude_panes = find_claude_panes(&panes, &procs);

    let claude_session_names: std::collections::HashSet<&str> = claude_panes
        .iter()
        .filter_map(|(pane_id, _cwd)| {
            panes
                .iter()
                .find(|p| p.pane_id == *pane_id)
                .map(|p| p.session_name.as_str())
        })
        .collect();

    for session in &mut sessions {
        if claude_session_names.contains(session.name.as_str()) {
            session.has_claude = true;
        }
    }

    let claude_panes = claude_panes
        .into_iter()
        .map(|(pane_id, cwd)| RemoteClaudePane { pane_id, cwd })
        .collect();

    RemoteSnapshot {
        host: host.to_string(),
        sessions,
        claude_panes,
        is_error: false,
    }
}

/// One row from `tmux list-panes -a`.
struct PaneRow {
    session_name: String,
    pane_id: String,
    pane_pid: u32,
    cwd: String,
}

/// Parsed process: (pid, ppid, comm).
struct ProcEntry {
    pid: u32,
    ppid: u32,
    comm: String,
}

fn parse_process_table(block: &str) -> Vec<ProcEntry> {
    block.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let ppid: u32 = parts.next()?.parse().ok()?;
            let comm = parts.next()?.to_string();
            Some(ProcEntry { pid, ppid, comm })
        })
        .collect()
}

/// Walk the process tree from each Claude PID back to the enclosing
/// tmux pane. Returns `(pane_id, cwd)` for every pane that currently
/// contains a Claude process, deduplicated — multiple Claude processes
/// in the same pane collapse to one entry.
fn find_claude_panes(
    panes: &[PaneRow],
    procs: &[ProcEntry],
) -> Vec<(String, String)> {
    let by_pid: std::collections::HashMap<u32, &PaneRow> = panes
        .iter()
        .map(|p| (p.pane_pid, p))
        .collect();

    let ppid_map: std::collections::HashMap<u32, u32> = procs
        .iter()
        .map(|p| (p.pid, p.ppid))
        .collect();

    let claude_pids: Vec<u32> = procs
        .iter()
        .filter(|p| p.comm == claude::BINARY_NAME)
        .map(|p| p.pid)
        .collect();

    let mut result: Vec<(String, String)> = Vec::new();
    let mut seen_panes: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for claude_pid in claude_pids {
        // Walk up the process tree (max 15 levels) looking for a pane PID.
        let mut current = claude_pid;
        for _ in 0..15 {
            if let Some(pane) = by_pid.get(&current)
                && seen_panes.insert(pane.pane_id.clone())
            {
                result.push((pane.pane_id.clone(), pane.cwd.clone()));
                break;
            }
            match ppid_map.get(&current) {
                Some(&parent) if parent != 0 && parent != current => current = parent,
                _ => break,
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL_OUTPUT: &str = "\
main\x010\x011713200000
work\x011\x011713200100
---WINDOWS---
main\x010\x01bash\x011\x011
main\x011\x01vim\x010\x011
work\x010\x01dev\x011\x012
---PANES---
main\x01%0\x011000\x01/home/u/main
main\x01%1\x011001\x01/home/u/vim
work\x01%2\x011100\x01/home/u/work
---PROCS---
  PID  PPID COMM
    1     0 init
 1000     1 bash
 1001     1 bash
 1100     1 bash
 2000  1100 node
 3000  2000 claude
 4000  1001 vim
";

    #[test]
    fn parses_sessions_with_windows() {
        let snap = parse_poll_output("testhost", POLL_OUTPUT);
        assert_eq!(snap.host, "testhost");
        assert_eq!(snap.sessions.len(), 2);

        let main = &snap.sessions[0];
        assert_eq!(main.name, "main");
        assert!(!main.attached);
        assert_eq!(main.windows.len(), 2);
        assert_eq!(main.windows[0].name, "bash");
        assert!(main.windows[0].active);
        assert_eq!(main.windows[1].name, "vim");
        assert!(!main.windows[1].active);

        let work = &snap.sessions[1];
        assert_eq!(work.name, "work");
        assert!(work.attached);
        assert_eq!(work.windows.len(), 1);
        assert_eq!(work.windows[0].pane_count, 2);
    }

    #[test]
    fn detects_claude_in_correct_session() {
        let snap = parse_poll_output("testhost", POLL_OUTPUT);
        // claude (3000) → node (2000) → bash (1100) which is pane_pid of "work"
        assert!(!snap.sessions[0].has_claude, "main should not have claude");
        assert!(snap.sessions[1].has_claude, "work should have claude");
    }

    #[test]
    fn session_origin_is_remote() {
        let snap = parse_poll_output("testhost", POLL_OUTPUT);
        for s in &snap.sessions {
            assert!(s.origin.is_remote());
            assert_eq!(s.origin.host_label(), "testhost");
        }
    }

    #[test]
    fn empty_tmux_returns_empty_sessions() {
        let output = "---WINDOWS---\n---PANES---\n---PROCS---\n  PID  PPID COMM\n    1     0 init\n";
        let snap = parse_poll_output("emptyhost", output);
        assert!(snap.sessions.is_empty());
    }

    #[test]
    fn no_claude_means_no_has_claude() {
        let output = "\
dev\x010\x011713200000
---WINDOWS---
dev\x010\x01bash\x011\x011
---PANES---
dev\x01%0\x015000\x01/home/u/dev
---PROCS---
  PID  PPID COMM
    1     0 init
 5000     1 bash
 5001  5000 vim
";
        let snap = parse_poll_output("host", output);
        assert!(!snap.sessions[0].has_claude);
        assert!(snap.claude_panes.is_empty());
    }

    #[test]
    fn exposes_claude_pane_id_and_cwd() {
        let snap = parse_poll_output("testhost", POLL_OUTPUT);
        // Claude (pid 3000) lives in pane %2 (pane_pid 1100) in session "work".
        assert_eq!(snap.claude_panes.len(), 1);
        let pane = &snap.claude_panes[0];
        assert_eq!(pane.pane_id, "%2");
        assert_eq!(pane.cwd, "/home/u/work");
    }
}
