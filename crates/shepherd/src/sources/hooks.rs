// Unix socket server that receives Claude Code hook events forwarded by shepherd-hook.
// Each line on the socket is a JSON object enriched with TMUX env vars.

use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

pub fn socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude")
        .join("shepherd.sock")
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    pub hook_event_name: Option<String>,
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    // User prompt text (from UserPromptSubmit hook)
    pub prompt: Option<String>,
    // Notification message and type (from Notification hook)
    pub message: Option<String>,
    pub notification_type: Option<String>,
    // Enriched by shepherd-hook
    pub tmux_pane: Option<String>,
    // Subagent fields (present when hook fires from a subagent context)
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub agent_transcript_path: Option<String>,
}

/// Spawn a tokio task listening on the Unix socket.
pub fn spawn_listener(tx: UnboundedSender<Event>) -> Result<()> {
    let path = socket_path();

    // Remove stale socket from a previous run
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    let listener = UnixListener::bind(&path)?;

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let reader = BufReader::new(stream);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if line.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<HookPayload>(&line) {
                                Ok(payload) => {
                                    let _ = tx.send(Event::Hook(payload));
                                }
                                Err(_) => {
                                    // Try parsing as a permission command: "permission <y|n|w>"
                                    if let Some(key) = parse_permission_command(&line) {
                                        let _ = tx.send(Event::PermissionResponse(key));
                                    } else {
                                        tracing::warn!("unrecognized message: {line}");
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Unix socket accept failed: {e}");
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Parse a permission command line like "permission y" → Some("1").
/// Maps y→"1" (yes), w→"2" (always), n→"3" (no).
fn parse_permission_command(line: &str) -> Option<String> {
    let mut parts = line.trim().splitn(2, ' ');
    if parts.next()? != "permission" {
        return None;
    }
    match parts.next()?.trim() {
        "y" => Some("1".into()),
        "w" => Some("2".into()),
        "n" => Some("3".into()),
        _ => None,
    }
}
