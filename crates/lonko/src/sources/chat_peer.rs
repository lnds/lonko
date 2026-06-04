//! Cross-host chat peer transport over SSH.
//!
//! `PeerFrame` is the wire protocol between a workstation's lonko and a
//! remote host's lonko, keyed by **session_id** (the stable Claude Code
//! session UUID) rather than the ppid the plugin protocol uses. The
//! remote host owns the ppid↔session_id translation (it has the local
//! `Session` whose `pid == ppid`), so peers only ever see session_ids.
//!
//! `run()` is the body of the `lonko chat-link` subcommand. It executes
//! **on the remote host** (spawned by the workstation as
//! `ssh <host> lonko chat-link`) and is a transparent pump: it connects to
//! the remote host's own `lonko-chat-peer.sock` and shuttles JSON-lines
//! between that socket and its stdin/stdout. The workstation reads the
//! child's stdout (router→peer frames) and writes its stdin (peer→router
//! frames). One `chat-link` child per remote host, managed like
//! `remote_bridge` (see `sources::chat_link`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Frames exchanged on the chat-peer socket / SSH stdio channel.
/// `#[serde(tag = "kind")]` → `peer.send`, `peer.online`, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PeerFrame {
    /// workstation → remote router → local plugin.
    #[serde(rename = "peer.send")]
    Send {
        session_id: String,
        msg_id: String,
        text: String,
    },
    /// remote router → workstation: a local plugin announced itself.
    #[serde(rename = "peer.online")]
    Online { session_id: String },
    /// remote router → workstation: a local plugin disconnected.
    #[serde(rename = "peer.offline")]
    Offline { session_id: String },
    /// remote router → workstation: Claude replied via the `reply` tool.
    #[serde(rename = "peer.reply")]
    Reply {
        session_id: String,
        in_reply_to: String,
        text: String,
    },
    /// remote router → workstation: the plugin acked a delivered send.
    #[serde(rename = "peer.ack")]
    Ack {
        session_id: String,
        msg_id: String,
        status: String,
    },
}

/// Body of `lonko chat-link`, run on the remote host. Connects to the
/// local chat-peer socket and pumps JSON-lines bidirectionally between it
/// and stdin/stdout until either side closes. Exits non-zero if the local
/// lonko isn't running (socket connect fails) — the workstation's
/// reconcile loop then reaps the child and retries with backoff.
pub async fn run() -> Result<()> {
    let sock_path = crate::sources::chat::peer_socket_path();
    let stream = tokio::net::UnixStream::connect(&sock_path)
        .await
        .with_context(|| format!("chat-link: cannot connect to {}", sock_path.display()))?;
    let (sock_read, mut sock_write) = stream.into_split();

    let mut sock_lines = BufReader::new(sock_read).lines();
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    loop {
        tokio::select! {
            // stdin (workstation) → socket (local router)
            line = stdin_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        sock_write.write_all(l.as_bytes()).await?;
                        sock_write.write_all(b"\n").await?;
                    }
                    _ => break, // EOF or error on stdin: workstation went away
                }
            }
            // socket (local router) → stdout (workstation)
            line = sock_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        stdout.write_all(l.as_bytes()).await?;
                        stdout.write_all(b"\n").await?;
                        stdout.flush().await?;
                    }
                    _ => break, // EOF or error on socket: local lonko went away
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_frame_roundtrips_with_dotted_kind() {
        let f = PeerFrame::Send {
            session_id: "uuid-1".into(),
            msg_id: "m3".into(),
            text: "hola".into(),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains(r#""kind":"peer.send""#));
        assert!(s.contains(r#""session_id":"uuid-1""#));
        let back: PeerFrame = serde_json::from_str(&s).unwrap();
        matches!(back, PeerFrame::Send { .. }).then_some(()).unwrap();
    }

    #[test]
    fn online_offline_reply_ack_roundtrip() {
        for f in [
            PeerFrame::Online { session_id: "u".into() },
            PeerFrame::Offline { session_id: "u".into() },
            PeerFrame::Reply {
                session_id: "u".into(),
                in_reply_to: "m1".into(),
                text: "hi".into(),
            },
            PeerFrame::Ack {
                session_id: "u".into(),
                msg_id: "m1".into(),
                status: "delivered".into(),
            },
        ] {
            let s = serde_json::to_string(&f).unwrap();
            let back: PeerFrame = serde_json::from_str(&s).unwrap();
            // kind tag survives the round-trip
            assert_eq!(
                std::mem::discriminant(&f),
                std::mem::discriminant(&back)
            );
        }
    }
}
