// lonko --install-hooks
// Merges lonko-hook into ~/.claude/settings.json preserving all existing hooks.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agents::claude;

const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

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

pub fn run() -> Result<()> {
    let path = settings_path();
    let cmd = hook_cmd();

    let mut settings: Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        json!({})
    };

    let hook_entry = json!({
        "hooks": [{ "type": "command", "command": cmd }]
    });

    let settings_obj = settings.as_object_mut().context("settings.json is not an object")?;
    let hooks_obj = settings_obj
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .context("hooks is not an object")?;

    for event in HOOK_EVENTS {
        let existing = hooks_obj
            .entry(*event)
            .or_insert(json!([]))
            .as_array_mut()
            .with_context(|| format!("{event} hooks is not an array"))?;

        // Clean up stale bare-name entries from previous installs
        remove_stale_entries(existing);

        if has_hook(existing, &cmd) {
            println!("  {event}: already configured");
        } else {
            existing.push(hook_entry.clone());
            println!("  {event}: added");
        }
    }

    let content = serde_json::to_string_pretty(&settings)? + "\n";
    std::fs::write(&path, content)
        .with_context(|| format!("writing {}", path.display()))?;

    println!("\nHooks installed in {}", path.display());
    println!("Using: {cmd}");
    Ok(())
}
