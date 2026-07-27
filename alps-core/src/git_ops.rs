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

/// Pattern that excludes the ralph nested git repo from `git add -A`.
/// Written to `<workdir>/.git/info/exclude` (git's per-repo, never-tracked
/// local exclude file) by `commit_smart` before staging.
const RALPH_EXCLUDE_PATTERN: &str = "tasks/*/implementation/ralph/";

/// Ensure `<workdir>/.git/info/exclude` contains the ralph exclusion. Idempotent:
/// the line is only appended if it isn't already present. This is the
/// `.git/info/exclude` mechanism (not `.gitignore` or a pathspec) because
/// it's the only layer honored by the directory walker before the
/// embedded-repo error fires — see the comment in `commit_smart` for details.
fn ensure_ralph_excluded(dir: &Path) -> Result<(), GitOpsError> {
    let exclude_path = dir.join(".git").join("info").join("exclude");
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.trim() == RALPH_EXCLUDE_PATTERN)
    {
        return Ok(());
    }
    // Ensure the file ends with a newline before appending.
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "# alps: exclude ralph's nested git repo (added by commit_smart)\n{}\n",
        RALPH_EXCLUDE_PATTERN
    ));
    std::fs::write(&exclude_path, content)?;
    Ok(())
}

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
    //
    // Ensure the ralph nested git repo is excluded via `.git/info/exclude`
    // (git's per-repo, never-tracked local exclude file). ralph.sh runs
    // `git init` inside `tasks/<id>/implementation/ralph/` as its own working
    // repo; without the exclusion, `git add -A` fails on git 2.42+ with
    // "does not have a commit checked out" (the embedded-repo error). The
    // alps source repo's own .gitignore already excludes
    // `/tasks/**/implementation/`, but alps runs in a USER workdir whose
    // .gitignore we don't control, so the fix must live here.
    //
    // Why `.git/info/exclude` and not a pathspec: git's `:`-exclude pathspec
    // is applied AFTER recursion, so `git add -A -- ':!tasks/*/…'` still
    // walks into the ralph dir and errors before the exclusion fires. The
    // local exclude file is honored by the directory walker, which is the
    // exact layer where the error originates.
    //
    // The exclude is idempotent: we only append the line if it isn't
    // already present, so re-runs don't accumulate duplicates.
    if let Err(e) = ensure_ralph_excluded(dir) {
        eprintln!("warning: failed to update .git/info/exclude: {} (commit may fail with embedded-repo error)", e);
    }
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

/// Create `branch_name` in `dir` and check it out. Idempotent: if the branch
/// already exists (e.g. from a prior outer-loop attempt), just check it out
/// without erroring.
///
/// Used to create per-task branches `alps/<task-id>` so each run has isolated
/// commit history that the user can review and merge or discard.
pub fn create_branch(dir: &Path, branch_name: &str) -> Result<(), GitOpsError> {
    // Check if the branch already exists.
    let exists = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", branch_name)])
        .current_dir(dir)
        .output()?
        .status
        .success();

    if exists {
        // Reuse — just check it out.
        let checkout = Command::new("git")
            .args(["checkout", branch_name])
            .current_dir(dir)
            .status()?;
        if !checkout.success() {
            return Err(GitOpsError::Git {
                op: format!("checkout existing branch {}", branch_name),
                msg: format!("exit {:?}", checkout.code()),
            });
        }
    } else {
        // Create and check out in one step.
        let create = Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(dir)
            .status()?;
        if !create.success() {
            return Err(GitOpsError::Git {
                op: format!("create branch {}", branch_name),
                msg: format!("exit {:?}", create.code()),
            });
        }
    }
    Ok(())
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

    #[test]
    fn commit_smart_excludes_nested_ralph_repo() {
        // Regression: when ralph.sh runs in tasks/<id>/implementation/ralph/,
        // it `git init`s that subdirectory as its own repo. `git add -A` from
        // the workdir root picks up the nested .git/ and emits the
        // "warning: adding embedded git repository" advisory — see
        // smoke7 (w9V:p1) 2026-07-27 14:25 smoke run, and earlier f452ca3
        // session. The fix: exclude the ralph subdirectory via pathspec so
        // it never lands in the outer index.
        let dir = unique_dir("nested-ralph");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "alps@test"]);
        git(&dir, &["config", "user.name", "ALPS"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);

        // Initial commit so we can stage new files.
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        git(&dir, &["add", "README.md"]);
        git(&dir, &["commit", "-m", "initial"]);

        // Create the alps task structure: tasks/<id>/ with a real plan.json.
        let task_id = "2026-07-27T000000-test";
        let task_dir = dir.join("tasks").join(task_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("plan.json"), "{}").unwrap();

        // Simulate ralph.sh: `git init` inside implementation/ralph/ + write
        // a real file. The nested .git/ is what triggers the warning.
        let ralph_dir = task_dir.join("implementation").join("ralph");
        fs::create_dir_all(&ralph_dir).unwrap();
        let ralph_init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&ralph_dir)
            .output()
            .unwrap();
        assert!(ralph_init.status.success(), "ralph git init should succeed");
        fs::write(ralph_dir.join("prd.json"), "{}").unwrap();
        fs::write(ralph_dir.join("progress.txt"), "# Ralph\n").unwrap();

        // Run commit_smart.
        let outcome = commit_smart(&dir, "auto: capture changes").unwrap();
        assert_eq!(outcome, CommitOutcome::Committed);

        // Verify the ralph files are NOT in the outer index.
        let ls_files = Command::new("git")
            .args(["ls-files"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&ls_files.stdout);
        assert!(
            !files.contains("implementation/ralph/"),
            "ralph dir must not be tracked by the outer repo, but index contains:\n{}",
            files
        );
        assert!(
            !files.contains("implementation/ralph/.git"),
            "ralph nested .git must not be tracked"
        );
        // Sanity: the alps-side files ARE tracked.
        assert!(
            files.contains("plan.json"),
            "plan.json should be tracked, but index contains:\n{}",
            files
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_branch_makes_new_branch_and_checks_it_out() {
        // First run of a task: branch doesn't exist, create_branch creates it
        // and switches to it.
        let dir = unique_dir("new-branch");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "alps@test"]);
        git(&dir, &["config", "user.name", "ALPS"]);
        // Need an initial commit so we can create a branch off it.
        fs::write(dir.join("README.md"), "init\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "init"]);

        create_branch(&dir, "alps/test-task").unwrap();

        // Verify we're on the new branch.
        let out = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(current, "alps/test-task", "expected on new branch, was: {}", current);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_branch_is_idempotent_on_retry() {
        // Outer-loop retry: branch already exists from a prior attempt.
        // create_branch must reuse it, NOT fail with "branch already exists".
        let dir = unique_dir("existing-branch");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "alps@test"]);
        git(&dir, &["config", "user.name", "ALPS"]);
        fs::write(dir.join("README.md"), "init\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "init"]);
        // Simulate a prior run: create the branch and add a commit on it.
        git(&dir, &["checkout", "-b", "alps/retry-task"]);
        fs::write(dir.join("prior.md"), "from previous attempt\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "prior attempt"]);

        // Now call create_branch on the existing branch.
        create_branch(&dir, "alps/retry-task").unwrap();

        // Verify the prior commit is still on the branch.
        let log = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&log.stdout);
        assert!(
            text.contains("prior attempt"),
            "prior commit should still be on the branch, got: {}",
            text
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
