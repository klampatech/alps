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

    // ─────────────────────────────────────────────────────────────────────
    // drive_* integration tests — verify the reject-resubmit cycle.
    //
    // These use the for_test constructors on the 4 agents (cfg(test) only)
    // to deterministically exercise run_iteration's recursion on Judge
    // Reject without spawning real Claude/Codex. The key invariant:
    // when the Judge rejects, the next iteration's Plan prompt must
    // contain the feedback from the rejection.
    // ─────────────────────────────────────────────────────────────────────

    use crate::domain::{
        Assertion, Artifact, Commit, DefinitionOfDone, Feedback, Finding, Implementation,
        Judgment, Plan, PlanId, Review, Severity, StoryId, TaskId, UserStory,
    };
    use crate::implement::ImplementAgent;
    use crate::judge::{
        JudgeAgent, JudgeContext, JudgeError, LlmJudge, StructuredJudge, StructuredResult,
    };
    use crate::plan::PlanAgent;
    use crate::receipt::{ImplementMetrics, Receipts, ReviewSummary};
    use crate::review::{ReviewAgent, ReviewContext};
    use std::sync::{Arc, Mutex};

    /// A canned Plan that matches a "build a Python function" prompt.
    fn canned_plan(plan_id: PlanId) -> Plan {
        Plan {
            id: plan_id,
            goal: "Build the requested Python function".to_string(),
            architecture: "Single module with one function".to_string(),
            stories: vec![UserStory {
                id: StoryId("US-001".to_string()),
                title: "Implement the function".to_string(),
                description: "Implement the requested function per spec".to_string(),
                acceptance_criteria: vec!["function returns correct value".to_string()],
                priority: 1,
            }],
            dod: vec![DefinitionOfDone {
                criterion: "tests pass with pytest".to_string(),
                verifiable: true,
            }],
        }
    }

    /// A canned Implementation that pairs with canned_plan.
    fn canned_implementation(prd_path: std::path::PathBuf) -> Implementation {
        // The deliverable is conventionally the ralph nested workspace
        // (tests construct the prd_path there). Use a hardcoded fallback
        // for the test fixture so we don't double-consume prd_path.
        let deliverable_path = prd_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        Implementation {
            ralph_branch: "alps/test".to_string(),
            prd_path,
            commits: vec![Commit {
                sha: "abc123".to_string(),
                message: "feat: implement".to_string(),
            }],
            artifacts: vec![Artifact {
                path: "fib.py".into(),
                kind: crate::domain::ArtifactKind::Other("python".to_string()),
            }],
            metrics: ImplementMetrics::default(),
            deliverable_path,
        }
    }

    /// A canned Review that always has 0 critical findings, 1 warning,
    /// 1 passing assertion.
    fn canned_review() -> Review {
        Review {
            findings: vec![Finding {
                severity: Severity::Warning,
                description: "docstring could be improved".to_string(),
                evidence: "missing parameter docs".to_string(),
            }],
            assertions: vec![Assertion {
                criterion: "tests pass".to_string(),
                passed: true,
                evidence: "pytest output".to_string(),
            }],
        }
    }

    /// Sequential LLM Judge that returns each pre-loaded Judgment in order.
    /// After the queue is exhausted, it returns the last judgment.
    /// Records every call so tests can verify call count + input ctx.
    struct ScriptedLlmJudge {
        script: Mutex<Vec<Judgment>>,
        calls: Mutex<Vec<JudgeContext>>,
    }

    impl ScriptedLlmJudge {
        fn new(script: Vec<Judgment>) -> Self {
            Self {
                script: Mutex::new(script),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl LlmJudge for ScriptedLlmJudge {
        async fn judge(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
            self.calls.lock().unwrap().push(ctx.clone());
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                // Should not happen in well-formed tests
                Err(JudgeError::Llm("script exhausted".to_string()))
            } else {
                Ok(script.remove(0))
            }
        }
    }

    /// Always-pass structured judge.
    struct AlwaysPassStructured;
    #[async_trait::async_trait]
    impl StructuredJudge for AlwaysPassStructured {
        async fn check(&self, _ctx: &JudgeContext) -> Result<StructuredResult, JudgeError> {
            Ok(StructuredResult {
                all_pass: true,
                failed: vec![],
            })
        }
    }

    #[tokio::test]
    async fn drive_rejects_then_passes_appends_feedback_to_next_plan() {
        // ── Setup ──
        let task_id = TaskId::new();
        let workspace_root = std::env::temp_dir().join(format!(
            "alps-drive-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace_root).unwrap();
        let workspace_root_for_impl = workspace_root.clone();
        let workspace = TaskWorkspace::new(&workspace_root);

        // Track Plan invocations to verify the second Plan sees the feedback.
        let plan_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let plan_calls_for_closure = plan_calls.clone();
        let plan_id = PlanId(uuid::Uuid::new_v4());
        let plan_id_for_closure = plan_id.clone();
        let plan = PlanAgent::for_test(move |input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            plan_calls_for_closure
                .lock()
                .unwrap()
                .push(input.as_str().to_string());
            Ok(canned_plan(plan_id_for_closure.clone()))
        });

        // Implement just returns a canned implementation pointing at a fake
        // prd.json (needed for the type but not actually read in this test).
        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root_for_impl, move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        // Review returns the canned review (no critical findings).
        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

        // Judge: first call rejects, second call passes.
        let pass_receipts = Receipts {
            task_id: task_id.clone(),
            plan_id,
            plan_summary: "Build the requested Python function".to_string(),
            implement_metrics: ImplementMetrics::default(),
            review_summary: ReviewSummary {
                findings_count: 1,
                critical_findings: 0,
                assertions_passed: 1,
                assertions_total: 1,
            },
            judged_at: chrono::Utc::now(),
            judge_model: "mock".to_string(),
        };
        let reject_feedback = Feedback {
            reason: "tests do not pass: 0 passed, 1 failed".to_string(),
            failed_assertions: vec![Assertion {
                criterion: "tests pass with pytest".to_string(),
                passed: false,
                evidence: "AssertionError: assert fib(10) == [...]".to_string(),
            }],
            retry_hints: vec!["fix the implementation so fib(10) matches the expected list".to_string()],
        };
        let scripted = Arc::new(ScriptedLlmJudge::new(vec![
            Judgment::Reject(reject_feedback),
            Judgment::Pass(pass_receipts),
        ]));
        let judge = JudgeAgent::new(Arc::new(AlwaysPassStructured), scripted.clone());

        // ── Run ──
        let task = Task::<crate::task::Idle>::new(
            task_id,
            workspace_root.clone(),
            Prompt::new("Build a fib function"),
        );
        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;
        let _ = std::fs::remove_dir_all(&workspace_root);

        // ── Assert ──
        // 1. drive() returned Ok(done) — the reject-resubmit cycle worked.
        let done = result.expect("drive() should return Ok after reject→pass cycle");
        // The type-state attempt counter resets on rejected.reset(), so we
        // verify the iteration count via plan_calls instead. The Task<Done>
        // attempts() reflects the FINAL iteration's attempt number, which
        // here is 1 (from the second plan call).
        let _ = done.attempts(); // smoke check that the accessor compiles

        // 2. Judge was called exactly TWICE (once for the reject, once for the pass).
        assert_eq!(scripted.call_count(), 2, "Judge should be called once per iteration");

        // 3. Plan was called exactly TWICE — this is the key invariant
        //    proving the loop recursed on the Judge Reject.
        let plan_inputs = plan_calls.lock().unwrap();
        assert_eq!(plan_inputs.len(), 2, "Plan should be called once per iteration");

        // 4. The SECOND Plan's prompt must contain the feedback from the first reject.
        let second_plan_prompt = &plan_inputs[1];
        assert!(
            second_plan_prompt.contains("Previous attempt rejected"),
            "second Plan's prompt should contain rejection feedback, got: {}",
            second_plan_prompt
        );
        assert!(
            second_plan_prompt.contains("tests do not pass"),
            "second Plan's prompt should contain the specific reason, got: {}",
            second_plan_prompt
        );
        assert!(
            second_plan_prompt.contains("fix the implementation"),
            "second Plan's prompt should contain retry hints, got: {}",
            second_plan_prompt
        );
        // The original prompt must still be there too (rejected.reset prepends feedback)
        assert!(
            second_plan_prompt.contains("Build a fib function"),
            "second Plan's prompt should still contain the original prompt"
        );

        // 5. The FIRST Plan's prompt should NOT have the feedback (sanity).
        let first_plan_prompt = &plan_inputs[0];
        assert!(
            !first_plan_prompt.contains("Previous attempt rejected"),
            "first Plan's prompt should not have feedback (it was the original run)"
        );
    }
}
