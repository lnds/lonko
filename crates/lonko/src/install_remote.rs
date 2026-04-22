// lonko install-remote <host>
//
// Provisions a Tailnet host so it can forward Claude Code hooks to lonko
// once the reverse-tunnel bridge lands (LONKO-49). Two steps:
//
//   1. Install the `lonko-hook` binary on the host (via `cargo install`,
//      pinned to this build's version).
//   2. Merge the hook commands into the host's `~/.claude/settings.json`,
//      using the same JSON logic as the local installer.
//
// Assumes SSH connectivity and a working Rust toolchain on the remote.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::install::{merge_hooks_into, MergeStatus, HOOK_EVENTS};

/// `cargo install` target — the published git repository.
const REPO_URL: &str = "https://github.com/lnds/lonko.git";

/// Remote path where Claude looks for user-level settings.
const REMOTE_SETTINGS_PATH: &str = "$HOME/.claude/settings.json";

/// Remote path where `cargo install` drops the binary.
/// Written out as a shell-expandable string because it is sent through SSH.
const REMOTE_HOOK_BIN: &str = "$HOME/.cargo/bin/lonko-hook";

pub fn run(host: &str) -> Result<()> {
    if host.is_empty() {
        anyhow::bail!("usage: lonko install-remote <host>");
    }

    println!("==> Checking SSH connectivity to {host}...");
    check_ssh(host)?;
    println!("    OK");

    println!("\n==> Checking Rust toolchain on {host}...");
    let cargo_version = remote_cargo_version(host)?;
    println!("    {cargo_version}");

    let version = env!("CARGO_PKG_VERSION");
    println!("\n==> Installing lonko-hook v{version} on {host}...");
    install_hook_binary(host, version)?;
    println!("    lonko-hook installed to {REMOTE_HOOK_BIN}");

    println!("\n==> Configuring Claude hooks in {host}:~/.claude/settings.json...");
    configure_hooks(host)?;

    println!("\nDone. {host} is ready for lonko remote support.");
    println!("(Hooks will only flow once the SSH bridge lands — see LONKO-49.)");
    Ok(())
}

/// Build the hook command string as it will appear in the remote settings.json.
///
/// Includes `--remote-tag <host>` so that once lonko-hook learns the flag
/// (LONKO-48) it can stamp events with the source host. Today's lonko-hook
/// ignores unknown args, so including the flag now is safe and avoids a
/// second pass later.
fn hook_command_for(host: &str) -> String {
    format!("{REMOTE_HOOK_BIN} --remote-tag {host}")
}

/// Run `ssh -o BatchMode=yes -o ConnectTimeout=5 <host> true`.
/// BatchMode disables password prompts so we fail fast instead of hanging.
fn check_ssh(host: &str) -> Result<()> {
    let status = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            host,
            "true",
        ])
        .status()
        .with_context(|| format!("failed to spawn ssh to {host}"))?;

    if !status.success() {
        anyhow::bail!(
            "cannot reach {host} over SSH (ensure it is online, that `ssh {host}` \
             works without a password prompt, and that BatchMode-compatible auth is set up)"
        );
    }
    Ok(())
}

/// Ask the remote for its cargo version. Fails loudly if Rust is missing.
fn remote_cargo_version(host: &str) -> Result<String> {
    let output = Command::new("ssh")
        .args([host, "cargo --version"])
        .output()
        .with_context(|| format!("failed to run cargo --version on {host}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "no Rust toolchain on {host}: `cargo --version` failed. \
             Install rustup on the host first (https://rustup.rs) and try again."
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Invoke `cargo install --git <repo> --tag v<version> lonko-hook` on the remote.
///
/// Pinning to the local build's version keeps both ends on the same wire
/// format. Streams cargo's output to the user so a long build doesn't look
/// like a hang.
fn install_hook_binary(host: &str, version: &str) -> Result<()> {
    let remote_cmd = format!(
        "cargo install --git {REPO_URL} --tag v{version} --locked lonko-hook"
    );
    let status = Command::new("ssh")
        .args([host, &remote_cmd])
        .status()
        .with_context(|| format!("failed to run cargo install on {host}"))?;

    if !status.success() {
        anyhow::bail!("cargo install failed on {host}");
    }
    Ok(())
}

/// Read the remote settings.json, merge the hook entries, write it back.
fn configure_hooks(host: &str) -> Result<()> {
    let existing = read_remote_settings(host)?;
    let cmd = hook_command_for(host);

    let (updated, statuses) = merge_hooks_into(existing, &cmd)?;

    for (event, status) in HOOK_EVENTS.iter().zip(statuses.iter()) {
        match status {
            MergeStatus::Added => println!("  {event}: added"),
            MergeStatus::AlreadyConfigured => println!("  {event}: already configured"),
        }
    }

    let payload = serde_json::to_string_pretty(&updated)? + "\n";
    write_remote_settings(host, &payload)?;

    println!("\n    Using: {cmd}");
    Ok(())
}

/// `ssh host "mkdir -p ~/.claude && [ -f settings.json ] && cat settings.json || echo '{}'"`.
///
/// Returns an empty JSON object if the file does not exist on the remote, so
/// that first-time installs work without special casing.
fn read_remote_settings(host: &str) -> Result<Value> {
    let remote_cmd = format!(
        "mkdir -p $HOME/.claude && \
         if [ -f {REMOTE_SETTINGS_PATH} ]; then cat {REMOTE_SETTINGS_PATH}; else echo '{{}}'; fi"
    );
    let output = Command::new("ssh")
        .args([host, &remote_cmd])
        .output()
        .with_context(|| format!("failed to read settings.json on {host}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "could not read {REMOTE_SETTINGS_PATH} on {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(trimmed)
        .with_context(|| format!("parsing remote {REMOTE_SETTINGS_PATH} from {host}"))
}

/// Pipe `payload` into `ssh host "cat > settings.json"`.
///
/// Using stdin avoids any shell-escaping concerns with the JSON contents.
fn write_remote_settings(host: &str, payload: &str) -> Result<()> {
    let remote_cmd = format!("cat > {REMOTE_SETTINGS_PATH}");

    let mut child = Command::new("ssh")
        .args([host, &remote_cmd])
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn ssh to {host} for writing settings.json"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("ssh child has no stdin"))?;
        stdin
            .write_all(payload.as_bytes())
            .context("writing settings payload to ssh stdin")?;
    }

    let status = child.wait().context("waiting for ssh to finish")?;
    if !status.success() {
        anyhow::bail!("failed to write {REMOTE_SETTINGS_PATH} on {host}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_command_includes_host_tag() {
        let cmd = hook_command_for("kayshon");
        assert!(cmd.contains("--remote-tag kayshon"), "missing tag: {cmd}");
        assert!(cmd.contains("lonko-hook"), "missing binary: {cmd}");
    }

    #[test]
    fn hook_command_is_distinct_per_host() {
        assert_ne!(hook_command_for("a"), hook_command_for("b"));
    }
}
