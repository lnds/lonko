// Discovers Tailnet peers by invoking `tailscale status --json`.
// Only peers reported Online are returned; Self is excluded because it
// is represented in a separate top-level field of the JSON.

use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

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
    let output = match Command::new("tailscale").arg("status").arg("--json").output() {
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
            hostname: p.hostname,
            dns_name: strip_trailing_dot(p.dns_name),
            os: p.os,
        })
        .collect();

    // Stable, predictable order by hostname (case-insensitive).
    peers.sort_by(|a, b| a.hostname.to_ascii_lowercase().cmp(&b.hostname.to_ascii_lowercase()));

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
                    hostname: "Charlie".into(),
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
}
