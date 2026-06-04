//! Remote-tailnet plumbing extracted from `app.rs`.
//!
//! Lifecycle of remote support — bridge spawning/teardown, the `remote/<host>`
//! wrapper-session protocol, snapshot reconciliation, runtime toggle —
//! lives here so `app.rs` can stay focused on event routing.

use super::{App, refresh_no_follow_sentinel_async, write_no_follow_sentinel};
use crate::control::tmux;
use crate::event::Event;
use crate::sources::chat_peer::PeerFrame;
use crate::state::{Session, SessionStatus, Tab};

// ── Free helpers ───────────────────────────────────────────────────────────────

/// Attach to the remote tmux session containing `pane_id`, following the
/// `remote/<host>` convention used by the user's tmux setup:
///
///   - A single long-lived local session named `remote/<short-host>`
///     holds an `ssh -t <host> 'tmux attach'` for reuse.
///   - Tmux hooks (`update-status-left.sh` in dotfiles) hide the local
///     status bar on `remote/*` sessions so the remote tmux's own status
///     bar shows through, making the attach visually indistinguishable
///     from a direct remote connection.
///   - Switching between remote sessions is done by telling the
///     *remote* tmux to `switch-client` via a separate ssh call,
///     rather than opening another nested tmux session locally.
///
/// This function creates the `remote/<host>` session lazily, switches
/// the local client to it, and then asks the remote tmux to move to
/// the session containing `pane_id`.
pub fn attach_remote_agent(host: &str, pane_id: &str) {
    let short = short_host(host);
    let local_session = format!("remote/{short}");
    ensure_remote_host_session(host, &local_session);

    // Suppress the follow hook for this switch-client: the `remote/<host>`
    // wrapper session already contains a nested lonko (via the ssh attach),
    // so moving the local lonko into it would stack two panels in the same
    // window. The `remote/*` guard in lonko-follow.sh covers this, and the
    // sentinel is an extra belt-and-suspenders defense against the guard
    // missing a hook due to timing. Rewrite the sentinel across a short
    // window so the second hook (`after-select-window` after
    // `client-session-changed`) still sees it.
    write_no_follow_sentinel();
    refresh_no_follow_sentinel_async();
    // Pin the switch to the most-recently-active client (`-c <client>`):
    // on multi-client setups (Ghostty tabs each with its own tmux
    // attach) tmux otherwise picks an arbitrary client and the user's
    // terminal can stay on a 1-window session, breaking `prefix N`
    // window switching afterwards.
    {
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("switch-client");
        if let Some(client) = tmux::find_main_client() {
            cmd.args(["-c", &client]);
        }
        cmd.args(["-t", &local_session]).stderr(std::process::Stdio::null());
        let _ = cmd.status();
    }

    // Ask the remote tmux to move to the session containing pane_id.
    let pane_escaped = pane_id.replace('\'', "'\\''");
    let remote_switch = format!(
        "tmux switch-client -t \"$(tmux display-message -p -F '#{{session_name}}' -t '{}')\"",
        pane_escaped,
    );
    let _ = std::process::Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "LogLevel=ERROR",
            host,
            &remote_switch,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Lowercased first DNS label of a host. `Kayshon.ts.net` → `kayshon`.
pub(super) fn short_host(host: &str) -> String {
    host.split('.').next().unwrap_or(host).to_ascii_lowercase()
}

/// Create `local_session` if it doesn't already exist. The session
/// runs a single `ssh -t <host> 'tmux attach'` — when that ssh exits
/// the local session ends too.
fn ensure_remote_host_session(host: &str, local_session: &str) {
    let has_session = std::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("={local_session}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if has_session {
        return;
    }

    // Bootstrap the remote's default `main` session if absent, then attach.
    // Matches the dotfile convention in remote-connect.sh.
    let host_escaped = host.replace('\'', "'\\''");
    let ssh_cmd = format!(
        "ssh -t '{host_escaped}' 'tmux new-session -d -s main 2>/dev/null; tmux attach -t main'"
    );
    let _ = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", local_session, &ssh_cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

// ── App methods ───────────────────────────────────────────────────────────────

impl App {
    /// Attach to the selected remote tmux session using the
    /// `remote/<host>` convention (see `attach_remote_agent` for the
    /// design rationale). Reuses the wrapper local session if already
    /// open, and tells the remote tmux to switch to the picked session
    /// via a separate ssh call.
    pub(super) fn attach_remote_session(&mut self) {
        self.mark_panel_moving();
        let Some((host, session_name)) = self.state.selected_remote_session() else { return };
        let short = short_host(host);
        let local_session = format!("remote/{short}");
        ensure_remote_host_session(host, &local_session);

        // See `attach_remote_agent`: suppress the follow script so lonko
        // stays put when we switch-client into `remote/<host>`. Pin to
        // the currently-active client so multi-client setups don't end
        // up with the wrong terminal stuck on a 1-window session.
        write_no_follow_sentinel();
        refresh_no_follow_sentinel_async();
        {
            let mut cmd = std::process::Command::new("tmux");
            cmd.arg("switch-client");
            if let Some(client) = tmux::find_main_client() {
                cmd.args(["-c", &client]);
            }
            cmd.args(["-t", &local_session]).stderr(std::process::Stdio::null());
            let _ = cmd.status();
        }

        let escaped = session_name.replace('\'', "'\\''");
        let _ = std::process::Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "LogLevel=ERROR",
                host,
                &format!("tmux switch-client -t '{escaped}'"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// Flip remote support on/off at runtime. When disabling, every
    /// remote-only artifact is torn down immediately: bridges killed,
    /// provisional remote agents dropped from the Agents list, the
    /// Remote tab's host cache cleared, the wrapper `remote/<host>`
    /// tmux sessions reaped, and the user bounced off the Remote
    /// tab if they happened to be on it. When enabling, the next
    /// on_tick discovery round wires everything back up.
    ///
    /// The choice persists across restarts via a small text file
    /// (`~/.config/lonko/remote-enabled`) that overrides the static
    /// config.toml value, so a user who toggles off on kayshon while
    /// their config says `enabled = true` stays off on reboot.
    pub(super) fn toggle_remote_support(&mut self) {
        let new_enabled = !self.state.remote_enabled;
        self.state.remote_enabled = new_enabled;
        crate::config::save_remote_enabled_override(new_enabled);

        if new_enabled {
            tracing::info!("remote support enabled (runtime toggle)");
            return;
        }

        tracing::info!("remote support disabled (runtime toggle)");

        // Capture the host set so we can tear down `remote/<host>`
        // wrapper sessions after the bridges are gone. The wrappers
        // are created lazily by `ensure_remote_host_session` and
        // outlive the SSH attach when, e.g., the bridge dies but the
        // SSH client is still hanging on a keep-alive timeout — leaving
        // empty wrapper sessions accumulating in tmux until the user
        // notices and kills them by hand.
        let hosts: Vec<String> = self
            .remote_bridges
            .keys()
            .chain(self.remote_online_hosts.iter())
            .cloned()
            .collect();

        // Kill all bridges; the map's Drop impls reap the ssh children.
        self.remote_bridges.clear();
        self.remote_bridge_starting.clear();

        // Tear down cross-host chat-links too (their Drop reaps the ssh
        // child via kill_on_drop). Remote chat state is dropped below when
        // we retain only host-less sessions.
        self.chat_links.clear();
        self.state.chat_online.retain(|(host, _)| host.is_none());
        self.state.chat_logs.retain(|(host, _), _| host.is_none());

        // Drop the Tailnet caches and Remote-tab host list.
        self.remote_online_hosts.clear();
        self.state.remote_hosts.clear();
        self.state.remote_selected = 0;

        // Tear down the `remote/<host>` wrapper tmux sessions.
        for host in hosts {
            let target = format!("remote/{}", short_host(&host));
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &target])
                .stderr(std::process::Stdio::null())
                .status();
        }

        // Remove every session that belongs to a remote host — both
        // `remote:` provisionals and hook-promoted real sessions.
        // Without this the Agents list would keep showing stale cards
        // for hosts we're no longer polling.
        self.state.sessions.retain(|s| s.host.is_none());
        let len = self.state.visible_len();
        self.state.selected = if len == 0 { 0 } else { self.state.selected.min(len - 1) };

        if self.state.active_tab == Tab::Remote {
            self.state.active_tab = Tab::Agents;
        }
    }

    /// Reconcile the live set of remote bridges against the latest
    /// Tailnet peer list. Adds bridges for new hosts, drops bridges
    /// for hosts that fell off the list or whose SSH child exited.
    /// Start attempts run on a blocking task so the SSH probe doesn't
    /// stall the UI; the resulting bridge arrives via
    /// `Event::RemoteBridgeStarted`.
    pub(super) fn sync_remote_bridges(&mut self) {
        let Some(ref tx) = self.scan_tx else { return };

        // Desired set: latest Tailnet peers, minus any explicitly excluded
        // by the user. Uses the cache populated on `RemotePeersOnline`
        // rather than `remote_hosts` (which is only filled when the
        // Remote tab has been activated).
        let desired: std::collections::HashSet<String> = self
            .remote_online_hosts
            .iter()
            .filter(|h| !self.state.excluded_hosts.contains(*h))
            .cloned()
            .collect();

        // Drop bridges for hosts that should no longer have one, and
        // reap any bridge whose SSH child has exited (so we retry next
        // cycle with a fresh spawn).
        self.remote_bridges.retain(|host, bridge| {
            if !desired.contains(host) {
                tracing::debug!("tearing down remote bridge to {host} (not online)");
                return false;
            }
            if !bridge.is_alive() {
                tracing::warn!("remote bridge to {host} exited; will retry");
                return false;
            }
            true
        });

        // Start bridges for desired hosts that don't have one yet, unless
        // a start task is already in flight.
        for host in desired {
            if self.remote_bridges.contains_key(&host)
                || self.remote_bridge_starting.contains(&host)
            {
                continue;
            }
            self.remote_bridge_starting.insert(host.clone());
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::sources::remote_bridge::RemoteBridge::start(&host)
                    .map_err(|e| e.to_string());
                let _ = tx.send(Event::RemoteBridgeStarted { host, result });
            });
        }
    }

    /// Translate a local plugin's `ppid` to its agent `session_id` by
    /// finding the local `Session` whose `pid == ppid`. `None` when no such
    /// session exists yet (the plugin announced before the hook landed).
    pub(super) fn ppid_to_session_id(&self, ppid: u32) -> Option<String> {
        self.state
            .sessions
            .iter()
            .find(|s| s.host.is_none() && s.pid == ppid)
            .map(|s| s.id.clone())
    }

    /// Reverse of `ppid_to_session_id`: the local `Session`'s `pid` for a
    /// given `session_id`, used to route an inbound `peer.send` to the
    /// right plugin connection.
    pub(super) fn session_id_to_ppid(&self, session_id: &str) -> Option<u32> {
        self.state
            .sessions
            .iter()
            .find(|s| s.host.is_none() && s.id == session_id)
            .map(|s| s.pid)
    }

    /// Try to resolve a connected local plugin's ppid to a session_id. On
    /// first success, mark the agent chat-online locally and announce it to
    /// connected peers. Idempotent: a plugin already resolved is left alone.
    pub(super) fn resolve_chat_plugin(&mut self, ppid: u32) {
        if matches!(self.chat_plugins.get(&ppid), Some(Some(_))) {
            return; // already resolved
        }
        let Some(session_id) = self.ppid_to_session_id(ppid) else { return };
        self.chat_plugins.insert(ppid, Some(session_id.clone()));
        self.state.on_chat_online((None, session_id.clone()));
        self.chat_peers.broadcast(&PeerFrame::Online { session_id });
    }

    /// Retry translation for any plugin connections still unresolved (the
    /// race where a plugin announced before its hook-created session
    /// landed). Called once per tick; cheap when there is nothing pending.
    pub(super) fn resolve_pending_chat_plugins(&mut self) {
        let pending: Vec<u32> = self
            .chat_plugins
            .iter()
            .filter(|(_, sid)| sid.is_none())
            .map(|(ppid, _)| *ppid)
            .collect();
        for ppid in pending {
            self.resolve_chat_plugin(ppid);
        }
    }

    /// Reconcile cross-host chat-links against the online host set, mirroring
    /// `sync_remote_bridges`. One `ssh <host> lonko chat-link` child per
    /// online host (parity with bridges); dead links are dropped and retried
    /// next cycle. `ChatLink::start` is non-blocking (`tokio::process`), so
    /// links are created inline rather than via a `spawn_blocking` event.
    pub(super) fn sync_chat_links(&mut self) {
        let Some(ref tx) = self.scan_tx else { return };

        let desired: std::collections::HashSet<String> = self
            .remote_online_hosts
            .iter()
            .filter(|h| !self.state.excluded_hosts.contains(*h))
            .cloned()
            .collect();

        // Drop links for hosts that are gone or whose ssh child exited.
        self.chat_links.retain(|host, link| {
            if !desired.contains(host) {
                return false;
            }
            if !link.is_alive() {
                tracing::warn!("chat-link to {host} exited; will retry");
                return false;
            }
            true
        });

        // Start links for desired hosts that don't have one yet.
        for host in desired {
            if self.chat_links.contains_key(&host) {
                continue;
            }
            match crate::sources::chat_link::ChatLink::start(&host, tx.clone()) {
                Ok(link) => {
                    self.chat_links.insert(host, link);
                }
                Err(e) => {
                    tracing::warn!("chat-link to {host} failed to start: {e}");
                }
            }
        }
    }

    /// Compute the next poll tick for a host based on its failure count.
    /// Doubles the base interval per failure, capped at 5 minutes.
    fn backoff_ticks(base_ticks: u64, fail_count: u32, current_tick: u64) -> u64 {
        let shift = fail_count.min(32);
        let delay = base_ticks
            .saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX))
            .min(3000);
        current_tick + delay
    }

    pub(super) fn on_remote_snapshot(
        &mut self,
        snapshot: crate::sources::remote_tmux::RemoteSnapshot,
    ) {
        let crate::sources::remote_tmux::RemoteSnapshot {
            host: snapshot_host,
            sessions,
            claude_panes,
            is_error,
        } = snapshot;
        let tick = self.state.tick;
        let base = self.state.remote_poll_ticks;
        if let Some(host) = self.state.remote_hosts.iter_mut().find(|h| h.hostname == snapshot_host) {
            if is_error {
                host.status = crate::state::HostStatus::Unreachable;
                host.fail_count += 1;
                host.next_poll_tick = Self::backoff_ticks(base, host.fail_count, tick);
            } else {
                host.status = crate::state::HostStatus::Online;
                host.sessions = sessions;
                host.fail_count = 0;
                host.next_poll_tick = tick + base;
            }
        } else {
            let (status, fail_count, next_poll_tick) = if is_error {
                (crate::state::HostStatus::Unreachable, 1, Self::backoff_ticks(base, 1, tick))
            } else {
                (crate::state::HostStatus::Online, 0, tick + base)
            };
            self.state.remote_hosts.push(crate::state::RemoteHost {
                hostname: snapshot_host.clone(),
                status,
                sessions,
                fail_count,
                next_poll_tick,
                health: crate::state::HealthCache::default(),
            });
            // Keep hosts sorted alphabetically.
            self.state.remote_hosts.sort_by(|a, b| {
                a.hostname.to_ascii_lowercase().cmp(&b.hostname.to_ascii_lowercase())
            });
        }

        // Seed provisional Agent entries for each pane on this host that
        // currently has a Claude process running. Hooks (when they
        // eventually fire) promote these in-place — see
        // `resolve_hook_session`'s `remote:<host>:<pane>` branch.
        //
        // Then reconcile the other way: if we have an agent for this host
        // whose pane no longer shows up in the snapshot, treat it as gone
        // (fade to Completed, then prune). Without this step, provisional
        // remote agents linger after the user kills their Claude pane.
        if !is_error {
            self.seed_remote_provisional_agents(&snapshot_host, &claude_panes);
            let live: std::collections::HashSet<&str> = claude_panes
                .iter()
                .map(|p| p.pane_id.as_str())
                .collect();
            self.state.reconcile_remote_panes(&snapshot_host, &live);
        }

        // Clamp selection.
        let count = self.state.remote_item_count();
        if count > 0 {
            self.state.remote_selected = self.state.remote_selected.min(count - 1);
        }
    }

    /// Create a provisional `remote:<host>:<pane>` session for every
    /// Claude pane reported by the latest poll, skipping any we already
    /// track (either as an unpromoted provisional or as a real hook-
    /// discovered session).
    fn seed_remote_provisional_agents(
        &mut self,
        host: &str,
        claude_panes: &[crate::sources::remote_tmux::RemoteClaudePane],
    ) {
        for pane in claude_panes {
            let provisional_id = format!("remote:{host}:{}", pane.pane_id);
            let already_tracked = self.state.sessions.iter().any(|s| {
                s.id == provisional_id
                    || (s.host.as_deref() == Some(host)
                        && s.tmux_pane.as_deref() == Some(pane.pane_id.as_str()))
            });
            if already_tracked {
                continue;
            }

            let cwd = if pane.cwd.is_empty() {
                format!("remote:{host}") // fallback; display_name needs a cwd
            } else {
                pane.cwd.clone()
            };

            let mut session = Session::new(provisional_id, 0, cwd.clone());
            session.status = SessionStatus::Idle;
            session.tmux_pane = Some(pane.pane_id.clone());
            session.host = Some(host.to_string());
            // Paths from the remote generally don't resolve against the
            // local git; fall back to the literal cwd so the session
            // still groups sensibly in the Agents list.
            session.repo_root = Some(
                crate::worktree::repo_common_root(&cwd).unwrap_or_else(|| cwd.clone()),
            );
            tracing::info!(
                "seeded provisional remote agent: host={host} pane={} cwd={cwd}",
                pane.pane_id
            );
            self.state.sessions.push(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Session;
    use tokio::sync::mpsc;

    /// The core cross-host translation: a local plugin (ppid) resolves to
    /// its session_id, marks the agent chat-online locally, and announces
    /// it to connected peers as a `PeerFrame::Online`.
    #[test]
    fn resolve_chat_plugin_translates_and_broadcasts() {
        let mut app = App::new();
        let mut s = Session::new("uuid-x".to_string(), 4242, "/tmp".to_string());
        s.host = None;
        app.state.sessions.push(s);

        let (tx, mut rx) = mpsc::unbounded_channel::<PeerFrame>();
        app.chat_peers.add(tx);

        app.chat_plugins.insert(4242, None);
        app.resolve_chat_plugin(4242);

        // Local online state keyed by (None, session_id).
        assert!(app.state.chat_online.contains(&(None, "uuid-x".to_string())));
        // Peer received the translated Online frame.
        match rx.try_recv() {
            Ok(PeerFrame::Online { session_id }) => assert_eq!(session_id, "uuid-x"),
            other => panic!("expected PeerFrame::Online, got {other:?}"),
        }
        // Both translation directions resolve.
        assert_eq!(app.session_id_to_ppid("uuid-x"), Some(4242));
        assert_eq!(app.ppid_to_session_id(4242), Some("uuid-x".to_string()));
    }

    /// A plugin that announces before its `Session` exists stays unresolved
    /// (no false online), then resolves once the session lands.
    #[test]
    fn resolve_chat_plugin_defers_until_session_exists() {
        let mut app = App::new();
        app.chat_plugins.insert(7000, None);

        app.resolve_chat_plugin(7000);
        assert!(app.state.chat_online.is_empty(), "no session yet → not online");
        assert_eq!(app.chat_plugins.get(&7000), Some(&None));

        // Session appears; the retry resolves it.
        let mut s = Session::new("uuid-late".to_string(), 7000, "/tmp".to_string());
        s.host = None;
        app.state.sessions.push(s);
        app.resolve_pending_chat_plugins();
        assert!(app.state.chat_online.contains(&(None, "uuid-late".to_string())));
    }
}
