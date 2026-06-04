//! Workstation side of cross-host chat: one long-lived
//! `ssh <host> lonko chat-link` child per remote host, relaying chat
//! frames between the remote host's chat-peer socket and this lonko.
//!
//! Modeled on `remote_bridge::RemoteBridge`, but uses `tokio::process`
//! because we continuously read the child's stdout. The child's stdout
//! carries `PeerFrame`s from the remote router (online/offline/reply/ack),
//! which a reader task turns into host-tagged `Event::Chat*`. Outbound
//! `PeerFrame::Send` frames are queued on an mpsc channel that a writer
//! task drains onto the child's stdin.
//!
//! `kill_on_drop(true)` means dropping the `ChatLink` reaps the ssh child,
//! mirroring `RemoteBridge`'s Drop. Managed by `App::sync_chat_links`.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::event::Event;
use crate::sources::chat_peer::PeerFrame;

#[derive(Debug)]
pub struct ChatLink {
    #[allow(dead_code)]
    pub host: String,
    child: Child,
    /// Queue outbound `peer.send` frames onto the child's stdin.
    tx: mpsc::UnboundedSender<PeerFrame>,
}

impl ChatLink {
    /// Spawn `ssh <host> lonko chat-link` and wire its stdio. Non-blocking:
    /// `tokio::process` spawns without a preparatory SSH round-trip (unlike
    /// `RemoteBridge`, which probes `$USER` first). Returns immediately;
    /// failures surface as the child exiting (caught by `is_alive`).
    pub fn start(host: &str, events: mpsc::UnboundedSender<Event>) -> Result<Self> {
        let mut child = Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "LogLevel=ERROR",
                // Same keepalive cadence as the hook bridge: detect a dead
                // tunnel in ~120s without poking Tailscale every 30s.
                "-o", "ServerAliveInterval=60",
                "-o", "ServerAliveCountMax=2",
                host,
                "lonko", "chat-link",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn chat-link ssh to {host}"))?;

        let mut stdin = child.stdin.take().context("chat-link child stdin missing")?;
        let stdout = child.stdout.take().context("chat-link child stdout missing")?;

        // Writer task: drain outbound PeerFrames onto the child's stdin.
        let (tx, mut rx) = mpsc::unbounded_channel::<PeerFrame>();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let Ok(line) = serde_json::to_string(&frame) else { continue };
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Reader task: parse stdout PeerFrames into host-tagged Events.
        let host_owned = host.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }
                let frame: PeerFrame = match serde_json::from_str(&line) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("chat-link {host_owned}: bad frame: {e}: {line}");
                        continue;
                    }
                };
                let host = Some(host_owned.clone());
                let event = match frame {
                    PeerFrame::Online { session_id } => Event::ChatOnline { host, session_id },
                    PeerFrame::Offline { session_id } => Event::ChatOffline { host, session_id },
                    PeerFrame::Reply { session_id, in_reply_to, text } => Event::ChatReply {
                        host,
                        session_id,
                        text,
                        in_reply_to,
                    },
                    PeerFrame::Ack { session_id, msg_id, status } => Event::ChatAck {
                        host,
                        session_id,
                        msg_id,
                        status,
                    },
                    // Send only flows workstation→remote; ignore if echoed back.
                    PeerFrame::Send { .. } => continue,
                };
                if events.send(event).is_err() {
                    break;
                }
            }
        });

        tracing::debug!("chat-link to {host} started");
        Ok(Self {
            host: host.to_string(),
            child,
            tx,
        })
    }

    /// Queue an outbound message to a remote agent on this host.
    pub fn send(&self, frame: PeerFrame) {
        let _ = self.tx.send(frame);
    }

    /// Non-blocking liveness check; `false` once the ssh child has exited.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
