//! Persistence — JSON file I/O for the task workspace.
//!
//! Each task has a directory at `tasks/<task-id>/`. State is written at
//! every transition; commits are made by the CLI.

use std::path::PathBuf;
use thiserror::Error;

use crate::domain::*;
use crate::receipt::Receipts;
use crate::summary::{TaskDetail, TaskState, TaskSummary};
use crate::task::*;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("task directory does not exist: {0}")]
    NoSuchTask(PathBuf),
}

/// All the files that make up a task workspace.
pub struct TaskWorkspace {
    pub root: PathBuf,
}

impl TaskWorkspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        TaskWorkspace { root: root.into() }
    }

    pub fn prompt_path(&self) -> PathBuf {
        self.root.join("prompt.md")
    }

    pub fn plan_path(&self) -> PathBuf {
        self.root.join("plan.json")
    }

    pub fn review_path(&self) -> PathBuf {
        self.root.join("review.json")
    }

    pub fn receipts_path(&self) -> PathBuf {
        self.root.join("receipts.json")
    }

    pub fn feedback_path(&self) -> PathBuf {
        self.root.join("feedback.json")
    }

    pub fn failure_path(&self) -> PathBuf {
        self.root.join("failure.json")
    }

    pub fn ralph_dir(&self) -> PathBuf {
        self.root.join("implementation").join("ralph")
    }

    /// Path to the per-task AGENTS.md (lives at the task level, not inside
    /// the ralph subdirectory, so it survives between ralph invocations and
    /// is visible to all agents: plan, review, judge, plan-on-retry).
    pub fn agents_md_path(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }

    /// Path to the implementation.json (the typed `Implementation` struct,
    /// serialized). Captures the deliverable_path, commits, and artifacts
    /// so the user can see exactly where the Judge walked. See SPEC §12
    /// item 2.
    pub fn implementation_path(&self) -> PathBuf {
        self.root.join("implementation.json")
    }

    pub fn exists(&self) -> bool {
        self.root.exists()
    }

    pub fn ensure_exists(&self) -> Result<(), PersistenceError> {
        if !self.exists() {
            std::fs::create_dir_all(&self.root)?;
        }
        Ok(())
    }

    pub fn write_prompt(&self, prompt: &Prompt) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        std::fs::write(
            self.prompt_path(),
            format!("# Prompt\n\n{}", prompt.as_str()),
        )?;
        Ok(())
    }

    pub fn write_plan(&self, plan: &Plan) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(plan)?;
        std::fs::write(self.plan_path(), json)?;
        Ok(())
    }

    pub fn write_review(&self, review: &Review) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(review)?;
        std::fs::write(self.review_path(), json)?;
        Ok(())
    }

    pub fn write_receipts(&self, receipts: &Receipts) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(receipts)?;
        std::fs::write(self.receipts_path(), json)?;
        Ok(())
    }

    /// Persist the typed `Implementation` to `implementation.json`. The CLI
    /// calls this at the end of the loop so the user can inspect which
    /// deliverable tree the Judge walked, what commits were made, and
    /// which artifacts were collected. See SPEC §12 item 2.
    pub fn write_implementation(&self, impl_: &Implementation) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(impl_)?;
        std::fs::write(self.implementation_path(), json)?;
        Ok(())
    }

    pub fn write_feedback(&self, feedback: &Feedback) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(feedback)?;
        std::fs::write(self.feedback_path(), json)?;
        Ok(())
    }

    pub fn write_failure(&self, failure: &FailureReason) -> Result<(), PersistenceError> {
        self.ensure_exists()?;
        let json = serde_json::to_string_pretty(failure)?;
        std::fs::write(self.failure_path(), json)?;
        Ok(())
    }
}

/// Trait so each state can write its own artifacts.
pub trait Persistable {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError>;
}

/// Persist a task in its current state. Each state has its own writer.
pub fn persist_task<S>(task: &Task<S>, workspace: &TaskWorkspace) -> Result<(), PersistenceError>
where
    Task<S>: Persistable,
{
    task.persist_to(workspace)
}

impl Persistable for Task<Idle> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_prompt(&self.prompt)
    }
}

impl Persistable for Task<Planned> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_prompt(&self.prompt)?;
        workspace.write_plan(&self.state.plan)
    }
}

impl Persistable for Task<Implemented> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_prompt(&self.prompt)?;
        workspace.write_plan(&self.state.plan)?;
        // Persist the typed Implementation too — captures deliverable_path,
        // commits, and artifacts. Without this, the user has no durable
        // record of which tree the Judge walked. See SPEC §12 item 2.
        workspace.write_implementation(&self.state.implementation)?;
        // Source artifacts (the actual code) are written by Ralph itself
        Ok(())
    }
}

impl Persistable for Task<Reviewed> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_prompt(&self.prompt)?;
        workspace.write_plan(&self.state.plan)?;
        workspace.write_review(&self.state.review)?;
        Ok(())
    }
}

impl Persistable for Task<Done> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_receipts(&self.state.receipts)
    }
}

impl Persistable for Task<Rejected> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_feedback(&self.state.feedback)
    }
}

impl Persistable for Task<Failed> {
    fn persist_to(&self, workspace: &TaskWorkspace) -> Result<(), PersistenceError> {
        workspace.write_failure(&self.state.reason)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Read-side API. Added 2026-08-23 to support `alps list` / `alps show`
// for the alps-gui consumer. Pure additions — every function below
// only reads from disk; nothing here mutates the workspace.
// ─────────────────────────────────────────────────────────────────────

impl TaskWorkspace {
    pub fn read_prompt(&self) -> Result<Option<Prompt>, PersistenceError> {
        match std::fs::read_to_string(self.prompt_path()) {
            Ok(s) => {
                // write_prompt() prefixes with "# Prompt\n\n"; strip it.
                let body = s
                    .strip_prefix("# Prompt\n\n")
                    .unwrap_or(&s)
                    .to_string();
                Ok(Some(Prompt(body)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_plan(&self) -> Result<Option<Plan>, PersistenceError> {
        match std::fs::read_to_string(self.plan_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_review(&self) -> Result<Option<Review>, PersistenceError> {
        match std::fs::read_to_string(self.review_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_receipts(&self) -> Result<Option<Receipts>, PersistenceError> {
        match std::fs::read_to_string(self.receipts_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_feedback(&self) -> Result<Option<Feedback>, PersistenceError> {
        match std::fs::read_to_string(self.feedback_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_failure(&self) -> Result<Option<FailureReason>, PersistenceError> {
        match std::fs::read_to_string(self.failure_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    pub fn read_implementation(&self) -> Result<Option<Implementation>, PersistenceError> {
        match std::fs::read_to_string(self.implementation_path()) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    /// Last-modified timestamp of the artifact that signals a terminal
    /// state (receipts.json / feedback.json / failure.json), in
    /// chronological order. Returns None if no terminal artifact exists.
    pub fn terminal_mtime(&self) -> Result<Option<std::time::SystemTime>, PersistenceError> {
        let candidates = [
            self.receipts_path(),
            self.feedback_path(),
            self.failure_path(),
        ];
        let mut latest: Option<std::time::SystemTime> = None;
        for path in &candidates {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(mtime) = meta.modified() {
                    latest = Some(match latest {
                        Some(prev) => prev.max(mtime),
                        None => mtime,
                    });
                }
            }
        }
        Ok(latest)
    }
}

/// Infer the current state of a task from which artifact files exist
/// on disk.
///
/// Precedence (highest wins):
///   1. `receipts.json`     → Done
///   2. `failure.json`      → Failed
///   3. `feedback.json`     → Rejected
///   4. `review.json`       → Reviewed
///   5. `implementation.json` → Implemented
///   6. `plan.json`         → Planned
///   7. `prompt.md` only    → Idle
///   8. nothing             → Unknown
///
/// `running_hint` is set by the caller when the orchestrator's
/// `<workdir>/.alps-pids.json` mentions this task AND the terminal
/// artifact's mtime is older than the PID's `started_at`. In that case
/// `Running` wins over any non-terminal state above (but not over Done /
/// Failed — those are truly terminal even if a stale PID file exists).
pub fn infer_state(workspace: &TaskWorkspace, running_hint: bool) -> TaskState {
    use TaskState::*;
    let receipts = workspace.receipts_path().exists();
    let failure = workspace.failure_path().exists();
    let feedback = workspace.feedback_path().exists();
    let review = workspace.review_path().exists();
    let implementation = workspace.implementation_path().exists();
    let plan = workspace.plan_path().exists();
    let prompt = workspace.prompt_path().exists();

    let inferred = if receipts {
        Done
    } else if failure {
        Failed
    } else if feedback {
        Rejected
    } else if review {
        Reviewed
    } else if implementation {
        Implemented
    } else if plan {
        Planned
    } else if prompt {
        Idle
    } else {
        Unknown
    };

    // Running overrides non-terminal states only. Done / Failed /
    // Rejected are sticky terminal states (a stale PID file should
    // not retroactively flip them to Running).
    if running_hint && !inferred.is_terminal() {
        Running
    } else {
        inferred
    }
}

/// Read every task under `<workdir>/tasks/` and return a summary for
/// each, sorted newest-first by `created_at`.
///
/// A task directory with no `prompt.md` is silently skipped (the
/// orchestrator creates the dir before writing `prompt.md`; a missing
/// prompt means the dir was created but the run died before
/// persisting — nothing useful to show).
pub fn list_tasks(workdir: &std::path::Path) -> Result<Vec<TaskSummary>, PersistenceError> {
    let tasks_root = workdir.join("tasks");
    if !tasks_root.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(&tasks_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let task_dir = entry.path();
        let task_id = entry.file_name().to_string_lossy().to_string();
        // Skip tasks with no prompt.md (mid-creation or pre-creation).
        match build_summary(&task_dir, &task_id) {
            Ok(s) => out.push(s),
            Err(PersistenceError::NoSuchTask(_)) => continue,
            Err(e) => return Err(e),
        }
    }

    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// Read the full detail view for one task. Returns Ok(None) if no task
/// with this ID exists under `<workdir>/tasks/`.
///
/// This is O(1) in the number of tasks in the workdir — it builds the
/// TaskSummary directly from this task's own artifacts via
/// `build_summary`, rather than re-walking the whole `tasks/` tree.
pub fn read_task(
    workdir: &std::path::Path,
    task_id: &str,
) -> Result<Option<TaskDetail>, PersistenceError> {
    use crate::summary::TaskDetail;

    let task_dir = workdir.join("tasks").join(task_id);
    if !task_dir.exists() {
        return Ok(None);
    }
    let workspace = TaskWorkspace::new(&task_dir);
    if !workspace.prompt_path().exists() {
        return Ok(None);
    }

    let prompt = workspace.read_prompt()?.map(|p| p.0);
    let plan = workspace.read_plan()?;
    let implementation = workspace.read_implementation()?;
    let review = workspace.read_review()?;
    let receipts = workspace.read_receipts()?;
    let feedback = workspace.read_feedback()?;
    let failure = workspace.read_failure()?;

    let summary = match build_summary(&task_dir, task_id) {
        Ok(s) => s,
        Err(PersistenceError::NoSuchTask(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    Ok(Some(TaskDetail {
        summary,
        prompt,
        plan,
        implementation,
        review,
        receipts,
        feedback,
        failure,
    }))
}

/// Build a `TaskSummary` from a single task directory's artifacts.
///
/// Single source of truth for the summary field assembly — used by both
/// `list_tasks` (over the whole tree) and `read_task` (over one task)
/// so the two paths can never disagree on field semantics.
///
/// Returns `PersistenceError::NoSuchTask` if `prompt.md` is missing —
/// the caller decides whether that means "skip silently" (list) or
/// "return None" (read_task).
pub fn build_summary(
    task_dir: &std::path::Path,
    task_id: &str,
) -> Result<TaskSummary, PersistenceError> {
    use chrono::Utc;

    let workspace = TaskWorkspace::new(task_dir);
    if !workspace.prompt_path().exists() {
        return Err(PersistenceError::NoSuchTask(task_dir.to_path_buf()));
    }

    let state = infer_state(&workspace, false);
    let prompt_excerpt = match workspace.read_prompt()? {
        Some(p) => excerpt(&p.0, 200),
        None => String::new(),
    };
    let created_at = parse_task_id_timestamp(task_id).unwrap_or_else(Utc::now);
    let completed_at = workspace
        .terminal_mtime()?
        .and_then(|t| chrono::DateTime::<Utc>::from(t).into());

    let (stories_passed, stories_total, iterations, elapsed_secs) =
        match workspace.read_receipts()? {
            Some(r) => (
                Some(r.implement_metrics.stories_passed),
                Some(r.implement_metrics.stories_total),
                Some(r.implement_metrics.iterations),
                Some(r.implement_metrics.elapsed_secs),
            ),
            None => (None, None, None, None),
        };
    let (review_assertions_passed, review_assertions_total, critical_findings) =
        match workspace.read_receipts()? {
            Some(r) => (
                Some(r.review_summary.assertions_passed),
                Some(r.review_summary.assertions_total),
                Some(r.review_summary.critical_findings),
            ),
            None => (None, None, None),
        };
    let (judge_verdict, judge_model) = match workspace.read_receipts()? {
        Some(r) => (Some("pass".to_string()), Some(r.judge_model.clone())),
        None => (None, None),
    };
    let has_receipts = workspace.receipts_path().exists();
    let has_feedback = workspace.feedback_path().exists();
    // `attempts` is best-effort. The outer-loop attempt counter lives
    // in Task<S>.state.attempts (a u32 on each state struct) but is
    // not currently persisted to disk — Receipts only carries
    // ImplementMetrics.iterations (inner-Ralph iterations, different
    // number). Until we add an explicit attempts field to receipts.json
    // (cross-crate contract change, deferred), this heuristic is the
    // best we can offer: 1 if receipts exist (at least one attempt
    // succeeded), 1 if feedback.json exists (at least one attempt
    // happened), else 0.
    let attempts = if has_receipts || has_feedback { 1 } else { 0 };

    Ok(TaskSummary {
        task_id: task_id.to_string(),
        state,
        attempts,
        prompt_excerpt,
        created_at,
        completed_at,
        stories_passed,
        stories_total,
        iterations,
        elapsed_secs,
        review_assertions_passed,
        review_assertions_total,
        critical_findings,
        judge_verdict,
        judge_model,
    })
}

/// Collapse whitespace and truncate to `max_chars`. Used for the
/// prompt_excerpt field on the dashboard.
fn excerpt(s: &str, max_chars: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= max_chars {
        collapsed
    } else {
        // Walk to a char boundary so we don't slice mid-codepoint.
        let mut end = max_chars;
        while end > 0 && !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &collapsed[..end])
    }
}

/// Parse the YYYY-MM-DDTHHMMSS prefix of a task ID into a UTC DateTime.
///
/// TaskId format is `YYYY-MM-DDTHHMMSS-<uuid8>` (see `domain::TaskId::new`).
/// We slice out the date and time digits and combine them with a space
/// for `NaiveDateTime::parse_from_str`.
///
/// Returns `None` if the task ID doesn't match the expected shape.
fn parse_task_id_timestamp(task_id: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, Utc};

    let t_pos = task_id.find('T')?;
    // After 'T' we need exactly 6 ASCII digits (HHMMSS), then either
    // end-of-string or '-' for the uuid suffix.
    if t_pos + 7 > task_id.len() {
        return None;
    }
    let time_part = &task_id[t_pos + 1..t_pos + 7];
    if !time_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Date portion is everything before 'T'. Validate the year/month/day
    // digits — NaiveDateTime will reject anything malformed but we want
    // to fail fast before allocating the format string.
    let date_part = &task_id[..t_pos];
    if date_part.len() != 10 || date_part.as_bytes()[4] != b'-' || date_part.as_bytes()[7] != b'-' {
        return None;
    }

    let naive_str = format!("{} {}", date_part, time_part);
    let naive = NaiveDateTime::parse_from_str(&naive_str, "%Y-%m-%d %H%M%S").ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}
