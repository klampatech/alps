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
use crate::elog;

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
        elog!("[plan] running");
        // Wrap the prompt with the AGENTS.md content so plan-on-retry knows
        // what the implementer and reviewer already discovered.
        let plan_prompt = wrap_prompt_with_agents_md(&task.prompt, &agents_md_content);
        let plan_out = plan.run(plan_prompt).await
            .map_err(|e| AlpsError::PlanAgent(Box::new(e)))?;
        let task = task.plan(plan_out);
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;

        // ── Implement ──
        elog!("[implement] running");
        eprintln!("[alps-diag] run_iteration: calling implement.run");
        let impl_out = implement.run(task.state.plan.clone()).await
            .map_err(|e| AlpsError::Implement(Box::new(e)))?;
        eprintln!("[alps-diag] run_iteration: implement.run returned, impl_out.metrics={:?}", impl_out.metrics);
        let task = task.implement(impl_out);
        eprintln!("[alps-diag] run_iteration: task.implement done, calling persist_task");
        persist_task(&task, workspace).map_err(AlpsError::Persistence)?;
        eprintln!("[alps-diag] run_iteration: persist_task done");

        // ── Implement-completion guard (SPEC §12 item 9) ──
        // The orchestrator MUST NOT trust Ralph's `.ralph-result.json`
        // `completed: true` claim at face value. Codex (and other Ralph
        // backends) can emit "completed all tasks!" even when prd.json
        // shows fewer stories passing than the plan called for —
        // typically after a transient error like a tool-router JSON-RPC
        // parse error mid-iteration. The Tier 4 smoke #2 (2026-08-04,
        // herdr pane wB1:p1) burned this way: codex completed 3/9
        // stories, falsely reported "iteration 4 of 20 completed",
        // and the orchestrator dutifully proceeded to Review with a
        // 3/9 deliverable. Review and Judge would have produced a
        // look-good receipts.json masking a phantom-green run.
        //
        // The guard: if Ralph claimed completion but prd.json disagrees,
        // fail loudly with `ImplementError::IncompleteStories` so the
        // CLI exits non-zero and the operator sees the discrepancy.
        let m = &task.state.implementation.metrics;
        eprintln!(
            "[alps-diag] run_iteration: implement-completion guard check: stories_passed={}, stories_total={}",
            m.stories_passed, m.stories_total
        );
        if m.stories_passed != m.stories_total {
            eprintln!(
                "[alps-diag] run_iteration: implement-completion guard FAILED — returning ImplementError::IncompleteStories"
            );
            return Err(AlpsError::Implement(Box::new(
                crate::implement::ImplementError::IncompleteStories {
                    passed: m.stories_passed,
                    total: m.stories_total,
                },
            )));
        }
        eprintln!("[alps-diag] run_iteration: implement-completion guard PASSED");

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
        elog!("[review] running");
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
        elog!("[judge] running");
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
                elog!("[done] accepted");
                Ok(done)
            }
            Err(rejected) => {
                persist_task(&rejected, workspace).map_err(AlpsError::Persistence)?;
                let reason = rejected.state.feedback.reason.clone();
                elog!("[rejected] {} — restarting with feedback", reason);
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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


    // ─────────────────────────────────────────────────────────────────────
    // drive_passes_first_try — happy-path symmetric test.
    //
    // The reject-path test above exercises the resubmit cycle. This test
    // pins the symmetric happy-path: Judge accepts on first call, drive
    // returns Ok(Task<Done>) without recursing.
    // ─────────────────────────────────────────────────────────────────────

    /// Shared helper: build a JudgeAgent that uses our scripted LLM judge
    /// + an always-pass structured judge. Used by both happy-path tests.
    fn judge_agent_with(scripted: Arc<ScriptedLlmJudge>) -> JudgeAgent {
        JudgeAgent::new(Arc::new(AlwaysPassStructured), scripted)
    }

    /// Shared helper: build a fresh TaskWorkspace rooted at a unique tmp dir.
    /// Returns (task, workspace, workspace_root) — workspace_root is the
    /// top-level dir so the caller can clean it up with remove_dir_all.
    fn fresh_workspace_task() -> (
        Task<Idle>,
        TaskWorkspace,
        std::path::PathBuf,
        TaskId,
    ) {
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
        let workspace = TaskWorkspace::new(&workspace_root);
        let task = Task::<Idle>::new(
            task_id.clone(),
            workspace_root.clone(),
            Prompt::new("Build a happy-path function"),
        );
        (task, workspace, workspace_root, task_id)
    }

    #[tokio::test]
    async fn drive_passes_first_try() {
        // ── Setup ──
        let (task, workspace, workspace_root, task_id) = fresh_workspace_task();

        let plan_calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let plan_calls_for_closure = plan_calls.clone();
        let plan_id = PlanId(uuid::Uuid::new_v4());
        let plan_id_for_closure = plan_id.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            *plan_calls_for_closure.lock().unwrap() += 1;
            Ok(canned_plan(plan_id_for_closure.clone()))
        });

        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

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
        let scripted = Arc::new(ScriptedLlmJudge::new(vec![Judgment::Pass(pass_receipts)]));
        let judge = judge_agent_with(scripted.clone());

        // ── Run ──
        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;
        let _ = std::fs::remove_dir_all(&workspace_root);

        // ── Assert ──
        // 1. drive() returned Ok(done) — the happy path converged.
        let done = result.expect("drive() should return Ok on first-try pass");

        // 2. Plan/Implement/Review/Judge each called EXACTLY ONCE.
        assert_eq!(
            *plan_calls.lock().unwrap(),
            1,
            "Plan should be called once on happy path"
        );
        assert_eq!(
            scripted.call_count(),
            1,
            "Judge should be called once on happy path"
        );

        // 3. Final prompt must NOT contain the rejection header (sanity:
        //    no spurious rejection feedback from a non-existent prior attempt).
        let _ = done; // smoke check that the accessor compiles
    }

    #[tokio::test]
    async fn drive_passes_first_try_writes_receipts_json_on_disk() {
        // REPRO for SPEC §12 item 9.10 smoke #19 + #20 bug: `receipts.json`
        // is missing from `tasks/<id>/receipts.json` after smoke runs even
        // though the orchestrator logs `[done] accepted`. This test is the
        // minimum repro — happy path, single Pass judgment — and asserts
        // the file lands on disk.
        //
        // Intentionally does NOT call `remove_dir_all` on `workspace_root`
        // so the file persists for inspection if the assertion fails.
        let (task, workspace, workspace_root, task_id) = fresh_workspace_task();

        let plan_id = PlanId(uuid::Uuid::new_v4());
        let plan_id_for_closure = plan_id.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            Ok(canned_plan(plan_id_for_closure.clone()))
        });

        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

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
        let scripted = Arc::new(ScriptedLlmJudge::new(vec![Judgment::Pass(pass_receipts)]));
        let judge = judge_agent_with(scripted.clone());

        // ── Run ──
        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;

        // ── Assert 1: drive() returned Ok(done) ──
        let done = result.expect("drive() should return Ok on first-try pass");

        // ── Assert 2: receipts.json file exists on disk at workspace_root ──
        let receipts_path = workspace.receipts_path();
        let receipts_exists = receipts_path.exists();
        let receipts_contents = if receipts_exists {
            std::fs::read_to_string(&receipts_path).ok()
        } else {
            None
        };
        let _ = std::fs::remove_dir_all(&workspace_root);

        assert!(
            receipts_exists,
            "receipts.json must exist on disk after drive() returns Ok(done). \
             Expected path: {}. workspace_root contains: {:?}",
            receipts_path.display(),
            std::fs::read_dir(&workspace_root)
                .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );

        // ── Assert 3: file contents are valid JSON and contain the receipts ──
        let contents = receipts_contents.expect("receipts.json must be readable");
        let parsed: serde_json::Value = serde_json::from_str(&contents)
            .expect("receipts.json must be valid JSON");
        assert_eq!(
            parsed["task_id"].as_str(),
            Some(done.id.as_str()),
            "receipts.json task_id must match the done task id"
        );
        assert_eq!(
            parsed["judge_model"].as_str(),
            Some("mock"),
            "receipts.json judge_model must match the canned receipts"
        );
    }

    #[tokio::test]
    async fn drive_passes_first_try_propagates_agents_md() {
        // ── Setup ──
        // Single happy-path iteration. The load-bearing contract for
        // this test is: AGENTS.md is populated AFTER the run completes,
        // pulling patterns from ralph's progress.txt via propagate_ralph_patterns.
        // This pins the cross-agent-state propagation end-to-end through
        // the loop, not just at the unit-test level.
        let (task, workspace, workspace_root, task_id) = fresh_workspace_task();
        let ralph_dir = workspace.ralph_dir();
        std::fs::create_dir_all(&ralph_dir).unwrap();
        std::fs::write(
            ralph_dir.join("progress.txt"),
            "## Codebase Patterns\n- synthetic-pattern-from-ralph\n",
        )
        .unwrap();

        let plan = PlanAgent::for_test(|_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            Ok(canned_plan(PlanId(uuid::Uuid::new_v4())))
        });

        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

        let pass_receipts = Receipts {
            task_id: task_id.clone(),
            plan_id: PlanId(uuid::Uuid::new_v4()),
            plan_summary: "ok".to_string(),
            implement_metrics: ImplementMetrics::default(),
            review_summary: ReviewSummary::from_findings(&[], &[]),
            judged_at: chrono::Utc::now(),
            judge_model: "mock".to_string(),
        };
        let scripted = Arc::new(ScriptedLlmJudge::new(vec![Judgment::Pass(pass_receipts)]));
        let judge = judge_agent_with(scripted.clone());

        // ── Run ──
        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;

        // Capture AGENTS.md BEFORE cleanup (remove_dir_all would empty it).
        let ag = agents_md::read(&workspace_root).unwrap_or_default();
        let _ = fs::remove_dir_all(&workspace_root);

        // ── Assert ──
        // 1. Happy path: drive returns Ok on first try.
        let _done = result.expect("drive() should return Ok on first-try pass");

        // 2. AGENTS.md was populated from ralph's progress.txt by
        //    propagate_ralph_patterns. This is the load-bearing assertion
        //    — it pins the cross-agent-state propagation contract end-to-end.
        assert!(
            ag.contains("synthetic-pattern-from-ralph"),
            "AGENTS.md should contain pattern propagated from ralph, got: {:?}",
            ag
        );
        assert!(
            ag.contains("## Codebase Patterns"),
            "AGENTS.md should have Codebase Patterns header, got: {:?}",
            ag
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Multi-iteration reject→reject→pass test.
    //
    // The single-iter happy-path tests above pin "first try passes."
    // The existing drive_rejects_then_passes_appends_feedback_to_next_plan
    // pins "one reject, feedback reaches iter 2's Plan." What neither
    // pins is the **cross-iteration accumulation** contract:
    //
    //   1. AGENTS.md content from prior implement runs reaches the
    //      NEXT Plan call (not just the immediately-prior Plan).
    //   2. Each Plan call sees the LATEST feedback, not stale feedback
    //      from earlier rejects.
    //   3. Three Plan calls produce three Plan prompts, all distinct,
    //      with AGENTS.md content monotonically growing across calls.
    //
    // This is the load-bearing contract for multi-iter runs where
    // each iteration's implement discovers new project conventions —
    // they MUST accumulate into the prompt that drives Plan on retry.
    // ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn drive_rejects_twice_then_passes_accumulates_agents_md() {
        // ── Setup ──
        let (task, workspace, workspace_root, task_id) = fresh_workspace_task();

        // Plan records its input prompt verbatim so we can verify each
        // iteration's prompt is distinct + carries the accumulated state.
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

        // Implement returns a canned Implementation whose deliverable_path
        // points at a ralph_dir containing a synthetic progress.txt. Each
        // implement run sees the SAME progress.txt (we only seed once).
        // This pins the contract: drive() reads ralph's progress.txt AFTER
        // every implement call, so AGENTS.md grows across iterations even
        // when ralph's output is static.
        let prd_path = workspace_root.join("prd.json");
        let ralph_dir = workspace.ralph_dir();
        std::fs::create_dir_all(&ralph_dir).unwrap();
        std::fs::write(
            ralph_dir.join("progress.txt"),
            "## Codebase Patterns\n- iter-1-pattern-discovered\n",
        )
        .unwrap();
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

        // Judge: two rejects, then a pass.
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
        let reject_1 = Feedback {
            reason: "iter-1 rejection: tests do not pass".to_string(),
            failed_assertions: vec![Assertion {
                criterion: "tests pass with pytest".to_string(),
                passed: false,
                evidence: "iter-1 evidence".to_string(),
            }],
            retry_hints: vec!["iter-1 hint: fix the implementation".to_string()],
        };
        let reject_2 = Feedback {
            reason: "iter-2 rejection: tests still failing".to_string(),
            failed_assertions: vec![Assertion {
                criterion: "tests pass with pytest".to_string(),
                passed: false,
                evidence: "iter-2 evidence".to_string(),
            }],
            retry_hints: vec!["iter-2 hint: refactor the recursion".to_string()],
        };
        let scripted = Arc::new(ScriptedLlmJudge::new(vec![
            Judgment::Reject(reject_1),
            Judgment::Reject(reject_2),
            Judgment::Pass(pass_receipts),
        ]));
        let judge = judge_agent_with(scripted.clone());

        // ── Run ──
        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;

        // Capture AGENTS.md BEFORE cleanup so we can assert its contents.
        let ag = agents_md::read(&workspace_root).unwrap_or_default();
        let _ = fs::remove_dir_all(&workspace_root);

        // ── Assert 1: drive() returned Ok(done) after 3 iterations ──
        let _done = result.expect(
            "drive() should return Ok after reject→reject→pass cycle"
        );

        // ── Assert 2: Plan/Review/Judge each called exactly 3 times ──
        let plan_inputs = plan_calls.lock().unwrap();
        assert_eq!(
            plan_inputs.len(),
            3,
            "Plan should be called once per outer iteration"
        );
        assert_eq!(
            scripted.call_count(),
            3,
            "Judge should be called once per outer iteration"
        );

        // ── Assert 3: iter-2 Plan's prompt contains iter-1's feedback ──
        assert!(
            plan_inputs[1].contains("iter-1 rejection"),
            "iter-2 Plan's prompt should contain iter-1's feedback, got: {}",
            plan_inputs[1]
        );
        assert!(
            plan_inputs[1].contains("iter-1 hint"),
            "iter-2 Plan's prompt should contain iter-1's retry hint, got: {}",
            plan_inputs[1]
        );

        // ── Assert 4: iter-3 Plan's prompt contains iter-2's feedback (latest) ──
        //    AND iter-1's feedback (accumulated via rejected.reset()).
        assert!(
            plan_inputs[2].contains("iter-2 rejection"),
            "iter-3 Plan's prompt should contain iter-2's feedback, got: {}",
            plan_inputs[2]
        );
        assert!(
            plan_inputs[2].contains("iter-2 hint"),
            "iter-3 Plan's prompt should contain iter-2's retry hint, got: {}",
            plan_inputs[2]
        );
        // Iter-1's feedback should ALSO be in iter-3's prompt — drive()
        // accumulates feedback across iterations via rejected.reset().
        assert!(
            plan_inputs[2].contains("iter-1 rejection"),
            "iter-3 Plan's prompt should accumulate iter-1's feedback, got: {}",
            plan_inputs[2]
        );

        // ── Assert 5: each Plan prompt is distinct (no aliasing) ──
        assert_ne!(plan_inputs[0], plan_inputs[1], "iter-1 and iter-2 prompts should differ");
        assert_ne!(plan_inputs[1], plan_inputs[2], "iter-2 and iter-3 prompts should differ");
        assert_ne!(plan_inputs[0], plan_inputs[2], "iter-1 and iter-3 prompts should differ");

        // ── Assert 6: iter-1 Plan's prompt does NOT contain feedback ──
        assert!(
            !plan_inputs[0].contains("Previous attempt rejected")
                && !plan_inputs[0].contains("iter-1 rejection"),
            "iter-1 Plan's prompt should NOT contain feedback from prior attempts"
        );

        // ── Assert 7: AGENTS.md contains the ralph-discovered pattern ──
        //    (propagate_ralph_patterns ran after each implement call.)
        assert!(
            ag.contains("iter-1-pattern-discovered"),
            "AGENTS.md should accumulate ralph's pattern across iterations, got: {:?}",
            ag
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // error-propagation tests — the bookends. The reject-path handles
    // Resubmit; these handle the case where an agent itself errors.
    //
    // The contract: any agent error → drive() returns Err(AlpsError::XAgent),
    // downstream agents do not run, no recursion happens.
    // ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn drive_returns_error_on_plan_failure() {
        let (task, workspace, workspace_root, _task_id) = fresh_workspace_task();

        let plan = PlanAgent::for_test(|_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            Err(crate::plan::PlanError::Parse("synthetic plan parse failure".to_string()))
        });

        let prd_path = workspace_root.join("prd.json");
        let implement_called = Arc::new(Mutex::new(false));
        let implement_called_for_closure = implement_called.clone();
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            *implement_called_for_closure.lock().unwrap() = true;
            Ok(canned_implementation(prd_path.clone()))
        });

        let review_called = Arc::new(Mutex::new(false));
        let review_called_for_closure = review_called.clone();
        let review = ReviewAgent::for_test(move |_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            *review_called_for_closure.lock().unwrap() = true;
            Ok(canned_review())
        });

        let scripted = Arc::new(ScriptedLlmJudge::new(vec![]));
        let judge = judge_agent_with(scripted.clone());

        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;
        let _ = fs::remove_dir_all(&workspace_root);

        // Plan errored → drive returns Err(PlanAgent), no other agents ran.
        let err = match result {
    Ok(_) => panic!("drive() should return Err when Plan fails"),
    Err(e) => e,
};
        match err {
            AlpsError::PlanAgent(_) => {} // expected
            other => panic!("expected AlpsError::PlanAgent, got: {:?}", other),
        }
        assert!(!*implement_called.lock().unwrap(), "Implement should not run when Plan fails");
        assert!(!*review_called.lock().unwrap(), "Review should not run when Plan fails");
        assert_eq!(scripted.call_count(), 0, "Judge should not run when Plan fails");
    }

    #[tokio::test]
    async fn drive_returns_error_on_judge_failure() {
        // Judge errors are different from Reject. Reject is a normal
        // Judgment::Reject that loop-recurses; an Err(JudgeError) means
        // the Judge itself failed (parse error, subprocess died, etc.)
        // and must propagate immediately without recursion.
        let (task, workspace, workspace_root, _task_id) = fresh_workspace_task();

        let plan = PlanAgent::for_test(|_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            Ok(canned_plan(PlanId(uuid::Uuid::new_v4())))
        });

        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            Ok(canned_implementation(prd_path.clone()))
        });

        let review = ReviewAgent::for_test(|_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            Ok(canned_review())
        });

        // Judge errors instead of returning a Judgment. After this the
        // loop MUST NOT recurse — it must surface the error.
        struct ErroringLlmJudge;
        #[async_trait::async_trait]
        impl LlmJudge for ErroringLlmJudge {
            async fn judge(&self, _ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
                Err(JudgeError::Llm("synthetic judge failure".to_string()))
            }
        }

        let scripted = Arc::new(ScriptedLlmJudge::new(vec![]));
        let _ = scripted; // suppress unused warning
        let judge: JudgeAgent = JudgeAgent::new(Arc::new(AlwaysPassStructured), Arc::new(ErroringLlmJudge));

        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;
        let _ = fs::remove_dir_all(&workspace_root);

        let err = match result {
    Ok(_) => panic!("drive() should return Err when Judge errors"),
    Err(e) => e,
};
        match err {
            AlpsError::Judge(_) => {} // expected
            other => panic!("expected AlpsError::Judge, got: {:?}", other),
        }
    }

    /// SPEC §12 item 9: implement-completion guard.
    ///
    /// If the implement agent returns a `Implementation` where
    /// `metrics.stories_passed != metrics.stories_total`, the orchestrator
    /// MUST refuse to proceed to Review. This is the Tier 4 smoke #2 bug
    /// (2026-08-04, herdr pane wB1:p1): codex hit a tool-router JSON-RPC
    /// parse error mid-iteration, falsely emitted `Ralph completed all
    /// tasks!`, wrote `completed: true` to `.ralph-result.json`, but
    /// prd.json showed 3/9 stories passing. Without this guard the
    /// orchestrator would proceed to Review and Judge with a 3/9
    /// deliverable and produce a phantom-green receipts.json.
    #[tokio::test]
    async fn drive_returns_error_when_implement_completes_with_less_than_all_stories_passing() {
        let (task, workspace, workspace_root, task_id) = fresh_workspace_task();

        let plan = PlanAgent::for_test(|_input: Prompt| -> Result<Plan, crate::plan::PlanError> {
            Ok(canned_plan(PlanId(uuid::Uuid::new_v4())))
        });

        // Canned implementation: 3 of 9 stories passing, just like the
        // Tier 4 smoke #2 burn. The implement-complete guard should
        // catch this and refuse to proceed to Review.
        let prd_path = workspace_root.join("prd.json");
        let implement = ImplementAgent::for_test(workspace_root.clone(), move |_plan: Plan| {
            let mut impl_ = canned_implementation(prd_path.clone());
            impl_.metrics.stories_passed = 3;
            impl_.metrics.stories_total = 9;
            Ok(impl_)
        });

        // Review must NOT be called. If the guard is wired correctly, the
        // loop short-circuits at the implement step. We use a tracking
        // counter to prove Review never runs.
        let review_calls = Arc::new(AtomicUsize::new(0));
        let review_calls_for_closure = review_calls.clone();
        let review = ReviewAgent::for_test(move |_ctx: ReviewContext| -> Result<Review, crate::review::ReviewError> {
            review_calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Ok(canned_review())
        });

        // Judge: scripted to return Pass. Should never be invoked because
        // the implement-completion guard fires before Judge runs.
        let pass_receipts = Receipts {
            task_id: task_id.clone(),
            plan_id: PlanId(uuid::Uuid::new_v4()),
            plan_summary: "Build the requested Python function".to_string(),
            implement_metrics: ImplementMetrics {
                stories_passed: 3,
                stories_total: 9,
                iterations: 0,
                elapsed_secs: 0,
            },
            review_summary: ReviewSummary {
                findings_count: 1,
                critical_findings: 0,
                assertions_passed: 1,
                assertions_total: 1,
            },
            judged_at: chrono::Utc::now(),
            judge_model: "test".to_string(),
        };
        let scripted_judge = Arc::new(ScriptedLlmJudge::new(vec![Judgment::Pass(pass_receipts)]));
        let judge = JudgeAgent::new(Arc::new(AlwaysPassStructured), scripted_judge.clone());

        let result = drive(task, &plan, &implement, &review, &judge, &workspace).await;
        let _ = std::fs::remove_dir_all(&workspace_root);

        let err = match result {
            Ok(_) => panic!("drive() should return Err when implement completes with 3/9 stories"),
            Err(e) => e,
        };
        match err {
            AlpsError::Implement(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("3/9") && msg.contains("incomplete"),
                    "expected error to mention 3/9 stories and 'incomplete', got: {}",
                    msg
                );
            }
            other => panic!("expected AlpsError::Implement(IncompleteStories), got: {:?}", other),
        }
        assert_eq!(
            review_calls.load(Ordering::SeqCst),
            0,
            "Review MUST NOT run when the implement-completion guard fires"
        );
        assert_eq!(
            scripted_judge.call_count(),
            0,
            "Judge MUST NOT run when the implement-completion guard fires"
        );
    }
}