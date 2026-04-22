// Manages the SSH reverse tunnel that forwards Claude hook events from a
// Tailnet host into the local lonko socket.
//
// One child `ssh -N -R` process per host, kept alive for as long as the
// host is reachable. The bridge binds `$HOME/.claude/lonko-bridge.sock`
// on the remote to `~/.claude/lonko.sock` on this machine, so the remote
// `lonko-hook --remote-tag <host>` (LONKO-48) writes straight into our
// event stream. Permission responses flow back through a regular
// `ssh host tmux send-keys` (see `control::tmux::send_keys_remote`) —
// the tunnel is one-way, the responses are out-of-band.

use std::process::{Child, Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::agents::claude;

#[derive(Debug)]
pub struct RemoteBridge {
    pub host: String,
    child: Child,
    #[allow(dead_code)]
    started_at: Instant,
}

impl RemoteBridge {
    /// Spawn the SSH child for `host`. Blocks on one preparatory SSH call
    /// to discover the remote user's login name, so prefer calling this
    /// from a blocking task when the UI must stay responsive.
    pub fn start(host: &str) -> Result<Self> {
        let remote_user = query_remote_user(host)?;
        // /tmp rather than the remote $HOME/.claude: macOS sshd's sandbox
        // refuses to bind a listening socket under the user's home dir.
        // Matches the path lonko-hook writes to when given `--remote-tag`.
        let remote_bind = format!("/tmp/lonko-bridge-{remote_user}.sock");
        let local_sock = claude::socket_path();
        let forward = format!("{}:{}", remote_bind, local_sock.display());

        // `StreamLocalBindUnlink=yes` is a client-side hint; it does not
        // reach sshd, so a stale socket left by a crashed previous bridge
        // will block the new bind. Remove it preemptively over a one-shot
        // SSH call before asking sshd to listen there.
        unlink_remote(host, &remote_bind)?;

        // `StreamLocalBindUnlink=yes` is the sshd-side cleanup of a stale
        // bound socket — without it, a crashed previous bridge leaves the
        // path dangling and the new bridge fails with "cannot bind".
        let mut child = Command::new("ssh")
            .args([
                "-N",
                "-o", "BatchMode=yes",
                "-o", "LogLevel=ERROR",
                "-o", "ServerAliveInterval=30",
                "-o", "ServerAliveCountMax=2",
                "-o", "ExitOnForwardFailure=yes",
                "-o", "StreamLocalBindUnlink=yes",
                "-R", &forward,
                host,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn bridge ssh to {host}"))?;

        // Give ssh a brief moment to fail loudly (e.g., host unreachable,
        // forward refused). If it is already dead, surface that as the
        // start error instead of returning a zombie bridge.
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Ok(Some(status)) = child.try_wait() {
            anyhow::bail!("bridge ssh to {host} exited immediately (status: {status})");
        }

        tracing::info!("remote bridge to {host} started (forward={forward})");
        Ok(Self {
            host: host.to_string(),
            child,
            started_at: Instant::now(),
        })
    }

    /// Non-blocking liveness check. Returns `false` once the child has
    /// exited for any reason.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Send SIGTERM to the child and reap it. Safe to call even if the
    /// child already exited.
    pub fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RemoteBridge {
    fn drop(&mut self) {
        self.shutdown();
        tracing::info!("remote bridge to {} dropped", self.host);
    }
}

/// Remove a stale socket on the remote side, if any. Swallows failures —
/// a successful `rm -f` on a non-existent path is fine, and if the
/// remote is unreachable the subsequent `ssh -R` will surface that.
///
/// Stdout/stderr are discarded so ssh's informational warnings
/// (post-quantum key-exchange notice in OpenSSH 10.x) do not bleed
/// onto the TUI's alternate screen.
fn unlink_remote(host: &str, remote_path: &str) -> Result<()> {
    let _ = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "LogLevel=ERROR",
            host,
            "rm", "-f", remote_path,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

/// Resolve the remote user's login name. Needed to build the `/tmp`
/// socket path that matches what `lonko-hook --remote-tag` writes to.
fn query_remote_user(host: &str) -> Result<String> {
    let output = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "LogLevel=ERROR",
            host,
            "printf", "%s", "$USER",
        ])
        .output()
        .with_context(|| format!("failed to probe $USER on {host}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "could not read $USER on {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if user.is_empty() {
        anyhow::bail!("empty $USER on {host}");
    }
    Ok(user)
}
