// lonko-hook: reads a Claude Code hook event from stdin and forwards it
// to the lonko TUI via a Unix socket.
//
// Designed to be fast (<10ms) — no async runtime, no heavy deps.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

fn socket_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join("lonko.sock")
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

fn try_send(payload: &str) -> bool {
    let sock = socket_path();
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

fn main() -> anyhow::Result<()> {
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
    }

    let enriched = serde_json::to_string(&event)?;

    // Try to forward to lonko socket
    if try_send(&enriched) {
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
            if try_send(&enriched) {
                return Ok(());
            }
        }
        log("gave up waiting for lonko to start");
    } else {
        log("lonko not running");
    }

    Ok(())
}
