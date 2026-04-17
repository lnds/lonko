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
    pub is_error: bool,
}

/// Poll a single remote host over SSH and return its tmux sessions.
///
/// Runs a compound shell command over a single SSH connection.
/// Relies on SSH ControlMaster for connection reuse (configured in
/// the user's `~/.ssh/config`).
pub fn poll_host(host: &str) -> Result<RemoteSnapshot> {
    let script = concat!(
        "tmux list-sessions -F '#{session_name}\x01#{session_attached}\x01#{session_activity}' 2>/dev/null;",
        "echo '---WINDOWS---';",
        "tmux list-windows -a -F '#{session_name}\x01#{window_index}\x01#{window_name}\x01#{window_active}\x01#{window_panes}' 2>/dev/null;",
        "echo '---PANES---';",
        "tmux list-panes -a -F '#{session_name}\x01#{pane_pid}' 2>/dev/null;",
        "echo '---PROCS---';",
        "ps -eo pid,ppid,comm 2>/dev/null",
    );

    let output = Command::new("ssh")
        .args([
            "-o", "ConnectTimeout=5",
            "-o", "BatchMode=yes",
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
    parse_poll_output(host, &stdout)
}

// ── Parsing ─────────────────────────────────────────────────────────────────────

fn parse_poll_output(host: &str, output: &str) -> Result<RemoteSnapshot> {
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

    // 3. Parse pane→session mapping (session_name, pane_pid)
    let pane_pids: Vec<(String, u32)> = panes_block
        .lines()
        .filter_map(|line| {
            let mut p = line.splitn(2, '\x01');
            let sess = p.next()?.trim().to_string();
            let pid: u32 = p.next()?.trim().parse().ok()?;
            Some((sess, pid))
        })
        .collect();

    // 4. Parse process table and detect Claude
    let procs = parse_process_table(&procs_block);
    let claude_sessions = find_claude_sessions(&pane_pids, &procs);

    for session in &mut sessions {
        if claude_sessions.contains(&session.name) {
            session.has_claude = true;
        }
    }

    Ok(RemoteSnapshot { host: host.to_string(), sessions, is_error: false })
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

/// Walk up the process tree from each claude PID to find which pane (session)
/// it belongs to.  Returns the set of session names that have Claude running.
fn find_claude_sessions(
    pane_pids: &[(String, u32)],
    procs: &[ProcEntry],
) -> Vec<String> {
    let pane_pid_set: std::collections::HashMap<u32, &str> = pane_pids
        .iter()
        .map(|(sess, pid)| (*pid, sess.as_str()))
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

    let mut result = Vec::new();

    for claude_pid in claude_pids {
        // Walk up the process tree (max 15 levels) looking for a pane PID.
        let mut current = claude_pid;
        for _ in 0..15 {
            if let Some(session_name) = pane_pid_set.get(&current) {
                if !result.contains(&session_name.to_string()) {
                    result.push(session_name.to_string());
                }
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
main\x011000
main\x011001
work\x011100
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
        let snap = parse_poll_output("testhost", POLL_OUTPUT).unwrap();
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
        let snap = parse_poll_output("testhost", POLL_OUTPUT).unwrap();
        // claude (3000) → node (2000) → bash (1100) which is pane_pid of "work"
        assert!(!snap.sessions[0].has_claude, "main should not have claude");
        assert!(snap.sessions[1].has_claude, "work should have claude");
    }

    #[test]
    fn session_origin_is_remote() {
        let snap = parse_poll_output("testhost", POLL_OUTPUT).unwrap();
        for s in &snap.sessions {
            assert!(s.origin.is_remote());
            assert_eq!(s.origin.host_label(), "testhost");
        }
    }

    #[test]
    fn empty_tmux_returns_empty_sessions() {
        let output = "---WINDOWS---\n---PANES---\n---PROCS---\n  PID  PPID COMM\n    1     0 init\n";
        let snap = parse_poll_output("emptyhost", output).unwrap();
        assert!(snap.sessions.is_empty());
    }

    #[test]
    fn no_claude_means_no_has_claude() {
        let output = "\
dev\x010\x011713200000
---WINDOWS---
dev\x010\x01bash\x011\x011
---PANES---
dev\x015000
---PROCS---
  PID  PPID COMM
    1     0 init
 5000     1 bash
 5001  5000 vim
";
        let snap = parse_poll_output("host", output).unwrap();
        assert!(!snap.sessions[0].has_claude);
    }
}
