//! Workdir guard — prevent rapid re-invocation of alps in the same workdir.
//!
//! ## Why this exists
//!
//! On 2026-07-27, a herdr smoke run (`w9S:p1`) completed at 13:38:40 with
//! "ALPS — Done". 0.5 seconds later, a fresh Plan agent was enqueued for a
//! second task ID (`alps/2026-07-27T133840-...`). The alps loop itself
//! terminates correctly on Judge Pass — the second Plan was triggered by
//! a wrapping agent (likely Claude TUI) re-invoking `alps run` to verify
//! the prior result.
//!
//! alps cannot prevent the wrapping agent from typing the command again,
//! but it CAN make the second invocation refuse to start with a clear
//! "recent completion" error. This module implements that guard via a
//! sentinel file at `<workdir>/.alps-last-done`.
//!
//! ## How it works
//!
//! - `mark_complete(workdir, task_id)` — writes a sentinel at the end of every
//!   successful alps run. Content: `<task_id>:<unix_ms_timestamp>`.
//! - `check_recent_completion(workdir, threshold)` — at start of every alps
//!   run, reads the sentinel. If it exists AND was written within the last
//!   `threshold`, returns an error. The CLI uses 5 seconds as the default.
//! - `--force` flag in the CLI bypasses the guard for legitimate re-runs.
//!
//! The sentinel is intentionally outside `tasks/` so it's NOT part of the
//! per-task branch. It lives at the workdir root alongside `.git/`.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

/// Default sentinel filename (workdir-relative).
const SENTINEL_FILENAME: &str = ".alps-last-done";

/// Default threshold (in seconds) — the maximum age of a sentinel that
/// will block a new alps run. Below 5s, the wrapping agent is most likely
/// re-invoking; above 5s, a human almost certainly re-typed the command.
pub const DEFAULT_THRESHOLD_SECS: u64 = 5;

#[derive(Debug, Error)]
pub enum WorkdirGuardError {
    #[error("io error reading workdir sentinel: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed sentinel file at {path}: {reason}")]
    Parse { path: String, reason: String },

    #[error(
        "recent completion in workdir: task {task_id} completed {seconds_ago}s ago \
         (threshold {threshold_secs}s). The wrapping agent may have re-invoked alps. \
         Wait a few seconds, or pass --force to bypass."
    )]
    RecentCompletion {
        task_id: String,
        seconds_ago: u64,
        threshold_secs: u64,
    },
}

/// Read the sentinel file, if present. Returns `(task_id, completed_at_unix_ms)`.
pub fn read_sentinel(workdir: &Path) -> Result<Option<(String, u128)>, WorkdirGuardError> {
    let path = workdir.join(SENTINEL_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let mut parts = content.splitn(2, ':');
    let task_id = parts
        .next()
        .ok_or_else(|| WorkdirGuardError::Parse {
            path: path.display().to_string(),
            reason: "missing task_id".to_string(),
        })?
        .trim()
        .to_string();
    let ts_str = parts.next().ok_or_else(|| WorkdirGuardError::Parse {
        path: path.display().to_string(),
        reason: "missing timestamp".to_string(),
    })?;
    let ts: u128 = ts_str
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| WorkdirGuardError::Parse {
            path: path.display().to_string(),
            reason: format!("invalid timestamp '{}': {}", ts_str, e),
        })?;
    Ok(Some((task_id, ts)))
}

/// If a sentinel exists and was written within `threshold`, return
/// `RecentCompletion`. Otherwise (no sentinel, or old sentinel), return Ok.
pub fn check_recent_completion(
    workdir: &Path,
    threshold: Duration,
) -> Result<(), WorkdirGuardError> {
    let Some((task_id, ts)) = read_sentinel(workdir)? else {
        return Ok(());
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| WorkdirGuardError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?
        .as_millis();
    let age_ms = now.saturating_sub(ts);
    let age_secs = (age_ms / 1000) as u64;
    if age_secs < threshold.as_secs() {
        return Err(WorkdirGuardError::RecentCompletion {
            task_id,
            seconds_ago: age_secs,
            threshold_secs: threshold.as_secs(),
        });
    }
    Ok(())
}

/// Write the sentinel file marking this workdir as having a recently
/// completed task. Idempotent — overwrites any existing sentinel.
pub fn mark_complete(workdir: &Path, task_id: &str) -> Result<(), WorkdirGuardError> {
    let path = workdir.join(SENTINEL_FILENAME);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| WorkdirGuardError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?
        .as_millis();
    let content = format!("{}:{}", task_id, now);
    std::fs::write(&path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "alps-guard-test-{}-{}{}",
            label, pid, nanos
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn check_passes_when_no_sentinel() {
        let dir = unique_dir("no-sentinel");
        let result = check_recent_completion(&dir, Duration::from_secs(5));
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_fails_when_recent_sentinel_exists() {
        let dir = unique_dir("recent");
        mark_complete(&dir, "task-abc").unwrap();
        let result = check_recent_completion(&dir, Duration::from_secs(5));
        match result {
            Err(WorkdirGuardError::RecentCompletion { task_id, seconds_ago, threshold_secs }) => {
                assert_eq!(task_id, "task-abc");
                assert!(seconds_ago < 5, "seconds_ago was {}", seconds_ago);
                assert_eq!(threshold_secs, 5);
            }
            other => panic!("expected RecentCompletion, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_passes_when_old_sentinel_exists() {
        let dir = unique_dir("old");
        // Write a sentinel with timestamp 60s in the past
        let path = dir.join(SENTINEL_FILENAME);
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            - 60_000;
        std::fs::write(&path, format!("old-task:{}", old_ts)).unwrap();
        let result = check_recent_completion(&dir, Duration::from_secs(5));
        assert!(result.is_ok(), "expected Ok (old sentinel), got {:?}", result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_complete_writes_sentinel() {
        let dir = unique_dir("mark");
        mark_complete(&dir, "task-xyz").unwrap();
        let (task_id, ts) = read_sentinel(&dir).unwrap().expect("sentinel should exist");
        assert_eq!(task_id, "task-xyz");
        // Timestamp should be within the last few seconds
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        assert!(now - ts < 5_000, "timestamp too old: now={} ts={}", now, ts);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_complete_overwrites_existing_sentinel() {
        let dir = unique_dir("overwrite");
        mark_complete(&dir, "task-1").unwrap();
        mark_complete(&dir, "task-2").unwrap();
        let (task_id, _) = read_sentinel(&dir).unwrap().expect("sentinel should exist");
        assert_eq!(task_id, "task-2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_sentinel_returns_parse_error() {
        let dir = unique_dir("malformed");
        let path = dir.join(SENTINEL_FILENAME);
        std::fs::write(&path, "no-colon-here").unwrap();
        let result = read_sentinel(&dir);
        assert!(matches!(result, Err(WorkdirGuardError::Parse { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_timestamp_returns_parse_error() {
        let dir = unique_dir("bad-ts");
        let path = dir.join(SENTINEL_FILENAME);
        std::fs::write(&path, "task-id:not-a-number").unwrap();
        let result = read_sentinel(&dir);
        assert!(matches!(result, Err(WorkdirGuardError::Parse { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
