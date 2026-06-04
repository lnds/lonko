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
//! Cross-host chat (v2) reuses this same router: in addition to plugin
//! connections, the router binds a second "peer" socket
//! (`lonko-chat-peer.sock`) that a remote host's `lonko chat-link` SSH
//! child connects to. The peer socket speaks `PeerFrame` (keyed by
//! `session_id`, see `sources::chat_peer`); the plugin socket speaks
//! `ChatFrame` (keyed by `ppid`). The two are bridged in `app.rs`, which
//! owns the ppid↔session_id translation. See `docs/proposals/channels.md`.

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
            Ok(ChatFrame::Online { ppid, pid: _ }) => {
                registry.insert(ppid, write_tx.clone());
                bound_ppid = Some(ppid);
                let _ = tx.send(Event::PluginOnline { ppid });
            }
            Ok(ChatFrame::Reply {
                in_reply_to,
                text,
                agent_id,
            }) => {
                let _ = tx.send(Event::PluginReply {
                    in_reply_to,
                    text,
                    agent_id,
                });
            }
            Ok(ChatFrame::Ack { msg_id, status }) => {
                let _ = tx.send(Event::PluginAck { msg_id, status });
            }
            Ok(ChatFrame::Offline { ppid }) => {
                registry.remove(ppid);
                let _ = tx.send(Event::PluginOffline { ppid });
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
        let _ = tx.send(Event::PluginOffline { ppid });
    }
    drop(write_tx);
    let _ = writer.await;
}

// ── Peer transport (cross-host chat over SSH) ───────────────────────────────

use crate::sources::chat_peer::PeerFrame;

/// Path of the second socket the router binds for peer (`chat-link`)
/// connections. Distinct from the plugin socket so the two protocols
/// (`ChatFrame` by ppid vs `PeerFrame` by session_id) never mix.
pub fn peer_socket_path() -> PathBuf {
    crate::agents::claude::config_dir().join("lonko-chat-peer.sock")
}

/// Writer handle for one connected peer (a remote host's `chat-link`).
pub type PeerWriter = mpsc::UnboundedSender<PeerFrame>;

/// Live set of connected peers. The local router broadcasts a `PeerFrame`
/// to every peer whenever a local plugin goes online/offline or replies,
/// so a remote workstation watching this host sees the same chat activity.
#[derive(Clone, Default)]
pub struct PeerHub {
    inner: Arc<Mutex<Vec<(u64, PeerWriter)>>>,
    next: Arc<std::sync::atomic::AtomicU64>,
}

impl PeerHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a peer writer; returns an id for later removal.
    pub fn add(&self, w: PeerWriter) -> u64 {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.lock().expect("peer hub mutex poisoned").push((id, w));
        id
    }

    pub fn remove(&self, id: u64) {
        self.inner.lock().expect("peer hub mutex poisoned").retain(|(i, _)| *i != id);
    }

    /// Send `frame` to every connected peer, dropping any whose channel
    /// has closed.
    pub fn broadcast(&self, frame: &PeerFrame) {
        self.inner
            .lock()
            .expect("peer hub mutex poisoned")
            .retain(|(_, w)| w.send(frame.clone()).is_ok());
    }

    #[allow(dead_code)] // used by tests and useful for debug logging
    pub fn peer_count(&self) -> usize {
        self.inner.lock().expect("peer hub mutex poisoned").len()
    }
}

/// Spawn a tokio task listening on the chat **peer** Unix socket. Each
/// accepted connection is a remote host's `chat-link` relaying chat over
/// SSH; its writer is registered in `hub` so the router can broadcast to it.
pub fn spawn_peer_listener(tx: mpsc::UnboundedSender<Event>, hub: PeerHub) -> Result<()> {
    spawn_peer_listener_at(peer_socket_path(), tx, hub)
}

pub fn spawn_peer_listener_at(
    path: PathBuf,
    tx: mpsc::UnboundedSender<Event>,
    hub: PeerHub,
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
                    let hub = hub.clone();
                    tokio::spawn(handle_peer_connection(stream, tx, hub));
                }
                Err(e) => {
                    tracing::error!("chat peer socket accept failed: {e}");
                    break;
                }
            }
        }
    });
    Ok(())
}

/// Service one peer (`chat-link`) connection: register its writer for
/// broadcasts, forward inbound `peer.send` frames to the App as
/// `Event::PeerSend`, and notify the App so it can replay the current
/// online snapshot to the freshly-connected peer.
async fn handle_peer_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::UnboundedSender<Event>,
    hub: PeerHub,
) {
    let (read_half, mut write_half) = stream.into_split();
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<PeerFrame>();

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

    let peer_id = hub.add(write_tx.clone());
    // Ask the App to replay the current online set to this new peer.
    let _ = tx.send(Event::PeerConnected);

    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<PeerFrame>(&line) {
            Ok(PeerFrame::Send { session_id, msg_id, text }) => {
                let _ = tx.send(Event::PeerSend { session_id, msg_id, text });
            }
            // Online/Offline/Reply/Ack only flow router→peer; ignore if a
            // peer ever sends them inbound.
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("chat peer: bad frame: {e}: {line}");
            }
        }
    }

    hub.remove(peer_id);
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
            Event::PluginOnline { ppid } => {
                assert_eq!(ppid, 42);
            }
            other => panic!("expected PluginOnline, got {other:?}"),
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
            Event::PluginReply { agent_id, text, in_reply_to } => {
                assert_eq!(agent_id, "42");
                assert_eq!(text, "hi user");
                assert_eq!(in_reply_to, "m1");
            }
            other => panic!("expected PluginReply, got {other:?}"),
        }

        // Closing the client side must trigger Offline + registry removal.
        drop(write_half);
        drop(reader);
        let offline = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match offline {
            Event::PluginOffline { ppid } => assert_eq!(ppid, 42),
            other => panic!("expected PluginOffline, got {other:?}"),
        }
        assert!(!registry.contains(42));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn peer_hub_add_broadcast_remove() {
        let hub = PeerHub::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<PeerFrame>();
        let id = hub.add(tx);
        assert_eq!(hub.peer_count(), 1);
        hub.broadcast(&PeerFrame::Online { session_id: "u".into() });
        assert!(matches!(rx.try_recv(), Ok(PeerFrame::Online { .. })));
        hub.remove(id);
        assert_eq!(hub.peer_count(), 0);
    }

    #[test]
    fn peer_hub_prunes_dead_writers_on_broadcast() {
        let hub = PeerHub::new();
        let (tx, rx) = mpsc::unbounded_channel::<PeerFrame>();
        hub.add(tx);
        drop(rx); // receiver gone → the next send fails
        hub.broadcast(&PeerFrame::Offline { session_id: "u".into() });
        assert_eq!(hub.peer_count(), 0, "dead writer should be pruned");
    }

    /// Peer-socket round-trip: connect as a fake `chat-link`, send a
    /// `peer.send` (→ `Event::PeerSend`), and read a broadcast frame pushed
    /// through the `PeerHub`.
    #[tokio::test]
    async fn peer_socket_send_in_and_broadcast_out() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader as TokioBufReader};
        use tokio::net::UnixStream;

        let tmp = std::env::temp_dir()
            .join(format!("lonko-chat-peer-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        let hub = PeerHub::new();
        spawn_peer_listener_at(tmp.clone(), event_tx, hub.clone()).unwrap();

        let stream = {
            let mut s = None;
            for _ in 0..20 {
                if let Ok(stream) = UnixStream::connect(&tmp).await {
                    s = Some(stream);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            s.expect("connect to peer socket")
        };
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = TokioBufReader::new(read_half).lines();

        // Connecting must emit PeerConnected so the App can replay snapshots.
        let connected = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(connected, Event::PeerConnected));

        // Inbound peer.send → Event::PeerSend.
        write_half
            .write_all(b"{\"kind\":\"peer.send\",\"session_id\":\"uuid-x\",\"msg_id\":\"m1\",\"text\":\"hi\"}\n")
            .await
            .unwrap();
        let sent = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match sent {
            Event::PeerSend { session_id, msg_id, text } => {
                assert_eq!(session_id, "uuid-x");
                assert_eq!(msg_id, "m1");
                assert_eq!(text, "hi");
            }
            other => panic!("expected PeerSend, got {other:?}"),
        }

        // Outbound: a broadcast must reach the connected peer.
        hub.broadcast(&PeerFrame::Reply {
            session_id: "uuid-x".into(),
            in_reply_to: "m1".into(),
            text: "pong".into(),
        });
        let line = reader.next_line().await.unwrap().unwrap();
        assert!(line.contains(r#""kind":"peer.reply""#));
        assert!(line.contains(r#""text":"pong""#));

        let _ = std::fs::remove_file(&tmp);
    }
}
