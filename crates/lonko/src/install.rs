// lonko --install-hooks
// Merges lonko-hook into ~/.claude/settings.json preserving all existing hooks.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agents::claude;

pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

/// Per-event outcome from a merge pass — what the installer should report.
#[derive(Debug, PartialEq, Eq)]
pub enum MergeStatus {
    Added,
    AlreadyConfigured,
}

fn hook_cmd() -> String {
    // Use full path so Claude Code hook runner finds it regardless of PATH
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/usr/local"))
        .join(".cargo")
        .join("bin")
        .join("lonko-hook")
        .to_string_lossy()
        .into_owned()
}

fn settings_path() -> PathBuf {
    claude::settings_file()
}

fn has_hook(groups: &[Value], cmd: &str) -> bool {
    groups.iter().any(|g| {
        g.get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| hooks.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(cmd)))
            .unwrap_or(false)
    })
}

/// Remove bare "lonko-hook" entries (without full path) — left from earlier installs.
fn remove_stale_entries(groups: &mut Vec<Value>) {
    groups.retain(|g| {
        let cmds: Vec<&str> = g
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter()
                    .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        // Remove groups whose only command is the bare "lonko-hook"
        !cmds.iter().all(|c| *c == "lonko-hook")
    });
}

/// Merge the given hook command into each HOOK_EVENTS entry of `settings`.
///
/// Pure: does no I/O. Caller decides where `settings` came from and where the
/// updated value is written to (local filesystem or remote host via SSH).
///
/// Returns the updated settings value together with a per-event status list,
/// in the same order as [`HOOK_EVENTS`].
pub fn merge_hooks_into(mut settings: Value, cmd: &str) -> Result<(Value, Vec<MergeStatus>)> {
    // Normalize to an object if the file was missing / empty
    if settings.is_null() {
        settings = json!({});
    }

    let hook_entry = json!({
        "hooks": [{ "type": "command", "command": cmd }]
    });

    let settings_obj = settings.as_object_mut().context("settings.json is not an object")?;
    let hooks_obj = settings_obj
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .context("hooks is not an object")?;

    let mut statuses = Vec::with_capacity(HOOK_EVENTS.len());

    for event in HOOK_EVENTS {
        let existing = hooks_obj
            .entry(*event)
            .or_insert(json!([]))
            .as_array_mut()
            .with_context(|| format!("{event} hooks is not an array"))?;

        remove_stale_entries(existing);

        if has_hook(existing, cmd) {
            statuses.push(MergeStatus::AlreadyConfigured);
        } else {
            existing.push(hook_entry.clone());
            statuses.push(MergeStatus::Added);
        }
    }

    Ok((settings, statuses))
}

pub fn run() -> Result<()> {
    let path = settings_path();
    let cmd = hook_cmd();

    let existing: Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        json!({})
    };

    let (updated, statuses) = merge_hooks_into(existing, &cmd)?;

    for (event, status) in HOOK_EVENTS.iter().zip(statuses.iter()) {
        match status {
            MergeStatus::Added => println!("  {event}: added"),
            MergeStatus::AlreadyConfigured => println!("  {event}: already configured"),
        }
    }

    let content = serde_json::to_string_pretty(&updated)? + "\n";
    std::fs::write(&path, content)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("\nHooks installed in {}", path.display());
    println!("Using: {cmd}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_into_empty_settings() {
        let (out, statuses) = merge_hooks_into(json!({}), "/cargo/bin/lonko-hook").unwrap();
        assert!(statuses.iter().all(|s| *s == MergeStatus::Added));
        let hooks = out.get("hooks").unwrap().as_object().unwrap();
        for event in HOOK_EVENTS {
            let arr = hooks.get(*event).unwrap().as_array().unwrap();
            assert_eq!(arr.len(), 1, "{event} should have exactly one group");
            let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, "/cargo/bin/lonko-hook");
        }
    }

    #[test]
    fn idempotent_on_second_run() {
        let (once, _) = merge_hooks_into(json!({}), "/cargo/bin/lonko-hook").unwrap();
        let (twice, statuses) = merge_hooks_into(once.clone(), "/cargo/bin/lonko-hook").unwrap();
        assert!(statuses.iter().all(|s| *s == MergeStatus::AlreadyConfigured));
        assert_eq!(once, twice);
    }

    #[test]
    fn preserves_unrelated_hooks() {
        let original = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{ "type": "command", "command": "/usr/bin/other-tool" }]
                }]
            },
            "someOtherKey": "keep me"
        });
        let (out, _) = merge_hooks_into(original, "/cargo/bin/lonko-hook").unwrap();
        assert_eq!(out["someOtherKey"], "keep me");
        let session_start = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2, "existing hook should be kept alongside lonko's");
        assert_eq!(session_start[0]["hooks"][0]["command"], "/usr/bin/other-tool");
        assert_eq!(session_start[1]["hooks"][0]["command"], "/cargo/bin/lonko-hook");
    }

    #[test]
    fn removes_stale_bare_lonko_hook_entries() {
        let original = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{ "type": "command", "command": "lonko-hook" }]
                }]
            }
        });
        let (out, _) = merge_hooks_into(original, "/cargo/bin/lonko-hook").unwrap();
        let session_start = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 1);
        assert_eq!(session_start[0]["hooks"][0]["command"], "/cargo/bin/lonko-hook");
    }

    #[test]
    fn different_commands_coexist() {
        // e.g. local install with `lonko-hook` and later a `lonko-hook --remote-tag foo`
        let (once, _) = merge_hooks_into(json!({}), "/cargo/bin/lonko-hook").unwrap();
        let (out, statuses) =
            merge_hooks_into(once, "/cargo/bin/lonko-hook --remote-tag host1").unwrap();
        assert!(statuses.iter().all(|s| *s == MergeStatus::Added));
        let session_start = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2);
    }
}
