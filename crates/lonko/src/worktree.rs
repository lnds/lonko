use std::path::{Path, PathBuf};
use std::process::Command;

use crate::control::tmux;

/// Sanitize a git branch name for use as a tmux session name and directory suffix.
/// Replaces `/` and `.` with `-`, strips leading `-`.
pub fn sanitize_branch(branch: &str) -> String {
    let s: String = branch
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    s.trim_start_matches('-').to_string()
}

/// Find the git repository root for a given directory.
pub fn git_root(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() { None } else { Some(root) }
}

/// Canonical root shared across all worktrees of the same repo.
///
/// Returns the parent directory of `git rev-parse --git-common-dir`, so the
/// main repo and every linked worktree of it all map to the same path. This
/// is the key used to group agent sessions in the UI and to locate the main
/// repo when removing a worktree.
///
/// Returns `None` strictly when `cwd` is not inside a git repository or the
/// `git` invocation fails — callers that want a soft fallback (e.g. the UI
/// grouping path) must apply their own.
pub fn repo_common_root(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if common_dir.is_empty() {
        return None;
    }
    let common_abs = if Path::new(&common_dir).is_absolute() {
        std::path::PathBuf::from(&common_dir)
    } else {
        Path::new(cwd).join(&common_dir)
    };
    // Canonicalize to normalize symlinks/relative segments so worktrees and
    // the main repo compare equal regardless of how cwd was spelled.
    let canonical = std::fs::canonicalize(&common_abs).unwrap_or(common_abs);
    let parent = canonical.parent()?;
    Some(parent.to_string_lossy().into_owned())
}

/// Recursively copy a directory tree from `src` to `dst`.
/// Creates `dst` and all intermediate directories as needed.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Copy the `.claude/` config directory from the source repo into a new
/// worktree, so Claude Code agents inherit local project settings
/// (settings.local.json, allowed permissions, etc.). Silently skips if
/// the source has no `.claude/` or the destination already has one.
fn copy_claude_config(source_root: &str, worktree_path: &Path) {
    let src = PathBuf::from(source_root).join(".claude");
    let dst = worktree_path.join(".claude");
    if src.is_dir() && !dst.exists() {
        if let Err(e) = copy_dir_recursive(&src, &dst) {
            eprintln!("warning: failed to copy .claude config to worktree: {e}");
        }
    }
}

/// Create a git worktree, a tmux session in it, and launch claude.
pub fn run(cwd: &str, branch: &str) -> anyhow::Result<()> {
    let root = git_root(cwd)
        .ok_or_else(|| anyhow::anyhow!("not a git repository: {cwd}"))?;

    let repo_name = Path::new(&root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");

    let safe_branch = sanitize_branch(branch);
    let session_name = format!("{repo_name}-{safe_branch}");
    let worktree_path = Path::new(&root)
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join(&session_name);

    let wt_str = worktree_path.to_string_lossy();

    // Create worktree: try with -b (new branch) first, fall back to existing branch
    let status = Command::new("git")
        .args(["-C", &root, "worktree", "add", &wt_str, "-b", branch])
        .status()?;
    if !status.success() {
        let status = Command::new("git")
            .args(["-C", &root, "worktree", "add", &wt_str, branch])
            .status()?;
        if !status.success() {
            anyhow::bail!("git worktree add failed for branch '{branch}'");
        }
    }

    // Copy .claude config so the new worktree inherits project settings
    copy_claude_config(&root, &worktree_path);

    // Create tmux session in the worktree directory
    tmux::create_session(&session_name, &wt_str)?;

    // Launch claude
    tmux::send_command(&session_name, "claude")?;

    // Switch to the new session
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", &session_name])
        .status();

    Ok(())
}

/// Metadata for an open pull request associated with a branch.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u32,
    pub title: String,
    pub branch: String,
}

/// Query GitHub for an **open** PR whose head branch matches `branch`.
/// Requires the `gh` CLI. Returns `None` when no open PR exists, `gh` is
/// missing, or the repo has no GitHub remote.
pub fn pr_for_branch(cwd: &str, branch: &str) -> Option<PrInfo> {
    let output = Command::new("gh")
        .args([
            "pr", "list",
            "--head", branch,
            "--state", "open",
            "--json", "number,title,headRefName",
            "--jq", ".[0] | .number,.title,.headRefName",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut lines = text.lines();
    let number: u32 = lines.next()?.parse().ok()?;
    let title = lines.next()?.to_string();
    let head_branch = lines.next()?.to_string();
    Some(PrInfo { number, title, branch: head_branch })
}

/// Create a worktree from a PR branch, open a tmux session, and launch claude.
///
/// Uses `gh pr checkout` semantics: fetches the remote branch so the local
/// worktree tracks the PR head. Falls back to `worktree::run` if the branch
/// is already available locally.
pub fn run_from_pr(cwd: &str, pr: &PrInfo) -> anyhow::Result<()> {
    let root = git_root(cwd)
        .ok_or_else(|| anyhow::anyhow!("not a git repository: {cwd}"))?;

    let repo_name = Path::new(&root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");

    let safe_branch = sanitize_branch(&pr.branch);
    let session_name = format!("{repo_name}-{safe_branch}");
    let worktree_path = Path::new(&root)
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join(&session_name);

    let wt_str = worktree_path.to_string_lossy();

    // Fetch the PR branch from origin so the worktree tracks upstream
    let _ = Command::new("git")
        .args(["-C", &root, "fetch", "origin", &pr.branch])
        .status();

    // Create worktree tracking the remote branch
    let status = Command::new("git")
        .args([
            "-C", &root, "worktree", "add",
            &wt_str, "-b", &pr.branch,
            &format!("origin/{}", pr.branch),
        ])
        .status()?;
    if !status.success() {
        // Branch may already exist locally — try plain worktree add
        let status = Command::new("git")
            .args(["-C", &root, "worktree", "add", &wt_str, &pr.branch])
            .status()?;
        if !status.success() {
            anyhow::bail!("git worktree add failed for PR #{} (branch '{}')", pr.number, pr.branch);
        }
    }

    // Copy .claude config so the new worktree inherits project settings
    copy_claude_config(&root, &worktree_path);

    // Create tmux session in the worktree directory
    tmux::create_session(&session_name, &wt_str)?;

    // Show PR context before launching claude
    let pr_msg = format!("echo '# PR #{}: {}' && claude", pr.number, pr.title.replace('\'', "'\\''"));
    tmux::send_command(&session_name, &pr_msg)?;

    // Switch to the new session
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", &session_name])
        .status();

    Ok(())
}

/// Check if a directory is inside a git worktree (not the main repo).
/// Compares --git-dir and --git-common-dir: if they differ, it's a linked worktree.
pub fn is_worktree(cwd: &str) -> bool {
    let git_dir = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-dir"])
        .output();
    let common_dir = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-common-dir"])
        .output();
    match (git_dir, common_dir) {
        (Ok(gd), Ok(cd)) if gd.status.success() && cd.status.success() => {
            let gd = String::from_utf8_lossy(&gd.stdout).trim().to_string();
            let cd = String::from_utf8_lossy(&cd.stdout).trim().to_string();
            // Resolve to absolute for reliable comparison
            let gd_abs = if Path::new(&gd).is_absolute() {
                Path::new(&gd).to_path_buf()
            } else {
                Path::new(cwd).join(&gd)
            };
            let cd_abs = if Path::new(&cd).is_absolute() {
                Path::new(&cd).to_path_buf()
            } else {
                Path::new(cwd).join(&cd)
            };
            gd_abs != cd_abs
        }
        _ => false,
    }
}

/// Remove a git worktree. Finds the main repo via `repo_common_root` and
/// runs `git worktree remove --force` from there.
pub fn remove(cwd: &str) -> anyhow::Result<()> {
    let wt_root = git_root(cwd)
        .ok_or_else(|| anyhow::anyhow!("not a git repository: {cwd}"))?;
    let main_repo = repo_common_root(cwd)
        .ok_or_else(|| anyhow::anyhow!("cannot derive main repo from {cwd}"))?;

    let status = Command::new("git")
        .args(["-C", &main_repo, "worktree", "remove", "--force", &wt_root])
        .status()?;
    if !status.success() {
        anyhow::bail!("git worktree remove failed for {wt_root}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_simple_branch() {
        assert_eq!(sanitize_branch("feat-login"), "feat-login");
    }

    #[test]
    fn sanitize_branch_with_slashes() {
        assert_eq!(sanitize_branch("feature/auth/oauth"), "feature-auth-oauth");
    }

    #[test]
    fn sanitize_branch_with_dots() {
        assert_eq!(sanitize_branch("release.1.0"), "release-1-0");
    }

    #[test]
    fn sanitize_branch_leading_slash() {
        assert_eq!(sanitize_branch("/hotfix"), "hotfix");
    }

    #[test]
    fn sanitize_branch_mixed() {
        assert_eq!(sanitize_branch("feat/v2.0/new-api"), "feat-v2-0-new-api");
    }

    #[test]
    fn git_root_nonexistent_dir() {
        assert!(git_root("/nonexistent/path/xyz").is_none());
    }

    #[test]
    fn git_root_non_git_dir() {
        assert!(git_root("/tmp").is_none());
    }

    #[test]
    fn git_root_valid_repo() {
        // This test runs inside a git checkout of the lonko repo. The checkout
        // directory may be named anything (e.g. `lonko`, `lonko-lonko-16`), so
        // instead of hardcoding a suffix, verify that the returned root is an
        // ancestor of CARGO_MANIFEST_DIR — the only invariant we actually care
        // about for `git_root`.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let root = git_root(manifest_dir).expect("manifest dir is inside a git repo");
        assert!(
            Path::new(manifest_dir).starts_with(&root),
            "git root {root} should be an ancestor of manifest dir {manifest_dir}"
        );
    }

    #[test]
    fn repo_common_root_is_stable_within_worktree() {
        // Every directory inside the same working tree must resolve to the
        // same canonical repo root. This is the invariant the agents list
        // relies on to cluster worktrees of the same repo together.
        let from_crate = repo_common_root(env!("CARGO_MANIFEST_DIR")).expect("git repo");
        let repo_top = git_root(env!("CARGO_MANIFEST_DIR")).expect("git repo");
        let from_top = repo_common_root(&repo_top).expect("git repo");
        assert_eq!(from_crate, from_top);
        assert!(Path::new(&from_crate).is_dir());
    }

    #[test]
    fn repo_common_root_non_git_dir() {
        assert!(repo_common_root("/tmp").is_none());
    }

    #[test]
    fn copy_dir_recursive_copies_nested_structure() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-dir");
        let _ = std::fs::remove_dir_all(&tmp);

        let src = tmp.join("src");
        let nested = src.join("subdir");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(src.join("a.json"), r#"{"key":"val"}"#).unwrap();
        std::fs::write(nested.join("b.txt"), "hello").unwrap();

        let dst = tmp.join("dst");
        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.json")).unwrap(), r#"{"key":"val"}"#);
        assert_eq!(std::fs::read_to_string(dst.join("subdir/b.txt")).unwrap(), "hello");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_claude_config_skips_when_dst_exists() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-claude");
        let _ = std::fs::remove_dir_all(&tmp);

        let source_root = tmp.join("repo");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(source_root.join(".claude")).unwrap();
        std::fs::write(source_root.join(".claude/settings.json"), "orig").unwrap();

        // Pre-create destination .claude — should NOT be overwritten
        std::fs::create_dir_all(worktree.join(".claude")).unwrap();
        std::fs::write(worktree.join(".claude/settings.json"), "existing").unwrap();

        copy_claude_config(source_root.to_str().unwrap(), &worktree);

        assert_eq!(std::fs::read_to_string(worktree.join(".claude/settings.json")).unwrap(), "existing");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_claude_config_copies_when_missing() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-claude-new");
        let _ = std::fs::remove_dir_all(&tmp);

        let source_root = tmp.join("repo");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(source_root.join(".claude")).unwrap();
        std::fs::write(source_root.join(".claude/settings.local.json"), "config").unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        copy_claude_config(source_root.to_str().unwrap(), &worktree);

        assert_eq!(
            std::fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap(),
            "config"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
