// Discovers Tailnet peers by invoking `tailscale status --json`.
// Only peers reported Online are returned; Self is excluded because it
// is represented in a separate top-level field of the JSON.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Absolute paths to try when `tailscale` is not on the inherited PATH.
/// macOS in particular installs the CLI shim at `/usr/local/bin/tailscale`,
/// which is missing from the PATH that tmux carries when launched from a
/// minimal login shell — without this fallback, the Remote panel stays
/// stuck on "scanning tailnet…" forever because the discovery silently
/// reports zero peers.
const TAILSCALE_FALLBACK_PATHS: &[&str] = &[
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetPeer {
    pub hostname: String,
    pub dns_name: String,
    pub os: String,
}

#[derive(Debug, Deserialize)]
struct Status {
    #[serde(rename = "Self", default)]
    self_node: Option<SelfNode>,
    #[serde(rename = "Peer", default)]
    peer: std::collections::BTreeMap<String, Peer>,
}

#[derive(Debug, Deserialize)]
struct SelfNode {
    #[serde(rename = "HostName")]
    hostname: String,
}

#[derive(Debug, Deserialize)]
struct Peer {
    #[serde(rename = "HostName")]
    hostname: String,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "Online", default)]
    online: bool,
}

/// Run `tailscale status --json` and return the list of online peers.
///
/// Returns `Ok(vec![])` if tailscale is not installed or the daemon is not
/// running — callers treat an empty tailnet the same as no remote hosts.
pub fn list_online_peers() -> Result<Vec<TailnetPeer>> {
    let bin = resolve_tailscale_bin();
    let output = match Command::new(&bin).arg("status").arg("--json").output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).context("failed to spawn `tailscale status --json`"),
    };

    if !output.status.success() {
        // Daemon not running, not logged in, etc. — treat as empty tailnet.
        tracing::debug!(
            "tailscale status exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(vec![]);
    }

    parse_peers(&output.stdout)
}

/// Pick a usable `tailscale` invocation. Prefers the PATH lookup so user
/// overrides win, and falls back to known absolute install locations
/// when PATH does not contain them (typical for processes inherited from
/// a long-lived tmux server on macOS).
fn resolve_tailscale_bin() -> String {
    if which("tailscale").is_some() {
        return "tailscale".to_string();
    }
    for candidate in TAILSCALE_FALLBACK_PATHS {
        if Path::new(candidate).exists() {
            return (*candidate).to_string();
        }
    }
    "tailscale".to_string()
}

/// Minimal PATH lookup. Returns the first directory in `$PATH` that
/// contains an executable file named `name`, joined into a full path.
/// We avoid pulling in a `which` crate just for this one call site.
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn parse_peers(json: &[u8]) -> Result<Vec<TailnetPeer>> {
    let status: Status =
        serde_json::from_slice(json).context("failed to parse tailscale status JSON")?;

    let self_hostname = status.self_node.map(|n| n.hostname.to_ascii_lowercase());

    let mut peers: Vec<TailnetPeer> = status
        .peer
        .into_values()
        .filter(|p| p.online)
        .filter(|p| {
            // Exclude any peer whose hostname matches this machine.
            self_hostname.as_deref() != Some(&p.hostname.to_ascii_lowercase())
        })
        .map(|p| TailnetPeer {
            // Normalise to lowercase so this matches what `lonko-hook
            // --remote-tag <host>` stamps into events (the installer
            // uses the user-typed arg verbatim, usually lowercase).
            // Without this, the same host ends up as two separate
            // agents: one provisional seeded from the uppercase
            // Tailscale name, and one promoted from a lowercase hook.
            hostname: p.hostname.to_ascii_lowercase(),
            dns_name: strip_trailing_dot(p.dns_name),
            os: p.os,
        })
        .collect();

    // Stable, predictable order by hostname (already lowercase above).
    peers.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    Ok(peers)
}

fn strip_trailing_dot(mut s: String) -> String {
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = br#"{
        "Version": "0.0.0",
        "Self": {
            "HostName": "selfhost",
            "DNSName": "selfhost.example.ts.net.",
            "OS": "macOS",
            "Online": true
        },
        "Peer": {
            "nodekey:aaa": {
                "HostName": "alpha",
                "DNSName": "alpha.example.ts.net.",
                "OS": "linux",
                "Online": true
            },
            "nodekey:bbb": {
                "HostName": "bravo",
                "DNSName": "bravo.example.ts.net.",
                "OS": "linux",
                "Online": false
            },
            "nodekey:ccc": {
                "HostName": "Charlie",
                "DNSName": "charlie.example.ts.net.",
                "OS": "macOS",
                "Online": true
            },
            "nodekey:ddd": {
                "HostName": "selfhost",
                "DNSName": "selfhost.example.ts.net.",
                "OS": "macOS",
                "Online": true
            }
        }
    }"#;

    #[test]
    fn parses_online_peers_excluding_offline_and_self() {
        let peers = parse_peers(FIXTURE).expect("fixture parses");
        assert_eq!(
            peers,
            vec![
                TailnetPeer {
                    hostname: "alpha".into(),
                    dns_name: "alpha.example.ts.net".into(),
                    os: "linux".into(),
                },
                TailnetPeer {
                    // Source name is "Charlie"; we lowercase at intake.
                    hostname: "charlie".into(),
                    dns_name: "charlie.example.ts.net".into(),
                    os: "macOS".into(),
                },
            ]
        );
    }

    #[test]
    fn empty_peer_map_returns_empty_vec() {
        let peers = parse_peers(br#"{"Peer": {}}"#).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn missing_peer_field_returns_empty_vec() {
        let peers = parse_peers(br#"{}"#).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn which_finds_known_executable_in_path() {
        // /bin/sh is required to exist on every POSIX system the test runs on.
        let dir = Path::new("/bin");
        assert!(dir.join("sh").is_file(), "test precondition");
        // Force PATH so the lookup is deterministic regardless of the
        // user's shell config.
        // SAFETY: tests in this module run sequentially; we restore PATH below.
        let saved = std::env::var_os("PATH");
        // SAFETY: single-threaded test mutating process env.
        unsafe { std::env::set_var("PATH", "/bin"); }
        let resolved = which("sh");
        if let Some(prev) = saved {
            // SAFETY: see above.
            unsafe { std::env::set_var("PATH", prev); }
        }
        assert_eq!(resolved.as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn which_returns_none_when_path_lacks_binary() {
        let saved = std::env::var_os("PATH");
        // SAFETY: see comment in `which_finds_known_executable_in_path`.
        unsafe { std::env::set_var("PATH", "/nonexistent-dir-for-lonko-test"); }
        let resolved = which("definitely-not-a-real-binary-xyzzy");
        if let Some(prev) = saved {
            // SAFETY: see above.
            unsafe { std::env::set_var("PATH", prev); }
        }
        assert!(resolved.is_none());
    }
}
