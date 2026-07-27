//! The outer loop driver for ALPS.
//!
//! Drives a `Task<Idle>` through Plan → Implement → Review → Judge.
//! On Judge reject, feedback is appended to the prompt and the loop
//! restarts from the plan step. The loop is **unbounded** — "brute force
//! development" — until the Judge accepts (resolves 2026-07-26).
//!
//! **Type-state note**: variables in Rust can't change type. We use
//! `let task = task.method(...)` shadowing to advance the state, and
//! a recursive function (`run_iteration`) to handle the rejection loop.
//!
//! See `SPEC.md` §8.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::agent::Agent;
use crate::error::AlpsError;
use crate::implement::ImplementAgent;
use crate::judge::{JudgeAgent, JudgeContext};
use crate::persistence::{TaskWorkspace, persist_task};
use crate::plan::PlanAgent;
use crate::review::{ReviewAgent, ReviewContext};
use crate::task::*;
use crate::{agents_md, domain::Prompt};

/// Drive the outer loop until Judge passes.
///
/// Agent errors bubble up as `AlpsError`. The CLI catches them and exits non-zero.
/// The loop never returns `Rejected` — it always either returns `Done` or errors.
pub async fn drive(
    task: Task<Idle>,
    plan: &PlanAgent,
    implement: &ImplementAgent,
    review: &ReviewAgent,
    judge: &JudgeAgent,
    workspace: &TaskWorkspace,
) -> Result<Task<Done>, AlpsError> {
    run_iteration(task, plan, implement, review, judge, workspace).await
}

/// One iteration of the outer loop. Recurses on reject.
fn run_iteration<'a>(
    task: Task<Idle>,
    plan: &'a PlanAgent,
    implement: &'a ImplementAgent,
    review: &'a ReviewAgent,
    judge: &'a JudgeAgent,
    workspace: &'a TaskWorkspace,
) -> Pin<Box<dyn Future<Output = Result<Task<Done>, AlpsError>> + Send + 'a>> {
    Box::pin(async move {
        // Initial persist
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── AGENTS.md: read what previous agents have learned ──
        // On the first iteration, the file is empty (implement hasn't run).
        // On retries, it contains patterns from prior implement + review + judge.
        let mut agents_md_content = agents_md::read(&workspace.root)
            .map_err(|e| AlpsError::Persistence(crate::persistence::PersistenceError::Io(
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )))?;

        // ── Plan ──
        eprintln!("[plan] running");
        // Wrap the prompt with the AGENTS.md content so plan-on-retry knows
        // what the implementer and reviewer already discovered.
        let plan_prompt = wrap_prompt_with_agents_md(&task.prompt, &agents_md_content);
        let plan_out = plan.run(plan_prompt).await
            .map_err(|e| AlpsError::PlanAgent(Box::new(e)))?;
        let task = task.plan(plan_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Implement ──
        eprintln!("[implement] running");
        let impl_out = implement.run(task.state.plan.clone()).await
            .map_err(|e| AlpsError::Implement(Box::new(e)))?;
        let task = task.implement(impl_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── AGENTS.md: extract patterns from ralph's progress.txt ──
        // Ralph writes `## Codebase Patterns` to progress.txt as it discovers
        // project conventions. We propagate that into the task-level AGENTS.md
        // so the Review and Judge agents (and Plan on retry) can see them.
        propagate_ralph_patterns(&workspace.ralph_dir(), &workspace.root)
            .map_err(|e| AlpsError::Persistence(crate::persistence::PersistenceError::Io(
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )))?;
        // Re-read the now-augmented AGENTS.md so the review/judge steps see
        // the freshest content.
        agents_md_content = agents_md::read(&workspace.root)
            .map_err(|e| AlpsError::Persistence(crate::persistence::PersistenceError::Io(
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            )))?;

        // ── Review ──
        eprintln!("[review] running");
        let review_ctx = ReviewContext {
            plan: task.state.plan.clone(),
            implementation: task.state.implementation.clone(),
            agents_md: agents_md_content.clone(),
        };
        let review_out = review.run(review_ctx).await
            .map_err(|e| AlpsError::ReviewAgent(Box::new(e)))?;
        let task = task.review(review_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Judge ──
        eprintln!("[judge] running");
        let ctx = JudgeContext {
            task_id: task.id.clone(),
            plan: task.state.plan.clone(),
            implementation: task.state.implementation.clone(),
            review: task.state.review.clone(),
            agents_md: agents_md_content,
        };
        let judgment = judge.run(ctx).await
            .map_err(|e| AlpsError::Judge(Box::new(e)))?;

        match task.judge(judgment) {
            Ok(done) => {
                persist_task(&done, workspace).map_err(AlpsError::Persistence)?;
                eprintln!("[done] accepted");
                Ok(done)
            }
            Err(rejected) => {
                persist_task(&rejected, workspace).map_err(AlpsError::Persistence)?;
                let reason = rejected.state.feedback.reason.clone();
                eprintln!("[rejected] {} — restarting with feedback", reason);
                let next = rejected.reset(vec![]);
                run_iteration(next, plan, implement, review, judge, workspace).await
            }
        }
    })
}

/// Extract patterns from ralph's progress.txt and append them to the task-level
/// AGENTS.md. Idempotent: appending a section we already appended is fine
/// (the file accumulates learnings), but we don't want to duplicate identical
/// content within a single run.
///
/// Currently we just append unconditionally — the file is the source of truth
/// and human-readable; duplicates are tolerable. Future: detect duplicates.
fn propagate_ralph_patterns(
    ralph_dir: &Path,
    task_dir: &Path,
) -> Result<(), agents_md::AgentsMdError> {
    let patterns = agents_md::extract_patterns(ralph_dir)?;
    if !patterns.is_empty() {
        agents_md::append(task_dir, &patterns)?;
    }
    Ok(())
}

/// Wrap the prompt with the AGENTS.md content if any. The Plan agent already
/// has a system prompt that expects the user prompt after `USER PROMPT:`; we
/// append the AGENTS.md block above the original user prompt so plan sees
/// both the new request and the accumulated context.
pub(crate) fn wrap_prompt_with_agents_md(prompt: &Prompt, agents_md_content: &str) -> Prompt {
    if agents_md_content.trim().is_empty() {
        return prompt.clone();
    }
    let wrapped = format!(
        "{}\n\n---\n\n## Codebase Patterns (from prior implement / review / judge)\n\n{}\n",
        prompt.as_str(),
        agents_md_content,
    );
    Prompt::new(wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Prompt;
    use std::fs;

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::path::PathBuf::from(format!(
            "/tmp/alps-loop-test-{}-{}{}",
            label, pid, nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn propagate_ralph_patterns_extracts_and_appends() {
        // Simulate a ralph run that wrote patterns to its progress.txt.
        // The loop should extract them and append to the task-level AGENTS.md.
        let ralph_dir = unique_dir("ralph");
        fs::write(
            ralph_dir.join("progress.txt"),
            "# Ralph Progress Log\n## Codebase Patterns\n- use thiserror for errors\n- never panic in library code\n",
        )
        .unwrap();
        let task_dir = unique_dir("task");

        propagate_ralph_patterns(&ralph_dir, &task_dir).unwrap();

        let ag = agents_md::read(&task_dir).unwrap();
        assert!(ag.contains("use thiserror for errors"));
        assert!(ag.contains("never panic in library code"));
        assert!(ag.contains("## Codebase Patterns"));

        let _ = fs::remove_dir_all(&ralph_dir);
        let _ = fs::remove_dir_all(&task_dir);
    }

    #[test]
    fn propagate_ralph_patterns_no_op_when_no_progress() {
        // If ralph didn't write progress.txt (e.g. crash before first story),
        // the task AGENTS.md should stay empty/absent.
        let ralph_dir = unique_dir("ralph-empty");
        let task_dir = unique_dir("task-empty");
        propagate_ralph_patterns(&ralph_dir, &task_dir).unwrap();
        let ag = agents_md::read(&task_dir).unwrap();
        assert!(ag.is_empty(), "expected no AGENTS.md, got: {:?}", ag);
        let _ = fs::remove_dir_all(&ralph_dir);
        let _ = fs::remove_dir_all(&task_dir);
    }

    #[test]
    fn wrap_prompt_with_agents_md_appends_section() {
        let p = Prompt::new("Build a fib function");
        let ag = "## Codebase Patterns\n- use foo\n";
        let wrapped = wrap_prompt_with_agents_md(&p, ag);
        let s = wrapped.as_str();
        assert!(s.contains("Build a fib function"));
        assert!(s.contains("Codebase Patterns"));
        assert!(s.contains("use foo"));
        // The agents_md section must come AFTER the original prompt so the
        // plan agent's system prompt sees the user request first.
        let prompt_pos = s.find("Build a fib function").unwrap();
        let ag_pos = s.find("Codebase Patterns").unwrap();
        assert!(prompt_pos < ag_pos, "AGENTS.md should be appended after the original prompt");
    }

    #[test]
    fn wrap_prompt_with_agents_md_returns_unchanged_when_empty() {
        // First iteration: no AGENTS.md. Don't add an empty section.
        let p = Prompt::new("Build a fib function");
        let wrapped = wrap_prompt_with_agents_md(&p, "");
        assert_eq!(wrapped.as_str(), p.as_str());
    }
}
