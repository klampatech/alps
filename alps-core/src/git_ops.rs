//! Small git wrappers for the ALPS CLI.
//!
//! The CLI needs a "commit if there are changes, otherwise no-op" behavior so
//! end-of-run auto-commits don't print noisy warnings when the work lives in
//! a gitignored directory (e.g. `tasks/<id>/`). `commit_smart` checks
//! `git status --porcelain` first and only commits when there's something to
//! commit.

use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitOpsError {
    #[error("git {op} failed: {msg}")]
    Git { op: String, msg: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// What happened when we tried to auto-commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// There were changes — committed successfully.
    Committed,
    /// `git status --porcelain` was empty — nothing to commit.
    NothingToCommit,
    /// `git status` succeeded but `git commit` failed (rare; usually a hook).
    CommitFailed(String),
}

/// Commit changes in `dir` with `message` if any exist. Returns `NothingToCommit`
/// if the working tree is clean. Returns `CommitFailed` (with stderr) if
/// `git commit` itself failed.
pub fn commit_smart(dir: &Path, message: &str) -> Result<CommitOutcome, GitOpsError> {
    // 1. Check porcelain status — empty means nothing to commit.
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()?;
    if !status_output.status.success() {
        return Err(GitOpsError::Git {
            op: "status --porcelain".to_string(),
            msg: String::from_utf8_lossy(&status_output.stderr).into_owned(),
        });
    }
    if status_output.stdout.is_empty() {
        return Ok(CommitOutcome::NothingToCommit);
    }

    // 2. There are changes — stage and commit.
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()?;
    if !add.success() {
        return Err(GitOpsError::Git {
            op: "add -A".to_string(),
            msg: format!("exit {:?}", add.code()),
        });
    }

    let commit = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .output()?;
    if !commit.status.success() {
        return Ok(CommitOutcome::CommitFailed(
            String::from_utf8_lossy(&commit.stderr).into_owned(),
        ));
    }
    Ok(CommitOutcome::Committed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::path::PathBuf::from(format!(
            "/tmp/alps-gitops-test-{}-{}{}",
            label, pid, nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let s = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(s.success(), "git {:?} failed", args);
    }

    #[test]
    fn commit_smart_returns_nothing_on_clean_repo() {
        // Regression: previously, commit_smart would try to commit even on a
        // clean repo and print "git commit failed" noise. The fix: detect the
        // clean state first and return NothingToCommit without calling commit.
        let dir = unique_dir("clean");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "alps@test"]);
        git(&dir, &["config", "user.name", "ALPS"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);

        let outcome = commit_smart(&dir, "should not commit").unwrap();
        assert_eq!(outcome, CommitOutcome::NothingToCommit);
        // Verify no commit was made.
        let log = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&log.stdout).trim().is_empty(),
            "expected no commits but log was: {}",
            String::from_utf8_lossy(&log.stdout)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_smart_commits_when_files_present() {
        let dir = unique_dir("changes");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "alps@test"]);
        git(&dir, &["config", "user.name", "ALPS"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);

        // Add a tracked file so we have something to commit.
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        git(&dir, &["add", "README.md"]);
        git(&dir, &["commit", "-m", "initial"]);

        // Now make a new change and run commit_smart.
        fs::write(dir.join("CHANGES.md"), "new content\n").unwrap();
        let outcome = commit_smart(&dir, "auto: capture changes").unwrap();
        assert_eq!(outcome, CommitOutcome::Committed);

        // Verify the commit landed.
        let log = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&log.stdout);
        assert!(
            text.contains("auto: capture changes"),
            "expected commit message in log, got: {}",
            text
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
