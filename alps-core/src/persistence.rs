//! Persistence — JSON file I/O for the task workspace.
//!
//! Each task has a directory at `tasks/<task-id>/`. State is written at
//! every transition; commits are made by the CLI.

use std::path::PathBuf;
use thiserror::Error;

use crate::domain::*;
use crate::task::*;
use crate::receipt::Receipts;

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
