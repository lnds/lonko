// Watches ~/.claude/sessions/<pid>.json for Claude Code session lifecycle events.
// Each file contains: { pid, sessionId, cwd, startedAt, kind, entrypoint }
// File created → session started. File deleted → session ended.

use std::path::{Path, PathBuf};

use anyhow::Result;
use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::agents::claude;
use crate::event::Event;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: String,
}

fn sessions_dir() -> PathBuf {
    claude::sessions_dir()
}

fn pid_from_path(path: &Path) -> Option<u32> {
    path.file_stem()?.to_str()?.parse().ok()
}

fn read_session_file(path: &Path) -> Option<SessionFile> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Check if a PID corresponds to a running process.
fn pid_is_alive(pid: u32) -> bool {
    // kill -0 sends no signal but checks if the process exists
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Read all existing session files and emit SessionDiscovered events.
/// Stale files (dead PID) are deleted silently.
pub fn scan_existing(tx: &UnboundedSender<Event>) {
    let dir = sessions_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(session) = read_session_file(&path) {
            if !pid_is_alive(session.pid) {
                // Stale file — process is gone, clean up silently
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let _ = tx.send(Event::SessionDiscovered(session));
        }
    }
}

/// Spawn a background task that watches ~/.claude/sessions/ and sends lifecycle events.
pub fn spawn_watcher(tx: UnboundedSender<Event>) -> Result<RecommendedWatcher> {
    let dir = sessions_dir();

    let tx_notify = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<NotifyEvent>| {
        let Ok(event) = res else { return };

        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in &event.paths {
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Some(session) = read_session_file(path) {
                        let _ = tx_notify.send(Event::SessionDiscovered(session));
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in &event.paths {
                    if let Some(pid) = pid_from_path(path) {
                        let _ = tx_notify.send(Event::SessionRemoved(pid));
                    }
                }
            }
            _ => {}
        }
    })?;

    if dir.exists() {
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    }

    Ok(watcher)
}
