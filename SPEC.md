# ALPS — Specification

> **Status**: Draft v0.1
> **Author**: Kyle + Evo
> **Date**: 2026-07-26

## 1. What is ALPS?

ALPS is a four-step orchestrator that takes a high-stakes prompt and drives it through:

1. **Plan** — Claude Code, prompt → granular implementation plan
2. **Implement** — Ralph loop with Codex, plan → finished work
3. **Review** — Claude Code, adversarial review → findings + assertions
4. **Judge** — Hermes, assertion matching → verdict

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

- Single task at a time, single process
- Files + git for state (no DB)
- Stub plan/review/judge agents that invoke CLIs as subprocesses
- Type-state for the outer loop
- Rejection restart loop
- Receipts written to JSON
- Ralph invoked as subprocess via `ralph.sh`
- CLI: `alps run "prompt"`

### Phase 2 — scale-out

- Multi-task daemon (one task per slot, queue)
- Pub/sub notifications to Kyle when Done (via Discord webhook)
- Receipt aggregation dashboard
- `MaxAttempts` and escalation policy
- AGENTS.md / CLAUDE.md updates from implement step
- Structured plan output (typed user stories, typed DOD)

### Phase 3 — advanced

- Persistent task queue (SQLite)
- Cross-task learning (reuse feedback patterns)
- Web UI for monitoring
- Multi-model judge (judge ensemble)
- Per-task branches in git (one branch per task)

## 10. Agent integrations

| Agent | Runtime | Invocation | Input | Output |
|---|---|---|---|---|
| **Plan** | Claude Code | `cat prompt.md \| claude -p` | `Prompt` | `Plan` (parsed from JSON) |
| **Implement** | Ralph + Codex | `./ralph.sh [--max-iters N]` | `Plan` (→ `prd.json`) | `Implementation` (parsed from git log + progress.txt) |
| **Review** | Claude Code | `cat impl.md \| claude -p` | `Implementation` | `Review` (parsed from JSON) |
| **Judge** | Hermes | (in-process or via CLI) | `Review` + `Plan` + `Implementation` | `Judgment` |

For MVP, Plan and Review use JSON-output prompts. Implement wraps Ralph. Judge is the most interesting — see open questions.

## 11. Open Questions

1. **Judge implementation** — is the judge a structured assertion matcher (deterministic), an LLM call, or both? My read: heavy LLM (Hermes) that consumes review assertions and verifies against implementation artifacts, but **also** loads and runs the DoD-criterion checks if they're verifiable (tests, typecheck, etc.). Need to confirm.
2. **Max attempts on the outer loop** — unbounded, or capped? Suggest capped at 3 with a clear failure mode.
3. **Notifications** — how does Kyle learn the task is Done? Discord pub/sub? File watch? CLI status?
4. **Receipts format** — what does Kyle see? Markdown summary? JSON? Full HTML report?
5. **Per-task git branches** — branch per task, or single main with date-stamped commits? Suggest single main for MVP.
6. **AGENTS.md / CLAUDE.md updates** — does ALPS propagate learnings from implement to the user's repo, or only within the task workspace?
7. **Testing the agent layer** — how do we test plan/review/judge without burning tokens? Mock CLIs? Recorded fixtures?
