use std::path::Path;
use std::process::Command;

use crate::control::tmux;

/// Expand `~` or `~/` prefix to the user's home directory.
pub(crate) fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|p| p.join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
    }
    path.to_string()
}

/// Re-collapse an absolute path back to `~/...` form when it lives under $HOME.
pub(crate) fn collapse_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            if rest.is_empty() {
                return "~".to_string();
            }
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

/// Tab-complete a path input, shell-style.
///
/// - Expands `~` to home dir internally.
/// - If the input ends with `/` or is a directory, lists its children.
/// - Otherwise treats the last component as a prefix and matches siblings.
/// - Returns the completed input (with `~` re-collapsed if applicable) or
///   the original input unchanged when there are no matches.
/// - Only completes to directories (files are ignored).
pub(crate) fn complete_path(input: &str) -> String {
    if input.is_empty() {
        return input.to_string();
    }

    let expanded = expand_tilde(input);
    let exp_path = Path::new(&expanded);

    let (parent, prefix) = if exp_path.is_dir() && input.ends_with('/') {
        // Input like "/tmp/" or "~/projects/" — list children of this dir.
        (exp_path.to_path_buf(), String::new())
    } else {
        // Input like "/tmp/fo" — complete "fo" against siblings in /tmp.
        let par = exp_path.parent().unwrap_or(Path::new("/"));
        let pfx = exp_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (par.to_path_buf(), pfx)
    };

    let Ok(entries) = std::fs::read_dir(&parent) else {
        return input.to_string();
    };

    let mut matches: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            // Only directories.
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
        })
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    if matches.is_empty() {
        return input.to_string();
    }

    matches.sort();

    // Longest common prefix among all matches.
    let lcp = longest_common_prefix(&matches);

    let completed = parent.join(&lcp).to_string_lossy().into_owned();

    // Append `/` when the completion is an exact single match (a directory).
    let completed = if matches.len() == 1 {
        format!("{completed}/")
    } else {
        completed
    };

    // Re-collapse to ~/... if the original input used tilde.
    if input.starts_with('~') {
        collapse_home(&completed)
    } else {
        completed
    }
}

fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let mut len = first.len();
    for s in &strings[1..] {
        len = first
            .chars()
            .zip(s.chars())
            .take_while(|(a, b)| a == b)
            .count()
            .min(len);
    }
    first.chars().take(len).collect()
}

/// Launch a new Claude Code agent session in a fresh tmux session.
///
/// Creates a detached tmux session at `cwd`, pipes the initial prompt to
/// `claude` via stdin so the session remains interactive after the first turn.
/// The cwd is tilde-expanded and created if it does not exist.
pub fn run(cwd: &str, prompt: &str) -> anyhow::Result<()> {
    let cwd = expand_tilde(cwd);

    // Create the directory if it does not exist.
    let path = Path::new(&cwd);
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }

    let session_name = unique_session_name(&cwd);

    tmux::create_session(&session_name, &cwd)?;

    // Pipe the prompt into claude via printf so it arrives as the first
    // user message. Single-quote escaping prevents shell injection.
    // The target "session:" means "active window in session" — safe because
    // derive_session_name only allows [a-zA-Z0-9_-].
    let escaped = prompt.replace('\'', "'\\''");
    let cmd = format!("clear && printf '%s\\n' '{}' | claude", escaped);
    tmux::send_command(&format!("{}:", session_name), &cmd)?;

    // Switch the user to the new session.
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", &session_name])
        .status();

    Ok(())
}

/// Derive a unique tmux session name from the cwd basename.
/// Appends `-2`, `-3`, etc. if the name already exists.
fn unique_session_name(cwd: &str) -> String {
    let base = derive_session_name(cwd);

    let exists = |name: &str| -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    if !exists(&base) {
        return base;
    }

    for n in 2..=100u32 {
        let candidate = format!("{base}-{n}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    // Fallback: should never realistically happen.
    format!("{base}-{}", std::process::id())
}

/// Sanitize the cwd basename into a tmux-friendly session name.
fn derive_session_name(cwd: &str) -> String {
    let base = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("agent");
    let safe: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = safe.trim_matches('-');
    if trimmed.is_empty() { "agent".to_string() } else { trimmed.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_session_name_basic() {
        assert_eq!(derive_session_name("/home/user/my-project"), "my-project");
    }

    #[test]
    fn derive_session_name_spaces() {
        assert_eq!(derive_session_name("/tmp/my project"), "my-project");
    }

    #[test]
    fn derive_session_name_dots() {
        assert_eq!(derive_session_name("/tmp/foo.bar"), "foo-bar");
    }

    #[test]
    fn derive_session_name_empty_fallback() {
        assert_eq!(derive_session_name("/"), "agent");
    }

    #[test]
    fn expand_tilde_home() {
        let expanded = expand_tilde("~/projects/foo");
        assert!(!expanded.starts_with("~/"));
        assert!(expanded.ends_with("/projects/foo"));
    }

    #[test]
    fn expand_tilde_bare() {
        let expanded = expand_tilde("~");
        assert!(!expanded.is_empty());
        assert_ne!(expanded, "~");
    }

    #[test]
    fn expand_tilde_absolute_unchanged() {
        assert_eq!(expand_tilde("/tmp/foo"), "/tmp/foo");
    }

    #[test]
    fn expand_tilde_relative_unchanged() {
        assert_eq!(expand_tilde("relative/path"), "relative/path");
    }

    // ── complete_path ─────────────────────────────────────────────────────

    fn make_dirs(base: &std::path::Path, names: &[&str]) {
        for name in names {
            std::fs::create_dir_all(base.join(name)).unwrap();
        }
    }

    #[test]
    fn complete_single_match_appends_slash() {
        let tmp = tempfile::tempdir().unwrap();
        make_dirs(tmp.path(), &["alpha", "beta"]);
        let input = format!("{}/alp", tmp.path().display());
        let result = complete_path(&input);
        assert_eq!(result, format!("{}/alpha/", tmp.path().display()));
    }

    #[test]
    fn complete_multiple_matches_to_common_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        make_dirs(tmp.path(), &["project-a", "project-b", "other"]);
        let input = format!("{}/proj", tmp.path().display());
        let result = complete_path(&input);
        assert_eq!(result, format!("{}/project-", tmp.path().display()));
    }

    #[test]
    fn complete_no_match_returns_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        make_dirs(tmp.path(), &["alpha"]);
        let input = format!("{}/zzz", tmp.path().display());
        let result = complete_path(&input);
        assert_eq!(result, input);
    }

    #[test]
    fn complete_trailing_slash_lists_children() {
        let tmp = tempfile::tempdir().unwrap();
        make_dirs(tmp.path(), &["only-child"]);
        let input = format!("{}/", tmp.path().display());
        let result = complete_path(&input);
        assert_eq!(result, format!("{}/only-child/", tmp.path().display()));
    }

    #[test]
    fn complete_ignores_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_dirs(tmp.path(), &["real-dir"]);
        std::fs::write(tmp.path().join("real-file"), "").unwrap();
        let input = format!("{}/real", tmp.path().display());
        let result = complete_path(&input);
        // Only the dir matches, not the file.
        assert_eq!(result, format!("{}/real-dir/", tmp.path().display()));
    }

    #[test]
    fn complete_tilde_path() {
        // This test exercises the tilde round-trip: expand → complete → collapse.
        // We can only test it meaningfully if $HOME exists and has subdirs.
        let home = dirs::home_dir().unwrap();
        if !home.exists() { return; }
        // Find any directory inside $HOME to use as a known target.
        let Some(entry) = std::fs::read_dir(&home).ok()
            .and_then(|mut rd| rd.find(|e| {
                e.as_ref().ok()
                    .and_then(|e| e.file_type().ok())
                    .is_some_and(|ft| ft.is_dir())
            }))
            .and_then(|e| e.ok())
        else { return };
        let name = entry.file_name().to_string_lossy().into_owned();
        // Give it a partial prefix (first 2 chars).
        let prefix: String = name.chars().take(2.min(name.len())).collect();
        let input = format!("~/{prefix}");
        let result = complete_path(&input);
        assert!(result.starts_with("~/"), "should stay in tilde form: {result}");
    }

    #[test]
    fn complete_empty_returns_empty() {
        assert_eq!(complete_path(""), "");
    }
}
