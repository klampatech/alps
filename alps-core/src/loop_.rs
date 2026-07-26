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
use std::pin::Pin;

use crate::agent::Agent;
use crate::error::AlpsError;
use crate::implement::ImplementAgent;
use crate::judge::{JudgeAgent, JudgeContext};
use crate::persistence::{TaskWorkspace, persist_task};
use crate::plan::PlanAgent;
use crate::review::ReviewAgent;
use crate::task::*;

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

        // ── Plan ──
        eprintln!("[plan] running");
        let plan_out = plan.run(task.prompt.clone()).await
            .map_err(|e| AlpsError::PlanAgent(Box::new(e)))?;
        let task = task.plan(plan_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Implement ──
        eprintln!("[implement] running");
        let impl_out = implement.run(task.state.plan.clone()).await
            .map_err(|e| AlpsError::Implement(Box::new(e)))?;
        let task = task.implement(impl_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Review ──
        eprintln!("[review] running");
        let review_out = review.run(task.state.implementation.clone()).await
            .map_err(|e| AlpsError::ReviewAgent(Box::new(e)))?;
        let task = task.review(review_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Judge ──
        eprintln!("[judge] running");
        let ctx = JudgeContext {
            plan: task.state.plan.clone(),
            implementation: task.state.implementation.clone(),
            review: task.state.review.clone(),
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
