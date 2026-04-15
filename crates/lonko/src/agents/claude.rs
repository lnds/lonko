//! Claude Code-specific constants, paths, and helpers.

use std::path::PathBuf;

/// Process name used by `pgrep -x` to find running Claude Code sessions.
pub const BINARY_NAME: &str = "claude";

/// Directory name used for config (both `$HOME/.claude` and repo-local `.claude/`).
pub const DIR_NAME: &str = ".claude";

/// Prefix stripped from model strings in the UI (e.g. `claude-opus-4-6` → `opus-4-6`).
pub const MODEL_NAME_PREFIX: &str = "claude-";

/// `$HOME/.claude`.
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(DIR_NAME)
}

/// `$HOME/.claude/settings.json` — user-level hook settings.
pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// `$HOME/.claude/lonko.sock` — Unix socket lonko listens on for hook events.
pub fn socket_path() -> PathBuf {
    config_dir().join("lonko.sock")
}

/// `$HOME/.claude/sessions/` — directory Claude writes session lifecycle files to.
pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

/// `$HOME/.claude/projects/<slug>/<session_id>.jsonl` — transcript JSONL for a given cwd+session.
///
/// Claude replaces `/` and `.` with `-` when slugifying the cwd.
pub fn transcript_path(cwd: &str, session_id: &str) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    config_dir()
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"))
}

/// Map a permission-prompt key (`y`/`w`/`n`) to the stdin byte Claude expects
/// for its numbered prompt (1=yes, 2=always, 3=no).
pub fn permission_key_to_stdin(key: &str) -> Option<&'static str> {
    match key {
        "y" => Some("1"),
        "w" => Some("2"),
        "n" => Some("3"),
        _ => None,
    }
}

/// Compact a Claude model string for display (e.g.
/// `claude-haiku-4-5-20251001` → `haiku-4-5`).
pub fn short_model_name(model: &str) -> String {
    model
        .replace(MODEL_NAME_PREFIX, "")
        .replace("-20251001", "")
}
