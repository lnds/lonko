// lonko-hook: reads a Claude Code hook event from stdin and forwards it
// to the lonko TUI via a Unix socket.
//
// Designed to be fast (<10ms) — no async runtime, no heavy deps.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

/// Unix socket lonko-hook writes to.
///
/// Local invocations use `~/.claude/lonko.sock` — the same socket the local
/// `lonko` TUI listens on. When `--remote-tag` is set, we switch to
/// `~/.claude/lonko-bridge.sock`, which the SSH reverse tunnel (LONKO-49)
/// forwards to the operator's local `lonko`. The two-path split keeps the
/// invariant that the unsuffixed socket belongs to *this* machine's lonko,
/// even on hosts where someone also runs lonko locally.
fn socket_path(remote: bool) -> std::path::PathBuf {
    let name = if remote { "lonko-bridge.sock" } else { "lonko.sock" };
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join(name)
}

fn log_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join("lonko-hook.log")
}

fn log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

fn try_send(payload: &str, remote: bool) -> bool {
    let sock = socket_path(remote);
    match UnixStream::connect(&sock) {
        Ok(mut stream) => {
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
            if let Err(e) = stream.write_all(payload.as_bytes()).and_then(|_| stream.write_all(b"\n")) {
                log(&format!("write error: {e}"));
                false
            } else {
                log("forwarded ok");
                true
            }
        }
        _ => false,
    }
}

/// Open the lonko side panel (20% right split) via the toggle script.
fn open_panel() {
    if std::env::var("TMUX").is_err() {
        return;
    }
    let _ = std::process::Command::new("bash")
        .arg(
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join(".config/tmux/scripts/lonko-panel.sh"),
        )
        .spawn();
}

/// Pull `--remote-tag <HOST>` out of argv. Unknown flags and trailing
/// arguments are silently ignored to keep the hook forward-compatible.
///
/// The host ends up as `"host": "<HOST>"` in the forwarded JSON, so the
/// local lonko can tell remote events apart from local ones and route
/// them to the right agent card (see LONKO-49/50).
fn parse_remote_tag(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--remote-tag" => {
                if let Some(host) = iter.next()
                    && !host.is_empty()
                {
                    return Some(host.clone());
                }
            }
            s if s.starts_with("--remote-tag=") => {
                let host = &s["--remote-tag=".len()..];
                if !host.is_empty() {
                    return Some(host.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let remote_tag = parse_remote_tag(&args);

    // Read the hook event JSON from stdin
    let mut payload = String::new();
    io::stdin().read_to_string(&mut payload)?;

    log(&format!("received: {}", payload.trim()));

    // Enrich with TMUX env vars if available
    let tmux_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let tmux = std::env::var("TMUX").unwrap_or_default();

    // Parse and re-serialize with extra fields
    let mut event: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            log(&format!("failed to parse hook payload: {e}"));
            return Ok(());
        }
    };

    if let Some(obj) = event.as_object_mut() {
        if !tmux_pane.is_empty() {
            obj.insert("tmux_pane".into(), serde_json::Value::String(tmux_pane));
        }
        if !tmux.is_empty() {
            obj.insert("tmux".into(), serde_json::Value::String(tmux));
        }
        if let Some(host) = &remote_tag {
            obj.insert("host".into(), serde_json::Value::String(host.clone()));
        }
    }

    let enriched = serde_json::to_string(&event)?;

    // Try to forward to lonko socket
    if try_send(&enriched, remote_tag.is_some()) {
        return Ok(());
    }

    // On a remote host the local socket is the one forwarded via SSH
    // reverse tunnel; there is no panel here to open, and retrying
    // just delays the hook. Log and exit.
    if remote_tag.is_some() {
        log("lonko socket not reachable on remote host (bridge down?)");
        return Ok(());
    }

    // lonko not running — if this is Notification or Stop, open the panel and retry
    let hook_name = event["hook_event_name"].as_str().unwrap_or("");
    let should_open = matches!(hook_name, "Notification" | "Stop");
    if should_open {
        log(&format!("lonko not running, opening panel for {hook_name} event"));
        open_panel();

        // Wait for lonko to start, then retry a few times
        for delay_ms in [200, 400, 600, 800] {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            if try_send(&enriched, remote_tag.is_some()) {
                return Ok(());
            }
        }
        log("gave up waiting for lonko to start");
    } else {
        log("lonko not running");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        std::iter::once("lonko-hook")
            .chain(raw.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn missing_flag_returns_none() {
        assert_eq!(parse_remote_tag(&args(&[])), None);
        assert_eq!(parse_remote_tag(&args(&["--unrelated", "foo"])), None);
    }

    #[test]
    fn space_separated_flag() {
        assert_eq!(
            parse_remote_tag(&args(&["--remote-tag", "kayshon"])),
            Some("kayshon".to_string())
        );
    }

    #[test]
    fn equals_form() {
        assert_eq!(
            parse_remote_tag(&args(&["--remote-tag=kayshon"])),
            Some("kayshon".to_string())
        );
    }

    #[test]
    fn empty_value_treated_as_missing() {
        assert_eq!(parse_remote_tag(&args(&["--remote-tag", ""])), None);
        assert_eq!(parse_remote_tag(&args(&["--remote-tag="])), None);
    }

    #[test]
    fn flag_after_unknown_args_still_parsed() {
        assert_eq!(
            parse_remote_tag(&args(&["--future-flag", "x", "--remote-tag", "h1"])),
            Some("h1".to_string())
        );
    }

    #[test]
    fn first_occurrence_wins() {
        assert_eq!(
            parse_remote_tag(&args(&["--remote-tag", "a", "--remote-tag", "b"])),
            Some("a".to_string())
        );
    }
}
