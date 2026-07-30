# ALPS — Specification

> **Status**: v0.1 implemented + working; reject path verified; agent retries in place; ralph max-iterations routes through Judge
> **Author**: Kyle + Evo
> **Date**: 2026-07-26 (initial); 2026-07-27 (updated for v0.1 implementation)

## 0. What's been built since the original spec

The v0.1 spec was written before any code shipped. As of 2026-07-27, the
core loop is implemented and working end-to-end. Below is the changelog of
what landed in `klampatech/alps` between the spec and now.

| Date | Commit | What it added |
|---|---|---|
| 2026-07-26 | `0275945` | Initial Cargo scaffold + 3 Mermaid sequence diagrams (happy/reject/multi-iteration) |
| 2026-07-26 | `1435477` | Resolved 4 open questions: hybrid judge, unbounded attempts, stdout notifications, md+json receipts |
| 2026-07-26 | `21a08d4` | `loop_::drive` with type-state-safe recursion on Judge Reject |
| 2026-07-26 | `a36978f` | Plan agent wired to real Claude Code (`--dangerously-skip-permissions -p`) |
| 2026-07-26 | `6271e3a` | Implement agent wired to `scripts/ralph.sh` (Ralph as subprocess) |
| 2026-07-26 | `a6382a2` | Review agent wired (adversarial Claude Code) |
| 2026-07-26 | `670eece` | Judge agent wired (Hermes via Claude Code, JSON-only) |
| 2026-07-26 | `b4aa362` | `DoDRunner` (auto-detects Python/Rust project type, runs tests) |
| 2026-07-26 | `799067d`–`752c41a` | Codex as default Ralph tool; cwd-relative AGENTS.md fallback; COMPLETE-signal fix |
| 2026-07-26 | `4013f6d` | `.codex-last-message.txt` gitignore |
| 2026-07-26 | `2be94ab` | Real implement metrics in receipts (was zeros); silent auto-commit when no changes |
| 2026-07-27 | `f452ca3` | **Per-task git branches** (`alps/<task-id>`) + **AGENTS.md propagation** from ralph → review/judge/next-plan |
| 2026-07-27 | `46327b4` | **Workdir completion guard** — refuses re-invocation within 5s of a prior success (defensive guard against Claude TUI auto-re-running alps; `--force` bypass) |
| 2026-07-27 | `6ebaf92` | **Nested git repo exclusion** — `commit_smart` writes `<workdir>/.git/info/exclude` so the ralph nested `.git/` doesn't fatal `git add -A` on git 2.42+ |
| 2026-07-27 | `731fbd3` | **Reject path verification** — `for_test` mock-agent infrastructure + `drive_rejects_then_passes_appends_feedback_to_next_plan` integration test (deterministic, <100ms) |
| 2026-07-27 | `6a414a8` | **Plan retry-on-parse-fail** — `PlanAgent::run` retries up to `max_retries=3` total attempts on `PlanError::Parse`. Spawn/schema errors propagate immediately. Plus 5 new deterministic tests covering the contract. |
| 2026-07-27 | `af9534c` | docs: sync SPEC.md after Plan retry work |
| 2026-07-27 | `894be6b` | **Review + Judge retry-on-parse-fail** — same pattern as Plan. `ReviewAgent::run` and `HermesLlmJudge::judge` each retry up to `max_retries=3` on parse failure. Added `JudgeError::Parse` variant to distinguish parse errors from semantic errors (validate_verdict failures). Plus 11 new deterministic tests (6 review + 5 judge). |
| 2026-07-27 | `06d916d` | **Ralph max-iterations routes through reject path** — fixed `ImplementAgent::run` to read prd.json regardless of ralph exit code. Previously, ralph hitting 20-iteration safety net (exit 1) returned `ImplementError::Ralph` and the loop died. Now, the partial progress flows to Judge, which rejects, which restarts the loop with feedback. Plus 3 new integration tests using fake ralph.sh scripts (covering ralph exit 0, ralph exit 1 with partial progress, ralph exit 1 with no prd.json). |
| 2026-07-27 | `d5ea92b` | docs: sync SPEC.md after ralph max-iterations fix + multi-iter smoke |
| 2026-07-27 | `fd35ff5` | **Recursive artifact collection** — `read_artifacts` now walks `ralph_dir` recursively (was non-recursive `std::fs::read_dir`). Fixes the bug that Hermes Judge couldn't see `src/lib.rs` in the LLM review prompt because Rust source lives in a subdirectory. +1 regression test. Surfaced by the Rust DoD smoke (this §12 item 1, now completed). |
| 2026-07-30 | (no commit) | **Real reject-path smoke** — first end-to-end smoke to *complete via the reject path*. CRUD FastAPI app at `/tmp/alps-crud-demo/` (4 endpoints, stdlib sqlite3, pytest) reached `# ALPS — Done` in 4 outer-loop iterations and 3 Plan→Implement→Review→Judge round-trips, with each reject catching a distinct real defect: (1) structured DoD missing runtime verification (`pytest -q` + `uvicorn` startup were not asserted-on), (2) captured artifacts (`pytest_output.txt`, `uvicorn_startup.log`) absent + a Pydantic `ItemOut` model introduced despite the spec saying "minimal Pydantic (ItemIn only)", (3) RFC violation: FastAPI's default 204 body handling returned non-empty content under `Response(status_code=204)`. Fourth iteration accepted — 6/6 ralph stories, 11/11 review assertions, 0 critical findings. This is the closed form of §12 item 1 below. |

### Verified end-to-end

- **Happy path** — 7 successful smokes (smoke3, smoke4, smoke5, smoke6, smoke7, smoke8, smoke9) — all pass with Judge LLM verdict "pass" on the first attempt.
- **Rust DoD path** — first Rust smoke (2026-07-27, herdr pane `wA6:p1`, 9 min wall clock, 1 attempt): 4/4 stories, 8/8 review assertions, 6 review findings (0 critical), `cargo build --quiet` exit 0, `cargo test --quiet` exit 0 (1 passed). Verified `DoDRunner.detect_project_type` finds `Cargo.toml` → `ProjectType::Rust` → `cargo test --quiet`. Workdir guard re-verified on this run.
- **Multi-iteration ralph** — smoke9 (wA5:p1, todo CLI with 7 stories) hit 5 ralph iterations, 4/7 stories passed, Judge correctly rejected for missing `test_todo.py` and stub commands, loop restarted with feedback. Second outer iteration made 5+ more iterations before smoke was manually killed (full completion would have taken 30+ min). Net: outer loop correctly handles partial progress and the fix for ralph max-iterations (3 unit tests) is the natural extension.
- **Real reject-path → acceptance** — CRUD smoke (2026-07-30, foreground `terminal(background=true)` diagnostic, ~25 min wall clock, 1 outer-loop attempt at the task level but 4 outer iterations inside the loop's perspective): rejected 3 times for distinct real defects (missing runtime verification, missing artifact capture + Pydantic `ItemOut` violation, RFC-violating 204 body), accepted on 4th iteration with 6/6 ralph stories, 11/11 review assertions, 0 critical findings. **This is the first ALPS run ever to complete end-to-end via the reject path** — surfaces that the existing `drive_rejects_then_passes_appends_feedback_to_next_plan` unit test faithfully represents real judge behavior, not just test-scripted Judge stubs. Deliverable at `/tmp/alps-crud-demo/` (`pytest -q` → 4 passed; `from main import app` works; 4 routes registered for POST/GET-list/GET-one/DELETE). Reasoning captured in the per-task `progress.txt` traces the close-then-reopen cycle on each iteration.
- **AGENTS.md propagation** — verified end-to-end; the task-level `AGENTS.md` accumulates patterns from each ralph iteration and is fed back to review/judge/next-plan.
- **Per-task branches** — verified; `git log` on the per-task branch shows `feat: [US-XXX]` commits per ralph story + `done: <task-id>` final auto-commit.
- **Workdir guard** — verified manually; sentinel written at end, blocks re-invocation within 5s, `--force` bypasses.
- **Nested repo exclusion** — verified; smoke7 produced no "embedded git repository" warning (was fatal on git 2.42+).
- **Deliverable outside workdir** — **gap surfaced** by the herdr-watch second smoke (2026-07-30, killed after iter 2 per smoke9-style "same reason twice → kill" rule). When the prompt specifies a target path *outside* `--workdir`, codex writes there as told, but `read_artifacts` only walks `tasks/.../implementation/ralph/` so Hermes has no source files to verify — even when the work is correct and `pytest -q` is green at the target path. Smoke 1 above worked *by accident* because codex happened to mirror into ralph's cwd; smoke 2 hit the gap deterministically. See §12 item 8 for the fix.

### Known issues

- ~~**Plan agent JSON flakiness**~~ — **resolved by `6a414a8` and `894be6b`**. All three LLM agents (Plan, Review, Judge) now retry up to 3 total attempts on JSON parse failure. Net effect: intermittent flakiness becomes transparent recovery. If all 3 fail, the run dies with a `failed after 3 attempts: ...` error.
- **Type-state attempt counter resets on `Rejected::reset()`** — the second iteration's plan shows `attempt=1`, not `2`. Not a bug per se (each `Planned` represents one attempt at a plan), but the type-state doesn't track global iteration. The `Rejected` struct carries `attempts: u32` but `reset()` doesn't pass it forward. Worth a small refactor if we want a global attempt counter on `Task<Done>`.

## 1. What is ALPS?

ALPS is a four-step orchestrator that takes a high-stakes prompt and drives it through:

1. **Plan** — Claude Code, prompt → granular implementation plan
2. **Implement** — Ralph loop with Codex, plan → finished work
3. **Review** — Claude Code, adversarial review → findings + assertions
4. **Judge** — Hermes (Hybrid: structured DoD + LLM), assertion matching → verdict

If the Judge **rejects**, the loop restarts at Plan with feedback appended to the prompt. If the Judge **passes**, the task is surfaced to Kyle with receipts.

**The point is to start simple with something that works and scale.** This spec is tight on MVP scope and explicit about what gets added later.

## 2. Architecture

```
            ┌──────────────────────────┐
            │ Kyle (human)             │
            │ - initiate with prompt   │
            │ - verify receipts on exit│
            └────────────┬─────────────┘
                         │ prompt
                         ▼
            ┌──────────────────────────┐
            │  ALPS Outer Loop         │
            │  (while !done)           │
            └────────────┬─────────────┘
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
   ┌─────────┐   ┌─────────────┐   ┌──────────┐
   │ Plan    │──▶│ Implement   │──▶│ Review   │
   │ Claude  │   │ Ralph+Codex │   │ Claude   │
   └─────────┘   └─────────────┘   └────┬─────┘
        ▲                                │
        │ feedback                       ▼
        │                          ┌──────────┐
        └──────────────────────────│  Judge   │
                                   │  Hermes  │
                                   └──────────┘
```

### 2.1 The compose boundary with Ralph

ALPS is the **outer orchestrator**. Ralph is the **inner implement loop**. ALPS treats Ralph as a black-box subprocess:

- ALPS writes `prd.json` and `prompt.md` into a Ralph working dir
- ALPS invokes `ralph.sh` (or `codex --ralph`-style entry point)
- Ralph runs its own loop: read PRD → pick story → implement → test → commit → loop
- Ralph exits when it sees `COMPLETE` in `progress.txt` or hits max iterations
- ALPS reads back `prd.json` (with `passes: true`) and `progress.txt` to produce `Implementation`

This is the same boundary Ralph's own loop uses for Claude Code / Amp. We don't reinvent Ralph.

## 3. Sequence Diagrams

See `docs/`:

- **[diagram-happy-path.html](docs/diagram-happy-path.html)** — outer loop runs once, Judge PASS
- **[diagram-rejection-restart.html](docs/diagram-rejection-restart.html)** — Judge REJECT → feedback loop
- **[diagram-state-machine.html](docs/diagram-state-machine.html)** — all states and transitions

## 4. State Machine

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Task<S>  —  type-state pattern                                          │
│                                                                          │
│  new(prompt)         plan(plan)          implement(impl)                 │
│  ┌─────────┐       ┌──────────┐       ┌──────────────┐                  │
│  │  Idle   │──────▶│ Planned  │──────▶│ Implemented  │                  │
│  └─────────┘       └──────────┘       └──────────────┘                  │
│                                              │                          │
│                                              │ review(review)            │
│                                              ▼                          │
│                                       ┌──────────────┐                  │
│                                       │   Reviewed   │                  │
│                                       └──────────────┘                  │
│                                          │       │                      │
│                              judge(pass) │       │ judge(reject)        │
│                                          ▼       ▼                      │
│                                       ┌──────┐  ┌──────────┐            │
│                                       │ Done │  │ Rejected │            │
│                                       └──────┘  └──────────┘            │
│                                                      │                   │
│                                          reset() with feedback          │
│                                                      │                   │
│                                                      ▼                   │
│                                                  (back to Idle)          │
│                                                                          │
│  Any state ──agent error──▶ Failed                                     │
└──────────────────────────────────────────────────────────────────────────┘
```

The state machine is encoded in the **Rust type system** — `Task<Idle>`, `Task<Planned>`, etc. are distinct types. You cannot call `Task<Idle>::review()` because that method only exists on `Task<Reviewed>`. Invalid transitions are **compile errors**.

## 5. Type Design (Rust, strict typing)

This is the spine of the spec. Read it carefully.

### 5.1 Newtypes (no stringly-typed APIs)

```rust
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::path::PathBuf;
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Unique task identifier. Format: `YYYY-MM-DDTHHMMSS-<uuid8>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new() -> Self {
        TaskId(format!(
            "{}-{}",
            Utc::now().format("%Y-%m-%dT%H%M%S"),
            Uuid::new_v4().simple()
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReceiptId(Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoryId(String);
```

### 5.2 Domain types

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prompt(String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub goal: String,
    pub architecture: String,
    pub stories: Vec<UserStory>,
    pub dod: Vec<DefinitionOfDone>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserStory {
    pub id: StoryId,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinitionOfDone {
    pub criterion: String,
    pub verifiable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Implementation {
    pub ralph_branch: String,
    pub prd_path: PathBuf,
    pub commits: Vec<Commit>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: ArtifactKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArtifactKind {
    Source,
    Test,
    Doc,
    Config,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub findings: Vec<Finding>,
    pub assertions: Vec<Assertion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub description: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub criterion: String,
    pub passed: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Judgment {
    Pass(Receipts),
    Reject(Feedback),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feedback {
    pub reason: String,
    pub failed_assertions: Vec<Assertion>,
    pub retry_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipts {
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub plan_summary: String,
    pub implement_metrics: ImplementMetrics,
    pub review_summary: ReviewSummary,
    pub judged_at: DateTime<Utc>,
    pub judge_model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImplementMetrics {
    pub stories_passed: u32,
    pub stories_total: u32,
    pub iterations: u32,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub findings_count: u32,
    pub critical_findings: u32,
    pub assertions_passed: u32,
    pub assertions_total: u32,
}
```

### 5.3 Sealed agent trait

```rust
mod sealed {
    /// Sealed trait — only `alps-core` can implement `Agent`.
    pub trait Sealed {}
}

/// Every agent in the ALPS loop is `Agent<Input, Output, Error>`.
/// Sealed: external crates cannot add new agent kinds.
pub trait Agent: Send + Sync + sealed::Sealed {
    type Input: Serialize + for<'de> Deserialize<'de> + Send + Sync;
    type Output: Serialize + for<'de> Deserialize<'de> + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &'static str;
    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}

// Concrete agents (one per step)
pub struct PlanAgent { /* Claude Code config */ }
pub struct ImplementAgent { /* Ralph config */ }
pub struct ReviewAgent { /* Claude Code config */ }
pub struct JudgeAgent { /* Hermes config */ }

impl sealed::Sealed for PlanAgent {}
impl sealed::Sealed for ImplementAgent {}
impl sealed::Sealed for ReviewAgent {}
impl sealed::Sealed for JudgeAgent {}

impl Agent for PlanAgent {
    type Input = Prompt;
    type Output = Plan;
    type Error = PlanError;
    fn name(&self) -> &'static str { "plan" }
    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error> { /* ... */ }
}
// ... etc for ImplementAgent, ReviewAgent, JudgeAgent
```

### 5.4 Type-state pattern — the heart of strict typing

Each state is a **distinct struct**. Transitions consume `self` and return the next state. Missing transitions are **compile errors**.

```rust
pub struct Task<State> {
    pub id: TaskId,
    pub workdir: PathBuf,
    pub state: State,
    _phantom: PhantomData<State>,
}

/// Initial state. Has a prompt, nothing else.
pub struct Idle {
    pub prompt: Prompt,
}

/// Plan emitted.
pub struct Planned {
    pub prompt: Prompt,
    pub plan: Plan,
    pub attempt: u32,
}

/// Implementation emitted by Ralph.
pub struct Implemented {
    pub prompt: Prompt,
    pub plan: Plan,
    pub implementation: Implementation,
    pub attempt: u32,
}

/// Review emitted.
pub struct Reviewed {
    pub prompt: Prompt,
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    pub attempt: u32,
}

/// Judge accepted.
pub struct Done {
    pub receipts: Receipts,
    pub attempts: u32,
}

/// Judge rejected — must reset to Idle with feedback.
pub struct Rejected {
    pub feedback: Feedback,
    pub attempts: u32,
    pub history: Vec<Attempt>,
}

/// Catastrophic failure.
pub struct Failed {
    pub reason: FailureReason,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    pub feedback: Option<Feedback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FailureReason {
    PlanAgentError(String),
    ImplementError(String),
    ReviewAgentError(String),
    JudgeError(String),
    PersistenceError(String),
    MaxAttemptsExceeded { max: u32 },
}

// ─────────────────────────────────────────────────────────────
// Transitions. Each impl block defines the methods available
// on a specific state. You cannot call `Task<Idle>::review()`
// — that method only exists on `Task<Reviewed>`.
// ─────────────────────────────────────────────────────────────

impl Task<Idle> {
    pub fn new(id: TaskId, workdir: PathBuf, prompt: Prompt) -> Self {
        Task {
            id,
            workdir,
            state: Idle { prompt },
            _phantom: PhantomData,
        }
    }

    pub fn plan(self, plan: Plan) -> Task<Planned> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Planned {
                prompt: self.state.prompt,
                plan,
                attempt: 1,
            },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Failed { reason, attempts: 0 },
            _phantom: PhantomData,
        }
    }
}

impl Task<Planned> {
    pub fn implement(self, implementation: Implementation) -> Task<Implemented> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Implemented {
                prompt: self.state.prompt,
                plan: self.state.plan,
                implementation,
                attempt: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Failed { reason, attempts: self.state.attempt },
            _phantom: PhantomData,
        }
    }
}

impl Task<Implemented> {
    pub fn review(self, review: Review) -> Task<Reviewed> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Reviewed {
                prompt: self.state.prompt,
                plan: self.state.plan,
                implementation: self.state.implementation,
                review,
                attempt: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Failed { reason, attempts: self.state.attempt },
            _phantom: PhantomData,
        }
    }
}

impl Task<Reviewed> {
    /// The judge. Returns Result — `Ok` on pass, `Err` on reject.
    pub fn judge(self, judgment: Judgment) -> Result<Task<Done>, Task<Rejected>> {
        match judgment {
            Judgment::Pass(receipts) => Ok(Task {
                id: self.id,
                workdir: self.workdir,
                state: Done {
                    receipts,
                    attempts: self.state.attempt,
                },
                _phantom: PhantomData,
            }),
            Judgment::Reject(feedback) => Err(Task {
                id: self.id,
                workdir: self.workdir,
                state: Rejected {
                    feedback,
                    attempts: self.state.attempt,
                    history: Vec::new(),
                },
                _phantom: PhantomData,
            }),
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Failed { reason, attempts: self.state.attempt },
            _phantom: PhantomData,
        }
    }
}

impl Task<Rejected> {
    /// The only way out of Rejected. Appends feedback to the prompt
    /// and returns to Idle. The next iteration starts a fresh attempt.
    pub fn reset(self, history: Vec<Attempt>) -> Task<Idle> {
        let prompt = Self::append_feedback(&self.state.prompt, &self.state.feedback, self.state.attempts);
        Task {
            id: self.id,
            workdir: self.workdir,
            state: Idle { prompt },
            _phantom: PhantomData,
        }
    }

    fn append_feedback(prompt: &Prompt, feedback: &Feedback, attempts: u32) -> Prompt {
        let mut s = prompt.0.clone();
        s.push_str(&format!(
            "\n\n## Previous attempt rejected (#{})\n\n\
             **Reason:** {}\n\n\
             **Failed assertions:**\n{}\n\n\
             **Retry hints:**\n{}\n",
            attempts,
            feedback.reason,
            feedback.failed_assertions.iter()
                .map(|a| format!("- {} {}",
                    if a.passed { "[x]" } else { "[ ]" },
                    a.criterion))
                .collect::<Vec<_>>()
                .join("\n"),
            feedback.retry_hints.iter()
                .map(|h| format!("- {}", h))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        Prompt(s)
    }
}
```

### 5.5 Typed errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum AlpsError {
    #[error("plan agent failed: {0}")]
    PlanAgent(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("implement (Ralph) failed: {0}")]
    Implement(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("review agent failed: {0}")]
    ReviewAgent(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("judge failed: {0}")]
    Judge(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("persistence failed: {0}")]
    Persistence(#[source] std::io::Error),

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid task state: {0}")]
    InvalidState(String),

    #[error("max attempts ({max}) exceeded; task aborted")]
    MaxAttemptsExceeded { max: u32, history: Vec<Attempt> },
}
```

## 6. Module Layout

```
alps/                              # Cargo workspace
├── Cargo.toml                     # [workspace]
├── alps-core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                 # Re-exports
│       ├── task.rs                # Type-state Task<S> + state structs
│       ├── loop_.rs               # Outer loop driver (`loop` is a keyword)
│       ├── plan.rs                # PlanAgent + PlanError
│       ├── implement.rs           # ImplementAgent (Ralph subprocess) + ImplementError
│       ├── review.rs              # ReviewAgent + ReviewError
│       ├── judge.rs               # JudgeAgent + JudgeError
│       ├── receipt.rs             # Receipts, ImplementMetrics, ReviewSummary
│       ├── persistence.rs         # JSON file I/O for task workspace
│       ├── error.rs               # AlpsError
│       └── domain.rs              # Plan, Review, Implementation, etc.
└── alps-cli/
    ├── Cargo.toml
    └── src/
        └── main.rs                # CLI entry — `alps run "prompt"`
```

## 7. Per-Task File Structure

Each task gets a directory under `tasks/`. Git is the main history — every artifact is committed.

```
tasks/<task-id>/
├── prompt.md                      # Initial prompt
├── prompt-history.md              # Prompt with feedback appended (rejection rounds)
├── plan.json                      # Latest plan
├── implementation/
│   └── ralph/                     # Ralph's working dir (clone of target repo)
│       ├── prd.json
│       ├── progress.txt
│       ├── CLAUDE.md              # if Claude Code
│       └── commits.log
├── review.json                    # Latest review findings
├── judgment.json                  # Latest judgment
├── receipts.json                  # Final receipts (only on Done)
└── feedback.json                  # Last rejected feedback (only on Rejected)
```

## 8. The Outer Loop

```rust
// alps-core/src/loop_.rs — sketch
pub async fn drive(
    task: Task<Idle>,
    plan: &PlanAgent,
    implement: &ImplementAgent,
    review: &ReviewAgent,
    judge: &JudgeAgent,
) -> Result<Task<Done>, AlpsError> {
    let mut task = task;

    loop {
        // Plan
        let plan_input = task.state.prompt.clone();
        let plan_out = plan.run(plan_input).await
            .map_err(|e| AlpsError::PlanAgent(Box::new(e)))?;
        task = task.plan(plan_out);
        persist(&task)?;

        // Implement
        let impl_input = task.state.plan.clone();
        let impl_out = implement.run(impl_input).await
            .map_err(|e| AlpsError::Implement(Box::new(e)))?;
        task = task.implement(impl_out);
        persist(&task)?;

        // Review
        let review_out = review.run(()).await
            .map_err(|e| AlpsError::ReviewAgent(Box::new(e)))?;
        task = task.review(review_out);
        persist(&task)?;

        // Judge
        let judgment = judge.run(()).await
            .map_err(|e| AlpsError::Judge(Box::new(e)))?;
        match task.judge(judgment) {
            Ok(done) => {
                persist(&done)?;
                return Ok(done);
            }
            Err(rejected) => {
                persist(&rejected)?;
                // append feedback to prompt, reset to Idle
                task = rejected.reset(vec![]);
            }
        }
    }
}
```

## 9. MVP vs Scale

### MVP (Phase 1) — what we build first

- [x] Single task at a time, single process
- [x] Files + git for state (no DB)
- [x] Stub plan/review/judge agents that invoke CLIs as subprocesses
- [x] Type-state for the outer loop
- [x] **Hybrid Judge** — verifiable DoD checks first, then LLM (Hermes) for soft judgment
- [x] **Unbounded loop** — no max attempts; brute force until Judge passes ("must succeed eventually")
- [x] **stdout-only notifications** — CLI prints receipt summary, writes `tasks/<id>/receipts.json`
- [x] **Markdown + JSON receipts** — terminal summary for Kyle, JSON for downstream tooling
- [x] Rejection restart loop
- [x] Ralph invoked as subprocess via `ralph.sh`
- [x] CLI: `alps run "prompt"`
- [x] **Per-task git branches** — `alps/<task-id>` per workdir, holds the per-run state
- [x] **AGENTS.md propagation** — ralph `## Codebase Patterns` → task-level AGENTS.md → review/judge/next-plan

### Phase 2 — scale-out

- [ ] Multi-task daemon (one task per slot, queue)
- [ ] Pub/sub notifications to Kyle when Done (via Discord webhook)
- [ ] Receipt aggregation dashboard
- [ ] `MaxAttempts` and escalation policy
- [x] ~~AGENTS.md / CLAUDE.md updates from implement step~~ (landed early in `f452ca3`)
- [x] ~~Structured plan output (typed user stories, typed DoD)~~ (landed in `a36978f`)
- [x] **Rust DoD path** — `DoDRunner` auto-detects Rust, runs `cargo test`. Code is there; never smoke-tested.
- [x] **Plan retry-on-parse-fail** — landed in `6a414a8`. `PlanAgent::run` retries up to `max_retries=3` on `PlanError::Parse`. 5 deterministic tests cover the contract.

### Phase 3 — advanced

- [ ] Persistent task queue (SQLite)
- [ ] Cross-task learning (reuse feedback patterns)
- [ ] Web UI for monitoring
- [ ] Multi-model judge (judge ensemble)
- [x] ~~Per-task branches in git (one branch per task)~~ (landed in `f452ca3`)
- [ ] Cost ceiling per task (LLM Judge + Plan + Review add up fast on a "brute force" reject cycle)
- [ ] CI (GitHub Actions on `klampatech/alps`)
- [ ] Mock-agent test coverage for happy path + multi-iteration reject cycles

## 10. Agent integrations

| Agent | Runtime | Invocation | Input | Output |
|---|---|---|---|---|
| **Plan** | Claude Code | `cat prompt.md \| claude -p` | `Prompt` | `Plan` (parsed from JSON) |
| **Implement** | Ralph + Codex | `./ralph.sh [--max-iters N]` | `Plan` (→ `prd.json`) | `Implementation` (parsed from git log + progress.txt) |
| **Review** | Claude Code | `cat impl.md \| claude -p` | `Implementation` | `Review` (parsed from JSON) |
| **Judge** | Hybrid: structured DoD + Hermes (LLM) | (in-process + subprocess) | `JudgeContext` (plan + impl + review) | `Judgment` |

For MVP, Plan and Review use JSON-output prompts. Implement wraps Ralph. Judge is the most interesting — see open questions.

## 11. Resolved Decisions

### 11.1 Judge — Hybrid (verifiable DoD + LLM) — resolved 2026-07-26

Two-stage:

1. **Structured pass**: run all `DefinitionOfDone { verifiable: true }` criteria
   deterministically (tests, typecheck, lint, etc.). If any verifiable check fails,
   the verdict is **REJECT** with the failed criteria as feedback.
2. **LLM pass**: if all verifiable checks pass, call Hermes (LLM) with the review
   findings and the implementation. The LLM can hold the verdict (PASS) or reject
   (REJECT) with soft reasoning (code quality, design, etc.).

Both must clear for PASS. Hermes can reject a structured PASS if it sees soft issues.
This is the "heavy LLM" judgement: it consumes the review assertions and verifies
them against the implementation artifacts, but it also runs the DoD checks if any
are verifiable.

### 11.2 Max attempts — Unbounded — resolved 2026-07-26

The loop has no cap. On Judge reject, the loop restarts with feedback appended
to the prompt. Philosophy: **brute force development** — "it must succeed eventually."

Cost is tracked in receipts (total attempts, total elapsed) but not bounded.

### 11.3 Notifications — stdout only — resolved 2026-07-26

CLI runs synchronously. On Done, prints the receipt summary to stdout and writes
`tasks/<id>/receipts.json`. No Discord, no polling, no file watch.

Kyle runs `alps run "..."` and gets the result on the terminal. The receipts.json
file is the durable artifact for downstream tooling.

### 11.4 Receipts format — Markdown + JSON — resolved 2026-07-26

- **Markdown**: printed to stdout for human reading (Kyle sees it on terminal)
- **JSON**: written to `tasks/<id>/receipts.json` for downstream tooling
- CLI exit code: 0 on Done, 1 on `AlpsError`, 2 on Failed

### 11.5 Deferred (Phase 2+)

- ~~Per-task branches (currently single main with date-stamped commits)~~ — landed in `f452ca3`
- ~~AGENTS.md / CLAUDE.md propagation from implement step~~ — landed in `f452ca3`
- Agent test fixtures (mock CLIs vs recorded fixtures) — partial; `for_test` constructors landed in `731fbd3` but the prompt mentioned "fixtures vs recorded" — recorded-replay fixtures still open
- Workdir-level guard against the wrapping agent re-invoking alps — landed in `46327b4`

## 12. Next work (prioritized)

This is the live roadmap as of 2026-07-30. Items in **bold** are
load-bearing for "ALPS works" claims. Items below the line are
quality-of-life or scale concerns.

1. **Deliverable-outside-workdir gap** *(surfaced 2026-07-30 by the herdr-watch CRUD smoke)* — when the prompt specifies a target path outside `--workdir` (e.g. "Build at `/tmp/foo-2/`"), codex writes there as told, but `read_artifacts` only walks `tasks/.../implementation/ralph/`. Hermes then has empty source files and rejects everything even when `pytest -q` is green at the target. Smoke 1 of the CRUD pair worked *by accident* (codex mirrored into ralph cwd per its US-001 progress pattern); smoke 2 hit the gap deterministically and was killed after iter 2. Three fix approaches, in increasing order of correctness vs cost:

   - **A. Prompt-side** *(~5 min)* — rewrite the smoke recipe and the herdr-iteration-workflow smoke recipe to instruct: *"always specify the deliverable path *inside* `--workdir` (or equal to it), so the recursive walker and Hermes see the actual code."* Optionally add a CLI-side warning when the prompt contains a hardcoded `/tmp/...` or `/home/...` path that doesn't match `--workdir`. Closes 80% of real-world cases; users who *intentionally* want work outside workdir still get a sensible guess.
   - **B. alps-side** *(~1-2 hr)* — add `--deliverable-path <path>` (default = `--workdir`). `read_artifacts` walks *that* path recursively instead of `ralph_dir`. `commit_smart` adds the deliverable path to `.git/info/exclude` so workdir commits don't pick up unrelated files. The deliverable tree is referenced by relative-path in receipts (`Receipts { deliverable: <path> }`) so downstream `alps show <id>` can find it later. Closes 100% of cases.
   - **C. alps-side, automatic** *(~1 day)* — auto-detect: scan `--workdir` for new top-level directories after each Implement, mirror them into the workdir git as additional branches or sub-paths, walk them all in `read_artifacts`. More moving parts; defer until we see another real failure that A+B don't cover.

   **My recommendation: ship A now as a doc/recipe change. Open B as the next item once CRUD-style "build at /tmp/X" prompts become routine.** Adding C only if B has gaps.

2. **Mock-agent happy-path test** — we have the reject-path test; the happy path is still only smoke-tested. Adding `drive_passes_first_try` (Plan/Implement/Review/Judge all return canned values, verify Ok(done) on first call) would close the symmetric gap.
3. **Spec §2.1 / §5.3 sync** — the implementation has drifted from the spec in a few places (per-task branches are now §4 state, not §3 deferred; the agent trait is still sealed; etc.). The spec is now ahead of the code in some areas and behind in others. Worth a top-to-bottom pass once the bug-bash is done.
4. **CI** — no GitHub Actions on `klampatech/alps`. The 110 tests run locally only. ~30 min to set up.
5. **alps-source `AGENTS.md` / `CLAUDE.md`** — when alps runs against itself, the workdir-level AGENTS.md starts empty. Worth seeding the alps source repo with project conventions.
6. **Cost ceiling** — "brute force" + LLM Judge = real money on a multi-reject cycle. The 3-reject CRUD smoke burned ~$1.50 of LLM Judge calls on top of 4× Claude Plan/Review calls. Add a per-task USD cap that exits with `AlpsError` if exceeded.
7. **More DoD project types** — currently Python + Rust. Add Node (`npm test`?), Go (`go test`?). 1-2 hours each.

### Recently completed (just shipped)

- ~~**Real reject-path smoke**~~ — verified end-to-end via the CRUD smoke (2026-07-30, foreground diagnostic). 4 outer iterations, 3 rejects (each catching a distinct real defect), 4th iteration accepted. 6/6 ralph stories, 11/11 review assertions, 0 critical findings. First ALPS run to complete via the reject-path in production (versus the existing deterministic unit test that exercised only the orchestration layer). Confirms that `drive_rejects_then_passes_appends_feedback_to_next_plan` faithfully represents real judge behavior, not just test-scripted Judge stubs.
- ~~**Rust DoD path**~~ — landed in `fd35ff5` (artifact-recursion fix) plus the pre-existing `DoDRunner` code. Verified by a 9-min herdr smoke on 2026-07-27: `alps run "create Rust lib with add(a,b) + test"` → `# ALPS — Done`, 1 attempt, 4/4 stories, 8/8 review assertions, `cargo build --quiet` 0 / `cargo test --quiet` 0 (1 passed). Workdir guard verified: re-invoke within 5s → exit 2 with the `recent completion` error; `--force` bypasses.
- ~~**Recursive artifact collection**~~ — landed in `fd35ff5`. `read_artifacts` now walks `ralph_dir` recursively (was non-recursive `std::fs::read_dir`), so Rust `src/lib.rs` (and any subdirectory source tree) lands in `Implementation.artifacts`. Skips `target/`, `node_modules/`, `.git/`, `__pycache__/`, `.gradle/`, `.cargo/`, etc. via a `SKIP_DIRS` list. +1 regression test (`read_artifacts_recurses_into_subdirectories`). Bug surfaced by the Rust DoD smoke — Hermes Judge rejected on "Source files section omits src/lib.rs entirely" even though `cargo test` was passing.
- ~~**Multi-iteration ralph**~~ — landed in `06d916d`. The 20-iteration safety net now routes through Judge (was a hard error before). 3 new integration tests with fake ralph.sh scripts. Smoke9 (todo CLI, 7 stories) demonstrated partial-progress-restart in production — 5 ralph iterations → 4/7 stories → Judge reject → loop restart → 5 more iterations.
- ~~**Plan retry-on-parse-fail**~~ — landed in `6a414a8`. `PlanAgent::run` retries up to 3 total attempts on `PlanError::Parse`.
- ~~**Review + Judge retry-on-parse-fail**~~ — landed in `894be6b`. Same pattern. Plus `JudgeError::Parse` variant to distinguish parse errors from semantic errors.
- ~~**Reject-path verification**~~ — landed in `731fbd3`. Deterministic unit test using `for_test` mock agents.
- ~~**AGENTS.md propagation**~~ — landed in `f452ca3`.
- ~~**Per-task branches**~~ — landed in `f452ca3`.
- ~~**Workdir completion guard**~~ — landed in `46327b4`.
- ~~**Nested git repo exclusion**~~ — landed in `6ebaf92`.
