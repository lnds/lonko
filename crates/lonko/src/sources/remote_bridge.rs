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
    /// to discover the remote `$HOME`, so prefer calling this from a
    /// blocking task when the UI must stay responsive.
    pub fn start(host: &str) -> Result<Self> {
        let remote_home = query_remote_home(host)?;
        let remote_bind = format!("{remote_home}/.claude/lonko-bridge.sock");
        let local_sock = claude::socket_path();
        let forward = format!("{}:{}", remote_bind, local_sock.display());

        // `StreamLocalBindUnlink=yes` is the sshd-side cleanup of a stale
        // bound socket — without it, a crashed previous bridge leaves the
        // path dangling and the new bridge fails with "cannot bind".
        let mut child = Command::new("ssh")
            .args([
                "-N",
                "-o", "BatchMode=yes",
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

/// Resolve the remote user's `$HOME`. The value is needed at ssh-client
/// parse time for the `-R` bind path, which does not go through a remote
/// shell and therefore does not expand `$HOME`.
fn query_remote_home(host: &str) -> Result<String> {
    let output = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            host,
            "printf", "%s", "$HOME",
        ])
        .output()
        .with_context(|| format!("failed to probe $HOME on {host}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "could not read $HOME on {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        anyhow::bail!("empty $HOME on {host}");
    }
    Ok(home)
}
