//! Task summary types — the read-side API surface.
//!
//! The GUI consumes `TaskSummary` (one per task, for `alps list`) and
//! `TaskDetail` (full prompt + raw artifacts, for `alps show <id>`).
//!
//! These types live here (not in `persistence.rs`) because they're the
//! derived-on-read surface — every field is computed from the on-disk
//! artifacts at call time. No state is held; no caching; no incremental
//! updates. If the orchestrator writes a new artifact after a read, the
//! next read sees it.
//!
//! ## Why a typed summary, not just `serde_json::Value`
//!
//! The GUI wants stable, versioned field names. `TaskSummary` is the
//! contract — when ALPS adds a new artifact file (or removes one), the
//! summary type either gains a new field (additive, non-breaking) or
//! drops one (breaking, requires a version bump in the GUI). Either
//! way the GUI sees a typed error at deserialization, not a silent
//! drift in field semantics.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Inferred state of a task, derived from which artifact files exist.
///
/// The on-disk layout (`tasks/<id>/{prompt.md, plan.json, review.json,
/// receipts.json, feedback.json, failure.json, implementation.json}`)
/// does NOT carry an explicit "current state" field — the state is
/// implied by which files are present. See `infer_state` in
/// `persistence.rs` for the precedence rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Orchestrator process is running this task right now.
    /// Detected via `<workdir>/.alps-pids.json` + recent mtime.
    Running,
    /// Only `prompt.md` exists; no Plan has run yet.
    Idle,
    /// `plan.json` exists, `implementation.json` doesn't.
    Planned,
    /// `implementation.json` exists, `review.json` doesn't.
    Implemented,
    /// `review.json` exists, no Judge verdict yet.
    Reviewed,
    /// `receipts.json` exists — Judge ACCEPTED. Terminal.
    Done,
    /// `feedback.json` exists without `receipts.json` — Judge REJECTED.
    /// May be reset to Idle by the orchestrator on retry.
    Rejected,
    /// `failure.json` exists — catastrophic agent error. Terminal.
    Failed,
    /// The task directory exists but no `prompt.md` is present (or the
    /// directory was deleted mid-flight). Should not appear in normal
    /// operation; surfaced so the UI doesn't silently drop it.
    Unknown,
}

impl TaskState {
    /// Color hint for the UI. Returns one of: "gray" / "blue" /
    /// "purple" / "yellow" / "green" / "red" / "dark-red" / "orange".
    /// Matches the StatusPill component's color palette in the SPEC.
    pub fn color_hint(&self) -> &'static str {
        match self {
            TaskState::Running => "blue",
            TaskState::Idle => "gray",
            TaskState::Planned => "indigo",
            TaskState::Implemented => "purple",
            TaskState::Reviewed => "yellow",
            TaskState::Done => "green",
            TaskState::Rejected => "red",
            TaskState::Failed => "dark-red",
            TaskState::Unknown => "orange",
        }
    }

    /// True for terminal states (Done, Failed, or — for the current
    /// outer-loop iteration — Rejected; the orchestrator may still
    /// reset a Rejected back to Idle on the next iteration, but the
    /// task as observed is not in flight).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Rejected
        )
    }
}

/// Summary view of one task — one row in `alps list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: String,
    pub state: TaskState,
    pub attempts: u32,

    /// First 200 chars of `prompt.md`, with newlines collapsed to spaces.
    /// Empty string if the prompt file is missing.
    pub prompt_excerpt: String,

    /// Timestamp parsed from the task ID prefix (YYYY-MM-DDTHHMMSS).
    pub created_at: DateTime<Utc>,

    /// Timestamp of the terminal artifact's last write — `receipts.json`
    /// for Done, `feedback.json` for Rejected, `failure.json` for Failed,
    /// `None` for non-terminal states.
    pub completed_at: Option<DateTime<Utc>>,

    /// From `receipts.json::implement_metrics` if present.
    pub stories_passed: Option<u32>,
    pub stories_total: Option<u32>,
    pub iterations: Option<u32>,
    pub elapsed_secs: Option<u64>,

    /// From `receipts.json::review_summary` if present.
    pub review_assertions_passed: Option<u32>,
    pub review_assertions_total: Option<u32>,
    pub critical_findings: Option<u32>,

    /// From `receipts.json` if Done. Always None for non-Done states.
    pub judge_verdict: Option<String>,
    pub judge_model: Option<String>,
}

/// Full detail view of one task — the response of `alps show <id>`.
///
/// `prompt`, `plan`, `review`, `receipts`, `feedback`, `failure`,
/// `implementation` are the parsed typed structs from the on-disk
/// artifacts; any of them may be `None` if the artifact file doesn't
/// exist (which is normal — they only appear once the orchestrator has
/// reached that state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub summary: TaskSummary,

    /// Full prompt text (from `prompt.md`).
    pub prompt: Option<String>,

    /// Raw `Plan` struct (from `plan.json`).
    pub plan: Option<crate::domain::Plan>,

    /// Raw `Implementation` struct (from `implementation.json`).
    pub implementation: Option<crate::domain::Implementation>,

    /// Raw `Review` struct (from `review.json`).
    pub review: Option<crate::domain::Review>,

    /// Raw `Receipts` struct (from `receipts.json`).
    pub receipts: Option<crate::receipt::Receipts>,

    /// Raw `Feedback` struct (from `feedback.json`).
    pub feedback: Option<crate::domain::Feedback>,

    /// Raw `FailureReason` enum (from `failure.json`).
    pub failure: Option<crate::task::FailureReason>,
}

/// Top-level wrapper for `alps list` JSON output.
///
/// Stable shape: always `{ "workdir": "...", "tasks": [...] }`. The
/// wrapper exists so the GUI can validate the workdir it asked about
/// against the one the server actually scanned (handy when the GUI
/// surfaces a stale path).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskList {
    pub workdir: String,
    pub tasks: Vec<TaskSummary>,
}

/// What `alps show <id>` returns when the task ID doesn't exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNotFound {
    pub task_id: String,
    pub workdir: String,
    /// Suggested fix for the GUI: the closest existing task ID, if any.
    pub suggestion: Option<String>,
}
