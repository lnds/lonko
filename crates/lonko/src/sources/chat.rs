//! Unix-socket chat router for the `lonko-channel` Claude Code plugin.
//!
//! The plugin (a Bun/Node MCP server registered as a `claude/channel`)
//! connects on startup, announces `{kind: "chat.online", ppid, pid}`,
//! and stays connected for the lifetime of its Claude Code session.
//! Lonko keys each connection by `ppid` (the PID of the parent Claude
//! Code process), which matches the `Session::pid` we already track,
//! so the local TUI can address an agent without an extra handshake.
//!
//! Frames are JSON-lines, tagged by `kind`:
//!
//! Inbound (plugin → daemon):
//!   {kind: "chat.online", ppid, pid}
//!   {kind: "chat.reply",  in_reply_to, text, agent_id}
//!   {kind: "chat.ack",    msg_id, status}
//!
//! Outbound (daemon → plugin):
//!   {kind: "chat.send", msg_id, text}
//!
//! The chat socket is intentionally separate from the hook socket
//! (`~/.claude/lonko.sock`): hook traffic is fire-and-forget, while
//! chat needs a persistent full-duplex pipe so the daemon can push
//! `chat.send` back to the plugin at any time.
//!
//! v1 is local-only. The tailnet TCP transport for cross-host chat is
//! deferred to v2 (see `docs/proposals/channels.md`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::event::Event;

pub fn socket_path() -> PathBuf {
    crate::agents::claude::config_dir().join("lonko-chat.sock")
}

/// Frames that travel on the chat socket. The `kind` field discriminates the
/// variant; serde renames it to lowercase-dotted form (`chat.send`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ChatFrame {
    #[serde(rename = "chat.send")]
    Send { msg_id: String, text: String },
    #[serde(rename = "chat.reply")]
    Reply {
        in_reply_to: String,
        text: String,
        agent_id: String,
    },
    #[serde(rename = "chat.online")]
    Online { ppid: u32, pid: u32 },
    #[serde(rename = "chat.offline")]
    Offline { ppid: u32 },
    #[serde(rename = "chat.ack")]
    Ack { msg_id: String, status: String },
}

/// Per-connection writer handle. Sending a `ChatFrame` on this channel
/// queues it for the writer task, which serializes it as JSON-lines on
/// the underlying Unix socket.
pub type Writer = mpsc::UnboundedSender<ChatFrame>;

/// Live registry of plugin connections, keyed by the parent PID
/// (the Claude Code process). Cloneable: shared between the listener
/// task and the App.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<HashMap<u32, Writer>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, ppid: u32, writer: Writer) {
        self.inner.lock().expect("registry mutex poisoned").insert(ppid, writer);
    }

    pub fn remove(&self, ppid: u32) {
        self.inner.lock().expect("registry mutex poisoned").remove(&ppid);
    }

    pub fn get(&self, ppid: u32) -> Option<Writer> {
        self.inner
            .lock()
            .expect("registry mutex poisoned")
            .get(&ppid)
            .cloned()
    }

    pub fn contains(&self, ppid: u32) -> bool {
        self.inner
            .lock()
            .expect("registry mutex poisoned")
            .contains_key(&ppid)
    }
}

/// Spawn a tokio task listening on the chat Unix socket. Each accepted
/// connection runs in its own task; the registry is shared so the TUI
/// can write back into any agent's plugin.
pub fn spawn_listener(tx: mpsc::UnboundedSender<Event>, registry: Registry) -> Result<()> {
    spawn_listener_at(socket_path(), tx, registry)
}

/// Same as `spawn_listener` but on a caller-supplied path. Used by
/// integration tests so they can bind in a tempdir without touching
/// the real socket.
pub fn spawn_listener_at(
    path: PathBuf,
    tx: mpsc::UnboundedSender<Event>,
    registry: Registry,
) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let listener = UnixListener::bind(&path)?;

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let tx = tx.clone();
                    let registry = registry.clone();
                    tokio::spawn(handle_connection(stream, tx, registry));
                }
                Err(e) => {
                    tracing::error!("chat socket accept failed: {e}");
                    break;
                }
            }
        }
    });

    Ok(())
}

/// Service one plugin connection: read frames, route to the App via
/// `Event`, and write outbound frames queued by the App through the
/// registry. The connection is removed from the registry on EOF.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::UnboundedSender<Event>,
    registry: Registry,
) {
    let (read_half, mut write_half) = stream.into_split();
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<ChatFrame>();

    // Writer task: drains the channel onto the socket as JSON-lines.
    let writer = tokio::spawn(async move {
        while let Some(frame) = write_rx.recv().await {
            let Ok(line) = serde_json::to_string(&frame) else { continue };
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if write_half.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let mut bound_ppid: Option<u32> = None;
    let mut lines = BufReader::new(read_half).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ChatFrame>(&line) {
            Ok(ChatFrame::Online { ppid, pid }) => {
                registry.insert(ppid, write_tx.clone());
                bound_ppid = Some(ppid);
                let _ = tx.send(Event::ChatOnline { ppid, pid });
            }
            Ok(ChatFrame::Reply {
                in_reply_to,
                text,
                agent_id,
            }) => {
                let _ = tx.send(Event::ChatReply {
                    in_reply_to,
                    text,
                    agent_id,
                });
            }
            Ok(ChatFrame::Ack { msg_id, status }) => {
                let _ = tx.send(Event::ChatAck { msg_id, status });
            }
            Ok(ChatFrame::Offline { ppid }) => {
                registry.remove(ppid);
                let _ = tx.send(Event::ChatOffline { ppid });
            }
            Ok(ChatFrame::Send { .. }) => {
                tracing::warn!("chat: ignoring unexpected inbound chat.send frame");
            }
            Err(e) => {
                tracing::warn!("chat: bad frame: {e}: {line}");
            }
        }
    }

    if let Some(ppid) = bound_ppid {
        registry.remove(ppid);
        let _ = tx.send(Event::ChatOffline { ppid });
    }
    drop(write_tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_frame_roundtrip() {
        let frame = ChatFrame::Online { ppid: 1234, pid: 5678 };
        let s = serde_json::to_string(&frame).unwrap();
        assert!(s.contains(r#""kind":"chat.online""#));
        assert!(s.contains(r#""ppid":1234"#));
        let parsed: ChatFrame = serde_json::from_str(&s).unwrap();
        match parsed {
            ChatFrame::Online { ppid, pid } => {
                assert_eq!(ppid, 1234);
                assert_eq!(pid, 5678);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn send_frame_serializes_with_dotted_kind() {
        let frame = ChatFrame::Send {
            msg_id: "m1".into(),
            text: "hola".into(),
        };
        let s = serde_json::to_string(&frame).unwrap();
        assert!(s.contains(r#""kind":"chat.send""#));
        assert!(s.contains(r#""text":"hola""#));
    }

    #[test]
    fn reply_frame_parses() {
        let json = r#"{"kind":"chat.reply","in_reply_to":"m1","text":"hi","agent_id":"42"}"#;
        let frame: ChatFrame = serde_json::from_str(json).unwrap();
        match frame {
            ChatFrame::Reply { in_reply_to, text, agent_id } => {
                assert_eq!(in_reply_to, "m1");
                assert_eq!(text, "hi");
                assert_eq!(agent_id, "42");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn registry_insert_and_get() {
        let reg = Registry::new();
        let (tx, _rx) = mpsc::unbounded_channel::<ChatFrame>();
        reg.insert(99, tx);
        assert!(reg.contains(99));
        assert!(reg.get(99).is_some());
        reg.remove(99);
        assert!(!reg.contains(99));
    }

    /// End-to-end integration spike: bind a listener on a tempdir socket,
    /// connect as a fake plugin, and exercise the Online / Reply / Offline
    /// flow that the real `lonko-channel` plugin will drive.
    #[tokio::test]
    async fn end_to_end_online_reply_offline() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader as TokioBufReader};
        use tokio::net::UnixStream;

        let tmp = std::env::temp_dir().join(format!("lonko-chat-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        let registry = Registry::new();
        spawn_listener_at(tmp.clone(), event_tx, registry.clone()).unwrap();

        // Listener readiness: the bind happens inline in spawn_listener_at,
        // but accept runs on a background task. Connect with a brief retry.
        let stream = {
            let mut last_err = None;
            let mut s = None;
            for _ in 0..20 {
                match UnixStream::connect(&tmp).await {
                    Ok(stream) => { s = Some(stream); break; }
                    Err(e) => {
                        last_err = Some(e);
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
            s.unwrap_or_else(|| panic!("connect failed: {last_err:?}"))
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = TokioBufReader::new(read_half).lines();

        // Send chat.online and assert Event::ChatOnline + registry insert.
        write_half
            .write_all(b"{\"kind\":\"chat.online\",\"ppid\":42,\"pid\":1001}\n")
            .await
            .unwrap();
        let online = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match online {
            Event::ChatOnline { ppid, pid } => {
                assert_eq!(ppid, 42);
                assert_eq!(pid, 1001);
            }
            other => panic!("expected ChatOnline, got {other:?}"),
        }
        assert!(registry.contains(42));

        // Daemon-side push: send a chat.send through the registry's writer
        // and read it on the client side (simulates the TUI sending a
        // message that the plugin should forward into Claude).
        let writer = registry.get(42).unwrap();
        writer
            .send(ChatFrame::Send {
                msg_id: "m1".into(),
                text: "hello agent".into(),
            })
            .unwrap();
        let pushed = reader.next_line().await.unwrap().unwrap();
        assert!(pushed.contains(r#""kind":"chat.send""#));
        assert!(pushed.contains(r#""msg_id":"m1""#));
        assert!(pushed.contains(r#""text":"hello agent""#));

        // Plugin replies (simulates Claude calling the `reply` tool).
        write_half
            .write_all(
                b"{\"kind\":\"chat.reply\",\"in_reply_to\":\"m1\",\"text\":\"hi user\",\"agent_id\":\"42\"}\n",
            )
            .await
            .unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match reply {
            Event::ChatReply { agent_id, text, in_reply_to } => {
                assert_eq!(agent_id, "42");
                assert_eq!(text, "hi user");
                assert_eq!(in_reply_to, "m1");
            }
            other => panic!("expected ChatReply, got {other:?}"),
        }

        // Closing the client side must trigger Offline + registry removal.
        drop(write_half);
        drop(reader);
        let offline = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match offline {
            Event::ChatOffline { ppid } => assert_eq!(ppid, 42),
            other => panic!("expected ChatOffline, got {other:?}"),
        }
        assert!(!registry.contains(42));

        let _ = std::fs::remove_file(&tmp);
    }
}
