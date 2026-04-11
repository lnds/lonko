use std::path::Path;
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

/// Remove a git worktree. Finds the main repo via --git-common-dir and runs
/// `git worktree remove --force` from there.
pub fn remove(cwd: &str) -> anyhow::Result<()> {
    // Get the worktree root
    let wt_root = git_root(cwd)
        .ok_or_else(|| anyhow::anyhow!("not a git repository: {cwd}"))?;

    // Get the main repo's .git dir (e.g. /path/to/main-repo/.git)
    let output = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-common-dir"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("git rev-parse --git-common-dir failed for {cwd}");
    }
    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // common_dir is the .git directory of the main repo; resolve to absolute if relative
    let common_dir_abs = if Path::new(&common_dir).is_absolute() {
        Path::new(&common_dir).to_path_buf()
    } else {
        Path::new(cwd).join(&common_dir)
    };
    let main_repo = common_dir_abs
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot derive main repo from {common_dir}"))?;

    let status = Command::new("git")
        .args(["-C", main_repo.to_string_lossy().as_ref(),
               "worktree", "remove", "--force", &wt_root])
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
        // This test runs inside the shepherd repo itself
        let root = git_root(env!("CARGO_MANIFEST_DIR"));
        assert!(root.is_some());
        let root = root.unwrap();
        assert!(root.ends_with("shepherd"));
    }
}
