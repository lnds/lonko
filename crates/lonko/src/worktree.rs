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

/// Well-known dotfiles that are typically gitignored but should be
/// propagated to new worktrees so the dev environment works out of the box.
const DOTFILES: &[&str] = &[
    ".envrc",
    ".tool-versions",
    ".nvmrc",
    ".node-version",
    ".ruby-version",
    ".python-version",
];

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

/// Copy well-known dotfiles from the source repo into a new worktree.
/// Each file is copied individually (not as a directory). Skips files that
/// don't exist at the source or already exist at the destination.
/// Returns `true` if `.envrc` was copied (so the caller can run `direnv allow`).
fn copy_dotfiles(source_root: &str, worktree_path: &Path) -> bool {
    let src_root = Path::new(source_root);
    let mut envrc_copied = false;
    for name in DOTFILES {
        let src = src_root.join(name);
        let dst = worktree_path.join(name);
        if src.is_file() && !dst.exists() {
            if let Err(e) = std::fs::copy(&src, &dst) {
                eprintln!("warning: failed to copy {name} to worktree: {e}");
            } else if *name == ".envrc" {
                envrc_copied = true;
            }
        }
    }
    envrc_copied
}

/// Run `direnv allow` on the worktree's `.envrc` so it is trusted.
/// Silently ignores failures (direnv may not be installed).
fn direnv_allow(worktree_path: &Path) {
    let _ = Command::new("direnv")
        .arg("allow")
        .arg(worktree_path.join(".envrc"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Set up the worktree environment: copy `.claude/` config and well-known
/// dotfiles from the source repo, and trust `.envrc` if it was copied.
fn setup_worktree_env(source_root: &str, worktree_path: &Path) {
    copy_claude_config(source_root, worktree_path);
    if copy_dotfiles(source_root, worktree_path) {
        direnv_allow(worktree_path);
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

    // Propagate local environment config to the new worktree
    setup_worktree_env(&root, &worktree_path);

    // Create tmux session in the worktree directory
    tmux::create_session(&session_name, &wt_str)?;

    // Launch claude
    tmux::send_command(&session_name, "clear && claude")?;

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

    // Propagate local environment config to the new worktree
    setup_worktree_env(&root, &worktree_path);

    // Create tmux session in the worktree directory
    tmux::create_session(&session_name, &wt_str)?;

    // Show PR context before launching claude
    let pr_msg = format!("clear && echo '# PR #{}: {}' && claude", pr.number, pr.title.replace('\'', "'\\''"));
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

/// Check whether the `gh` CLI is available on the system.
pub fn has_gh() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// PR merge state for a branch.
#[derive(Debug, PartialEq, Eq)]
pub enum PrState {
    /// The branch has a PR that was merged.
    Merged,
    /// The branch has a PR that is not merged (open, closed, draft).
    NotMerged,
    /// No PR found for this branch, or `gh` failed.
    Unknown,
}

/// Check whether a branch has a merged PR on GitHub using `gh pr view`.
///
/// Runs from `repo_cwd` so `gh` picks up the correct remote.
/// Returns `PrState::Unknown` if `gh` is not installed or the command fails.
pub fn pr_state_for_branch(repo_cwd: &str, branch: &str) -> PrState {
    let output = Command::new("gh")
        .args(["pr", "view", branch, "--json", "state", "--jq", ".state"])
        .current_dir(repo_cwd)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let state = String::from_utf8_lossy(&o.stdout).trim().to_lowercase();
            if state == "merged" {
                PrState::Merged
            } else {
                PrState::NotMerged
            }
        }
        _ => PrState::Unknown,
    }
}

/// Delete a local branch. Uses `git branch -D` (force delete) because the
/// caller has already confirmed via `gh` that the PR is merged — local git
/// may not know this without a recent fetch, so `-d` would refuse.
pub fn delete_local_branch(repo_cwd: &str, branch: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["-C", repo_cwd, "branch", "-D", branch])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git branch -d {branch}: {stderr}");
    }
    Ok(())
}

/// Delete the remote tracking branch. Runs `git push origin --delete <branch>`.
///
/// NOTE: assumes the remote is named `origin`. This will fail (harmlessly) for
/// forks or repos where the remote has a different name.
pub fn delete_remote_branch(repo_cwd: &str, branch: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["-C", repo_cwd, "push", "origin", "--delete", branch])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git push origin --delete {branch}: {stderr}");
    }
    Ok(())
}

/// Result of post-worktree-removal branch cleanup.
#[derive(Debug)]
pub struct CleanupResult {
    pub local_deleted: bool,
    pub remote_deleted: bool,
}

/// After a worktree is removed, check if the branch has a merged PR and clean
/// up local + remote branches if so.
///
/// Returns `None` if cleanup was skipped (no gh, no branch, PR not merged).
pub fn cleanup_merged_branch(repo_cwd: &str, branch: &str) -> Option<CleanupResult> {
    if !has_gh() {
        return None;
    }

    if pr_state_for_branch(repo_cwd, branch) != PrState::Merged {
        return None;
    }

    let local_deleted = delete_local_branch(repo_cwd, branch).is_ok();
    let remote_deleted = delete_remote_branch(repo_cwd, branch).is_ok();

    Some(CleanupResult {
        local_deleted,
        remote_deleted,
    })
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

    #[test]
    fn copy_dotfiles_copies_envrc_and_returns_true() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-dotfiles");
        let _ = std::fs::remove_dir_all(&tmp);

        let source = tmp.join("repo");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(source.join(".envrc"), "use flake").unwrap();
        std::fs::write(source.join(".tool-versions"), "erlang 26").unwrap();

        let result = copy_dotfiles(source.to_str().unwrap(), &worktree);

        assert!(result, "should return true when .envrc is copied");
        assert_eq!(std::fs::read_to_string(worktree.join(".envrc")).unwrap(), "use flake");
        assert_eq!(std::fs::read_to_string(worktree.join(".tool-versions")).unwrap(), "erlang 26");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_dotfiles_returns_false_without_envrc() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-dotfiles-no-envrc");
        let _ = std::fs::remove_dir_all(&tmp);

        let source = tmp.join("repo");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(source.join(".nvmrc"), "20").unwrap();

        let result = copy_dotfiles(source.to_str().unwrap(), &worktree);

        assert!(!result, "should return false when no .envrc");
        assert_eq!(std::fs::read_to_string(worktree.join(".nvmrc")).unwrap(), "20");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_dotfiles_skips_existing() {
        let tmp = std::env::temp_dir().join("lonko-test-copy-dotfiles-exist");
        let _ = std::fs::remove_dir_all(&tmp);

        let source = tmp.join("repo");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(source.join(".envrc"), "new").unwrap();
        std::fs::write(worktree.join(".envrc"), "existing").unwrap();

        let result = copy_dotfiles(source.to_str().unwrap(), &worktree);

        assert!(!result, "should not report copied when dst already exists");
        assert_eq!(std::fs::read_to_string(worktree.join(".envrc")).unwrap(), "existing");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn has_gh_does_not_panic() {
        // Smoke test: returns a bool regardless of whether gh is installed.
        let _ = has_gh();
    }

    #[test]
    fn pr_state_nonexistent_dir_returns_unknown() {
        assert_eq!(pr_state_for_branch("/nonexistent/path", "main"), PrState::Unknown);
    }

    #[test]
    fn pr_state_non_git_dir_returns_unknown() {
        assert_eq!(pr_state_for_branch("/tmp", "no-such-branch-xyz"), PrState::Unknown);
    }

    #[test]
    fn delete_local_branch_nonexistent_returns_err() {
        let repo = git_root(env!("CARGO_MANIFEST_DIR")).expect("git repo");
        assert!(delete_local_branch(&repo, "nonexistent-branch-xyz-999").is_err());
    }

    #[test]
    fn delete_remote_branch_nonexistent_returns_err() {
        // Use a local bare repo as "origin" so the test never hits the network.
        let tmp = std::env::temp_dir().join("lonko-test-delete-remote");
        let _ = std::fs::remove_dir_all(&tmp);

        let bare = tmp.join("remote.git");
        let repo = tmp.join("repo");
        assert!(Command::new("git").args(["init", "--bare"]).arg(&bare).output().unwrap().status.success());
        assert!(Command::new("git").args(["init"]).arg(&repo).output().unwrap().status.success());
        assert!(Command::new("git").args(["-C", repo.to_str().unwrap(), "remote", "add", "origin", bare.to_str().unwrap()]).output().unwrap().status.success());
        // Need at least one commit so git push has something to work with.
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        assert!(Command::new("git").args(["-C", repo.to_str().unwrap(), "add", "."]).output().unwrap().status.success());
        assert!(Command::new("git").args(["-C", repo.to_str().unwrap(), "commit", "-m", "init"]).output().unwrap().status.success());
        assert!(Command::new("git").args(["-C", repo.to_str().unwrap(), "push", "origin", "HEAD"]).output().unwrap().status.success());

        assert!(delete_remote_branch(repo.to_str().unwrap(), "nonexistent-branch-xyz-999").is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cleanup_merged_branch_non_git_dir_returns_none() {
        // No gh available in /tmp, or no PR — either way should return None.
        assert!(cleanup_merged_branch("/tmp", "no-such-branch").is_none());
    }
}
