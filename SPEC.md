# ALPS — Specification

> **Status**: v0.7.2 — orchestrator working end-to-end for Python, Rust, Node, Go; reject path verified in production; agent retries in place; ralph max-iterations routes through Judge; per-task branches + AGENTS.md propagation + workdir guard + nested-git exclude + recursive artifact walker + `--deliverable-path` flag + auto-detect deliverable path shipped. 118/118 tests passing.
> **Author**: Kyle + Evo
> **Date**: 2026-07-26 (initial); 2026-07-27 (v0.1 implementation); 2026-08-03 (v0.7.2 — major architectural drift cleanup, this revision)

## 0. What's been built since the original spec

The v0.1 spec was written before any code shipped. As of 2026-08-03, the
core loop is implemented and working end-to-end across four project types
(Python, Rust, Node, Go), with the full reject path verified in production.
Below is the changelog of what landed in `klampatech/alps` between the spec
and now.

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
| 2026-07-30 | (no commit) | **Judge model swap (claude-sonnet-4 → claude-opus-4)** — `LlmJudgeConfig::default().model` flipped to `claude-opus-4` (Claude Code Opus alias → MiniMax-M3 on this host). Plan + Review stay on `claude-sonnet-4` (MiniMax-M2.7) for cheaper sub-agent work; only the Judge gets the dedicated model. Rationale + naming-history ("HermesLlmJudge" struct-name from original spec-time decision, runtime invokes Claude Code) documented in SPEC §11.1. Direct CLI smoke confirms `claude --model opus` and `claude --model claude-sonnet-4` both work on this host. Receipts now record `judge_model: "claude-opus-4"`. |
| 2026-08-01 | \ (PR #2) | **--deliverable-path flag** — closes §12 item 2 / Runtime Pitfall #16. CLI flag defaults to `--workdir`; when set to a path outside the workdir, `read_artifacts` walks that path, the Judge/Review follow suit, and `commit_smart_with_excludes` appends the path to `.git/info/exclude`. +4 tests (110 → 114). |
| 2026-08-02 | `0d840c1` (PR #3) | **`detect_project_type` walks `--deliverable-path`** — `DoDRunner::check` previously called `detect_project_type(ralph_dir)` which never contained `package.json` or `go.mod` (ralph nested git). Now resolves `detect_root` from `Implementation.deliverable_path` (falling back to `ralph_dir` when empty). `run_cmd_with_timeout` runs in `detect_root` so `npm test` / `go test ./...` execute against the right tree. +4 tests (114 → 118). Closes §12 item 7 (Node + Go DoD types). Verified by Node smoke (herdr `wAK:p1`, 2026-08-02, `[judge:structured] detected project type: node → running: npm test --silent → PASS` on every iteration) and Go smoke (herdr `wAM:p1`, 2026-08-03, `[judge:structured] detected project type: go → running: go test ./... → PASS` on every iteration). |
| 2026-08-03 | `4c395a4` (PR #4) | **Auto-detect `--deliverable-path` from prompt text** — closes §12 item 1C. New `alps-cli/src/detect.rs` module (stdlib only) parses the prompt for preposition keywords (`at`, `in`, `to`, `into`, `under`, `inside`, `build at`, etc.) and picks the most-likely deliverable path via outside-workdir > mention-count > shortest-path scoring. 3-way override: explicit `--deliverable-path` always wins, prompt-derived wins when `--deliverable-path` is empty and the prompt mentions a build path, falls back to `--workdir` otherwise. 14 unit tests. Verified end-to-end by Node smoke (herdr `wAP:p1`, 2026-08-03, 5/5 stories, 182s implement, Judge `claude-opus-4` ACCEPTED, fired without `--deliverable-path` flag). |
| 2026-08-03 | `e23ec6f` (PR #4 merge) | **Tier 3 Vite + React full-stack verified** — first ALPS run to deliver a frontend SPA end-to-end. 8/8 user stories, `npm run build` exit 0, Vitest 3/3 passed, Playwright screenshot visually confirmed. Caveat: alps orchestrator's post-implement pipeline (Review + Judge) was SIGPIPE'd by the `tee | log` wrapper during heavy stdout (`npm install` + `npm run build` + Playwright invocation); deliverable correctness verified by direct disk invocation, but `receipts.json` was not written. To re-fire for a clean Judge verdict, drop `tee` (see §12 item 4 reference / alps skill Pitfall #28). No ALPS-side code changes needed — Node DoD path (PR #3) handled the Vite project without modification. |

### Verified end-to-end

- **Happy path** — 7 successful smokes (smoke3, smoke4, smoke5, smoke6, smoke7, smoke8, smoke9) — all pass with Judge LLM verdict "pass" on the first attempt.
- **Rust DoD path** — first Rust smoke (2026-07-27, herdr pane `wA6:p1`, 9 min wall clock, 1 attempt): 4/4 stories, 8/8 review assertions, 6 review findings (0 critical), `cargo build --quiet` exit 0, `cargo test --quiet` exit 0 (1 passed). Verified `DoDRunner.detect_project_type` finds `Cargo.toml` → `ProjectType::Rust` → `cargo test --quiet`. Workdir guard re-verified on this run.
- **Node DoD path** — Tier 2.5 smoke (2026-08-02, herdr `wAK:p1`, ~52 min wall clock, 4 outer iterations): `[judge:structured] detected project type: node → running: npm test --silent → PASS` on every iteration. Direct `npm test`: 2 passed. Closes §12 item 7 half 1.
- **Go DoD path** — Tier 2.5b smoke (2026-08-03, herdr `wAM:p1`, ~22 min wall clock, 3 outer iterations): `[judge:structured] detected project type: go → running: go test ./... → PASS` on every iteration. Direct `go test -v -count=1`: 3 passed. Closes §12 item 7 half 2.
- **Tier 3 full-stack (Vite + React + TypeScript)** — 2026-08-03, herdr `wAN:p1`, 8/8 user stories, `npm run build` exit 0, Vitest 3/3 passed, Playwright screenshot visually confirmed at 1280x800. Caveat: post-implement SIGPIPE on `tee | log` (see §12 item 4 / Runtime Pitfall #28) — deliverable verified by direct disk invocation, but `receipts.json` was not written. To re-fire for a clean Judge verdict, drop `tee` from the wrapper script.
- **Multi-iteration ralph** — smoke9 (wA5:p1, todo CLI with 7 stories) hit 5 ralph iterations, 4/7 stories passed, Judge correctly rejected for missing `test_todo.py` and stub commands, loop restarted with feedback. Second outer iteration made 5+ more iterations before smoke was manually killed (full completion would have taken 30+ min). Net: outer loop correctly handles partial progress and the fix for ralph max-iterations (3 unit tests) is the natural extension.
- **Real reject-path → acceptance** — CRUD smoke (2026-07-30, foreground `terminal(background=true)` diagnostic, ~25 min wall clock, 1 outer-loop attempt at the task level but 4 outer iterations inside the loop's perspective): rejected 3 times for distinct real defects (missing runtime verification, missing artifact capture + Pydantic `ItemOut` violation, RFC-violating 204 body), accepted on 4th iteration with 6/6 ralph stories, 11/11 review assertions, 0 critical findings. **This is the first ALPS run ever to complete end-to-end via the reject path** — surfaces that the existing `drive_rejects_then_passes_appends_feedback_to_next_plan` unit test faithfully represents real judge behavior, not just test-scripted Judge stubs. Deliverable at `/tmp/alps-crud-demo/` (`pytest -q` → 4 passed; `from main import app` works; 4 routes registered for POST/GET-list/GET-one/DELETE). Reasoning captured in the per-task `progress.txt` traces the close-then-reopen cycle on each iteration.
- **AGENTS.md propagation** — verified end-to-end; the task-level `AGENTS.md` accumulates patterns from each ralph iteration and is fed back to review/judge/next-plan.
- **Per-task branches** — verified; `git log` on the per-task branch shows `feat: [US-XXX]` commits per ralph story + `done: <task-id>` final auto-commit.
- **Workdir guard** — verified manually; sentinel written at end, blocks re-invocation within 5s, `--force` bypasses.
- **Nested repo exclusion** — verified; smoke7 produced no "embedded git repository" warning (was fatal on git 2.42+).
- **Deliverable outside workdir** — **resolved 2026-08-01** via the `--deliverable-path` CLI flag (§12 item 2). Gap originally surfaced by the herdr-watch second smoke (2026-07-30, killed after iter 2 per smoke9-style "same reason twice → kill" rule). When the prompt specifies a target path *outside* `--workdir`, codex writes there as told, but `read_artifacts` only walked `tasks/.../implementation/ralph/` so Hermes had no source files to verify — even when the work was correct and `pytest -q` was green at the target path. Smoke 1 above worked *by accident* because codex happened to mirror into ralph's cwd; smoke 2 hit the gap deterministically. **Fix**: `--deliverable-path /tmp/foo/` (default = `--workdir`) routes `read_artifacts`, the Judge's `read_files`, and the Review's `read_files` to walk that path. `commit_smart_with_excludes` appends the path to `.git/info/exclude` only when outside the workdir (idempotent). `Implementation.deliverable_path` is persisted to `tasks/<id>/implementation.json` at the Implemented state.

### Known issues

- ~~**Plan agent JSON flakiness**~~ — **resolved by `6a414a8` and `894be6b`**. All three LLM agents (Plan, Review, Judge) now retry up to 3 total attempts on JSON parse failure. Net effect: intermittent flakiness becomes transparent recovery. If all 3 fail, the run dies with a `failed after 3 attempts: ...` error.
- **Type-state attempt counter resets on `Rejected::reset()`** — the second iteration's plan shows `attempt=1`, not `2`. Not a bug per se (each `Planned` represents one attempt at a plan), but the type-state doesn't track global iteration. The `Rejected` struct carries `attempts: u32` but `reset()` doesn't pass it forward. Worth a small refactor if we want a global attempt counter on `Task<Done>`.
- **Tier 3 SIGPIPE on heavy stdout (Runtime Pitfall #28)** — when the wrapper script's `tee | log` pipe consumes the alps orchestrator's stdout during heavy-output smokes (`npm install` + `npm run build` + Playwright invocation), `tee` can get SIGPIPE'd. The alps process exits cleanly, but `receipts.json` is never written and `.alps-last-done` is never touched. The deliverable's correctness is real (verified by direct disk invocation); only the Judge verdict is missing. **Mitigation:** drop `tee` from the wrapper script. The canonical smoke recipe uses `herdr pane run <pane_id> "..."` + `herdr wait output <pane> --match "ALPS — Done" --timeout N` — `tee` is decorative.

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

- ALPS writes `prd.json` (a 1:1 mapping from `Plan.stories` to Ralph's `userStories` format) and `progress.txt` (with `## Codebase Patterns` header) into `tasks/<id>/implementation/ralph/<workdir>`.
- ALPS spawns `ralph.sh --tool codex <max-iterations>` with stdin/stdout inherited. Ralph runs its own loop: read PRD → pick story → implement → test → commit → loop. Ralph exits 0 when `<promise>COMPLETE</promise>` lands in `.codex-last-message.txt` (the codex-specific completion extraction; commits `799067d`–`752c41a`).
- ALPS reads back `prd.json` (stories now have `passes: true`), `progress.txt`, and `git log` for commits → typed `Implementation`. The `ImplementMetrics` (stories_passed, stories_total, iterations, elapsed_secs) are read from `.ralph-result.json` (persisted by Ralph after each invocation) and plumbed through to `Receipts`.
- `read_artifacts` walks `tasks/<id>/implementation/ralph/` (or `--deliverable-path` if set) recursively to populate `Implementation.artifacts` for the LLM Judge. Skips `target/`, `node_modules/`, `.git/`, etc. via `SKIP_DIRS`.

The inner-loop completion signal is **codex-specific** (`.codex-last-message.txt` containing the promise). When Ralph is later swapped to a different tool (claude, etc.), this extract/parsing must be re-implemented — it's the only alps-side knowledge of the inner tool. Ralph's nested git repo (`tasks/<id>/implementation/ralph/.git/`) is excluded from the parent workdir's `git add -A` via `<workdir>/.git/info/exclude` (commit `6ebaf92`, Runtime Pitfall #7).

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
    /// Metrics captured from the Ralph run — iterations, elapsed time, story
    /// completion. Plumbed through to `Receipts` so the user sees real numbers.
    /// When the orchestrator hits a partial-progress state (ralph exits non-zero
    /// at max-iterations), this is what surfaces back to the Judge.
    pub metrics: ImplementMetrics,
    /// Where the deliverable actually lives. Defaults to the ralph nested
    /// workspace (`tasks/<id>/implementation/ralph/`). When the prompt specifies
    /// a target path *outside* `--workdir` (e.g. "build at `/tmp/foo/`"), the
    /// CLI sets this to that path via `--deliverable-path` (or auto-detect) so
    /// `read_artifacts` and the Judge's `read_files` walk the right tree.
    /// See §12 item 2 — closes the gap surfaced by the 2026-07-30 CRUD smoke v2.
    pub deliverable_path: PathBuf,
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
    /// `Receipts` is the final-assembled output type and lives in `receipt.rs`
    /// (not `domain.rs`) — it's not domain data carried through the loop, it's
    /// the surface that Kyle sees + the durable JSON for downstream tooling.
    Pass(crate::receipt::Receipts),
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

/// Every agent in the ALPS loop is a subtype of `Agent`. The trait is sealed
/// (external crates cannot add new agent kinds) and uses associated types for
/// `Input`, `Output`, and `Error`. Agents that take no input (e.g. Review reads
/// implementation state from the workspace, Judge reads the Reviewed state)
/// use `EmptyInput` as their `Input`.
///
/// The original spec draft had `Agent<Input, Output, Error>` as a generic
/// struct — that was redesigned in the v0.1 implementation to use associated
/// types for cleaner monomorphization.
pub trait Agent: Send + Sync + sealed::Sealed {
    type Input: Serialize + for<'de> Deserialize<'de> + Send + Sync;
    type Output: Serialize + for<'de> Deserialize<'de> + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &'static str;
    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}

/// Sentinel for agents that take no input (Review, Judge).
pub struct EmptyInput;

/// Concrete agents (one per step)
pub struct PlanAgent { /* Claude Code config: model + JSON-output prompt */ }
pub struct ImplementAgent { /* Ralph config: max_iterations, ralph.sh path */ }
pub struct ReviewAgent { /* Claude Code config: model + adversarial prompt */ }
pub struct JudgeAgent { /* Hybrid: StructuredJudge + HermesLlmJudge */ }

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
impl Agent for ImplementAgent {
    type Input = Plan;
    type Output = Implementation;
    type Error = ImplementError;
    fn name(&self) -> &'static str { "implement" }
    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error> { /* ... */ }
}
impl Agent for ReviewAgent {
    type Input = EmptyInput;
    type Output = Review;
    type Error = ReviewError;
    fn name(&self) -> &'static str { "review" }
    async fn run(&self, _input: Self::Input) -> Result<Self::Output, Self::Error> { /* ... */ }
}
impl Agent for JudgeAgent {
    type Input = EmptyInput;
    type Output = Judgment;
    type Error = JudgeError;
    fn name(&self) -> &'static str { "judge" }
    async fn run(&self, _input: Self::Input) -> Result<Self::Output, Self::Error> { /* ... */ }
}
```

### 5.4 Type-state pattern — the heart of strict typing

Each state is a **distinct struct**. Transitions consume `self` and return the next state. Missing transitions are **compile errors**.

**Note on the prompt's location:** the original spec draft had `prompt: Prompt` inside each state struct (e.g. `Planned { prompt, plan, attempt }`). The v0.1 implementation moved `prompt` to `Task<S>` itself, because it doesn't change between states (except when `Rejected::reset()` appends feedback to it). This keeps the state structs minimal and lets `Task<Rejected>::reset()` produce a `Task<Idle>` with the feedback-augmented prompt.

```rust
/// `prompt` lives on `Task<S>` itself, not in each state.
pub struct Task<State> {
    pub id: TaskId,
    pub prompt: Prompt,
    pub workdir: PathBuf,
    pub state: State,
    _phantom: PhantomData<State>,
}

/// Initial state. Only the prompt exists.
pub struct Idle;

/// Plan has been emitted.
pub struct Planned {
    pub plan: Plan,
    pub attempt: u32,
}

/// Implementation has been emitted by Ralph.
pub struct Implemented {
    pub plan: Plan,
    pub implementation: Implementation,
    pub attempt: u32,
}

/// Review has been emitted.
pub struct Reviewed {
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    pub attempt: u32,
}

/// Judge has accepted. Terminal.
pub struct Done {
    pub receipts: Receipts,
    pub attempts: u32,
}

/// Judge has rejected. Must reset to Idle.
pub struct Rejected {
    pub feedback: Feedback,
    pub attempts: u32,
    pub history: Vec<Attempt>,
}

/// Catastrophic failure. Terminal.
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
            prompt,
            workdir,
            state: Idle,
            _phantom: PhantomData,
        }
    }

    pub fn plan(self, plan: Plan) -> Task<Planned> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Planned { plan, attempt: 1 },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            prompt: self.prompt,
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
            prompt: self.prompt,
            workdir: self.workdir,
            state: Implemented {
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
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Implemented> {
    pub fn review(self, review: Review) -> Task<Reviewed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Reviewed {
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
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Reviewed> {
    /// The judge. Returns `Ok` on pass, `Err` on reject.
    pub fn judge(self, judgment: Judgment) -> Result<Task<Done>, Task<Rejected>> {
        use Judgment::*;
        match judgment {
            Pass(receipts) => Ok(Task {
                id: self.id,
                prompt: self.prompt,
                workdir: self.workdir,
                state: Done {
                    receipts,
                    attempts: self.state.attempt,
                },
                _phantom: PhantomData,
            }),
            Reject(feedback) => Err(Task {
                id: self.id,
                prompt: self.prompt,
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
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Rejected> {
    /// The only way out of Rejected. Appends feedback to the prompt
    /// and returns to Idle. The next iteration starts a fresh attempt.
    pub fn reset(self, _history: Vec<Attempt>) -> Task<Idle> {
        // History is recorded by persistence; not used in reset.
        let new_prompt = Self::append_feedback(
            &self.prompt,
            &self.state.feedback,
            self.state.attempts,
        );
        Task {
            id: self.id,
            prompt: new_prompt,
            workdir: self.workdir,
            state: Idle,
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
│       ├── task.rs                # Type-state Task<S> + state structs + transitions
│       ├── loop_.rs               # Outer loop driver — RECURSIVE, not loop{} (mod loop_ because `loop` is a Rust keyword)
│       ├── plan.rs                # PlanAgent (real Claude Code invocation) + PlanError
│       ├── implement.rs           # ImplementAgent (Ralph subprocess) + ImplementError
│       ├── review.rs              # ReviewAgent (adversarial Claude with JSON schema) + ReviewError
│       ├── judge.rs               # JudgeAgent (hybrid: DoDRunner + HermesLlmJudge) + JudgeError
│       ├── agents_md.rs           # Task-level AGENTS.md read/write/append + extract_patterns
│       ├── git_ops.rs             # commit_smart + ensure_ralph_excluded + create_branch
│       ├── receipt.rs             # Receipts, ImplementMetrics, ReviewSummary (final-assembled output types — NOT in domain.rs)
│       ├── persistence.rs         # Per-state Persistable impls + TaskWorkspace helpers
│       ├── error.rs               # AlpsError taxonomy (thiserror)
│       ├── agent.rs               # Sealed Agent trait + EmptyInput
│       ├── workdir_guard.rs       # v0.4 sentinel debounce against auto-reinvoke
│       └── domain.rs              # Plan, Review, Implementation, Judgment, etc. (newtypes + IDs)
└── alps-cli/
    ├── Cargo.toml
    └── src/
        ├── main.rs                # CLI entry — `alps run "prompt" [--workdir] [--force] [--deliverable-path]`
        └── detect.rs              # v0.7.1+3 auto-detect --deliverable-path from prompt text (stdlib only)
├── scripts/                       # Vendored from snarktank/ralph
│   ├── ralph.sh                   # Ralph loop runner (must be executable)
│   └── CLAUDE.md                  # Ralph's Claude Code prompt
└── tasks/                         # Per-task workspaces, git-committed per state
```

## 7. Per-Task File Structure

Each task gets a directory under `tasks/`. Git is the main history — every artifact is committed.

The actual file structure (as of v0.7.2, persisting via `persistence.rs`):

```
tasks/<task-id>/
├── prompt.md                      # Initial prompt (canonical)
├── plan.json                      # Latest plan (typed `Plan` from `domain.rs`)
├── implementation.json            # Implementation artifact (v0.7+ — persists `Implementation` struct including `deliverable_path`)
├── review.json                    # Latest review findings (typed `Review`)
├── receipts.json                  # Final receipts (only on Done; `Receipts` from `receipt.rs`)
├── feedback.json                  # Last rejected feedback (only on Rejected; appended to prompt on next attempt)
├── AGENTS.md                      # Cross-agent memory — `## Codebase Patterns` from ralph → review/judge/next-plan
└── implementation/
    └── ralph/                     # Ralph's working dir (separate git repo, excluded from parent via `<workdir>/.git/info/exclude`)
        ├── prd.json               # 1:1 mapping from Plan.stories → Ralph's userStories format
        ├── progress.txt           # `## Codebase Patterns` header for orchestrator extraction
        ├── .ralph-result.json     # ImplementMetrics (stories_passed, iterations, elapsed_secs)
        ├── .codex-last-message.txt# Codex completion signal (`<promise>COMPLETE</promise>` extraction)
        ├── CLAUDE.md              # Ralph's Claude Code prompt (vendored)
        ├── ralph.sh               # Ralph's loop runner (vendored)
        └── *.git/                 # Nested git repo, excluded from parent
```

**Branch creation** — `git_ops::create_branch(dir, branch_name)` is idempotent (reuses an existing branch instead of erroring "already exists"). The CLI calls it at task start; on retry the same branch is reused.

**Per-task state files** are tracked on the per-task branch `alps/<task-id>` but gitignored on `main` so they don't pollute the source tree.

**Auto-commit** — `commit_smart` is the trailing commit at task end. It checks `git status --porcelain` first; if nothing changed, it's silent (not NothingToCommit noise). Only commits when the per-task state actually changed since the last iteration.

## 8. The Outer Loop

```rust
// alps-core/src/loop_.rs — actual signature is RECURSIVE, not a loop { } block
// (loop{} doesn't work with type-state: `task = task.method(...)` doesn't compile
//  after the assignment, task is still typed as the original state. Recursive
//  function calls with `let task = task.method(...)` shadowing work cleanly.)
pub async fn drive(
    task: Task<Idle>,
    plan: &PlanAgent,
    implement: &ImplementAgent,
    review: &ReviewAgent,
    judge: &JudgeAgent,
) -> Result<Task<Done>, AlpsError> {
    let plan_input = task.prompt.clone();
    let plan_out = plan.run(plan_input).await
        .map_err(|e| AlpsError::PlanAgent(Box::new(e)))?;
    let task = task.plan(plan_out);
    persist(&task)?;

    let impl_input = task.state.plan.clone();
    let impl_out = implement.run(impl_input).await
        .map_err(|e| AlpsError::Implement(Box::new(e)))?;
    let task = task.implement(impl_out);
    persist(&task)?;

    let review_out = review.run(EmptyInput).await
        .map_err(|e| AlpsError::ReviewAgent(Box::new(e)))?;
    let task = task.review(review_out);
    persist(&task)?;

    let judgment = judge.run(EmptyInput).await
        .map_err(|e| AlpsError::Judge(Box::new(e)))?;
    match task.judge(judgment) {
        Ok(done) => {
            persist(&done)?;
            Ok(done)
        }
        Err(rejected) => {
            persist(&rejected)?;
            // append feedback to prompt, reset to Idle, recurse
            let history = vec![]; // populated by persistence::record_attempt
            drive(rejected.reset(history), plan, implement, review, judge).await
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
- [x] **Rust DoD path** — verified end-to-end by Rust smoke (2026-07-27, herdr `wA6:p1`, `cargo test --quiet` exit 0).
- [x] **Node DoD path** — verified end-to-end by Node smoke (2026-08-02, herdr `wAK:p1`, `npm test` exit 0).
- [x] **Go DoD path** — verified end-to-end by Go smoke (2026-08-03, herdr `wAM:p1`, `go test -v -count=1` exit 0).
- [x] **Plan retry-on-parse-fail** — landed in `6a414a8`. `PlanAgent::run` retries up to `max_retries=3` on `PlanError::Parse`. 5 deterministic tests cover the contract.
- [x] **Review + Judge retry-on-parse-fail** — landed in `894be6b`. Same pattern; `JudgeError::Parse` variant added.
- [x] **Multi-iteration ralph** — landed in `06d916d`. The 20-iteration safety net now routes through Judge (was a hard error before). 3 new integration tests with fake ralph.sh scripts.
- [x] **Recursive artifact collection** — landed in `fd35ff5`. `read_artifacts` walks `ralph_dir` recursively (was non-recursive `std::fs::read_dir`), so subdirectory source trees land in `Implementation.artifacts`. +1 regression test.
- [x] **--deliverable-path flag** — landed in PR #2 (2026-08-01, commit `e6fce8a`). Routes `read_artifacts`, the Judge's `read_files`, and the Review's `read_files` to walk the deliverable path. +4 tests (110 → 114).
- [x] **Auto-detect --deliverable-path from prompt** — landed in PR #4 (2026-08-03, commit `4c395a4`). 14 unit tests in `alps-cli/src/detect.rs`. Operator no longer needs to pass the flag for the common "build at /tmp/foo" prompt shape.
- [x] **CI (GitHub Actions)** — landed in PR #1 (2026-07-31, commit `6b27037`). `.github/workflows/ci.yaml` runs `cargo build --workspace --all-targets` + `cargo test --workspace --all-targets` + release build smoke on every push to main and every PR.
- [x] **Real reject-path smoke** — verified end-to-end via the CRUD smoke (2026-07-30, foreground diagnostic). 4 outer iterations, 3 rejects (each catching a distinct real defect), 4th iteration accepted.

### Phase 3 — advanced

- [ ] Persistent task queue (SQLite)
- [ ] Cross-task learning (reuse feedback patterns)
- [ ] Web UI for monitoring
- [ ] Multi-model judge (judge ensemble)
- [x] ~~Per-task branches in git (one branch per task)~~ (landed in `f452ca3`)
- [x] ~~CI (GitHub Actions on `klampatech/alps`)~~ — landed in PR #1 (2026-07-31)
- [ ] **Cost ceiling per task** — *DEFERRED 2026-08-03 per klampa*. Kyle runs ALPS off a $20/mo coding plan with 5-hour resets, so cost is not a barrier. Skip-list: do NOT propose this unprompted. Revisit only if multi-day ALPS-on-real-work becomes routine.
- [ ] **Mock-agent happy-path test** — partial. `for_test` constructors landed in `731fbd3` and the reject-path test (`drive_rejects_then_passes_appends_feedback_to_next_plan`) is comprehensive. The symmetric happy-path test (`drive_passes_first_try`) is still missing — Tier 2 + smoke1 verified it in production but no unit test pins the contract. *Promoted to active — see §12 item 3.*

### Phase 3.5 — Tier 4+ (post-orchestrator hardening)

Verified end-to-end smokes that exercise the full pipeline (not unit tests):

- [x] **Tier 1** — Node.js CRUD API (4 endpoints, stdlib sqlite3, pytest) — verified 2026-07-30
- [x] **Tier 2** — Python FastAPI CRUD + vanilla-JS frontend (6 endpoints) — verified 2026-07-30
- [x] **Tier 2.5** — Node.js module (`npm test`) — verified 2026-08-02, herdr `wAK:p1`
- [x] **Tier 2.5b** — Go module (`go test`) — verified 2026-08-03, herdr `wAM:p1`
- [x] **Tier 3** — Vite + React + TypeScript full-stack weather dashboard — verified 2026-08-03, herdr `wAN:p1` (Playwright screenshot delivered; deliverable verified, Judge verdict incomplete due to `tee | log` SIGPIPE)
- [ ] **Tier 4** — TBD. Candidates: full-stack with real backend (Tier 3 + custom server), monorepo (Turborepo/Nx), mobile (React Native + Expo), infra (Terraform + Ansible), PDF/Excel deliverables. Each tier needs a `references/tierN-spec.md` draft and a `plan-then-execute` gate before firing.

## 10. Agent integrations

| Agent | Runtime | Invocation | Input | Output |
|---|---|---|---|---|
| **Plan** | Claude Code | `cat prompt.md \| claude -p` | `Prompt` | `Plan` (parsed from JSON) |
| **Implement** | Ralph + Codex | `./ralph.sh [--max-iters N]` | `Plan` (→ `prd.json`) | `Implementation` (parsed from git log + progress.txt) |
| **Review** | Claude Code | `cat impl.md \| claude -p` | `Implementation` | `Review` (parsed from JSON) |
| **Judge** | Hybrid: structured DoD + Hermes (LLM) | (in-process + subprocess) | `JudgeContext` (plan + impl + review) | `Judgment` |

For MVP, Plan and Review use JSON-output prompts. Implement wraps Ralph. Judge is the most interesting — see open questions.

## 11. Resolved Decisions

### 11.1 Judge — Hybrid (verifiable DoD + LLM) — resolved 2026-07-26, model swap 2026-07-30

- **Two-stage**: structured runner (DoD / cargo test / pytest) + LLM judge.
  Both must clear for PASS; LLM stage has Critical-only veto.
- **Implementation**: `HermesLlmJudge` in `alps-core/src/judge.rs`. Spawns
  `claude --dangerously-skip-permissions -p --model <model>` (Claude Code CLI)
  with the verdict prompt on stdin. No separate "Hermes" CLI exists; the
  struct name stuck from the original spec, not from the runtime.
- **Models** (as of 2026-07-30):
  - **Judge** = `claude-opus-4` (Opus alias → MiniMax-M3 on this host).
    Dedicated high-quality model for the judgment slot.
  - **Plan + Review** = `claude-sonnet-4` (Sonnet → MiniMax-M2.7).
    Cheaper for the longer structured-output prompts.
  - Swap rationale: per-attempt cost matters most where the loop is
    expensive (the judgment that decides accept/reject) so the higher-tier
    model earns its keep there. Plan + Review are bigger prompts but
    cheaper in absolute spend.
- **Failure modes** (see also §12 / Runaway Judge retry):
  - LLM parse failures → `JudgeError::Parse` → retry up to
    `max_retries=3` (1 original + 2 retries).
  - Spawn / schema / unknown-verdict errors → `JudgeError::Llm` →
    propagate immediately, no retry.
- **Why named "HermesLlmJudge"**: original spec-time decision (2026-07-26)
  referenced "Hermes" for the LLM Judge slot. The actual implementation
  chose Claude Code for the easier subprocess ergonomics. The struct name
  is preserved for backwards compat. Receipts already record
  `judge_model: "<real-model-id>"` so cost attribution is accurate
  regardless of what the code calls the role.

In the original (pre-swap) wording, the two stages were:

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

This is the live roadmap as of 2026-08-03. Items in **bold** are
load-bearing for "ALPS works" claims. Items below the line are
quality-of-life or scale concerns.

1. ~~**Deliverable-outside-workdir gap, A (prompt-side)**~~ — shipped 2026-07-30, commit `be4fd85`. Closed by the prompt-template guard line ("Write everything inside the workdir..."). Tier 2 CRUD smoke + Tier 2.5/2.5b smokes have all used this guard.
2. ~~**Deliverable-outside-workdir gap, B (alps-side `--deliverable-path` flag)**~~ — shipped 2026-08-01, **PR [#2](https://github.com/klampatech/alps/pull/2)** commit `e6fce8a` (squashed at `a2ef8fd`). 4 new tests (110 → 114).
3. **Mock-agent happy-path test** — *promoted to active 2026-08-03*. We have the reject-path test (`drive_rejects_then_passes_appends_feedback_to_next_plan`, comprehensive). The symmetric happy-path test (`drive_passes_first_try`) is still missing — Tier 2 + smoke1 verified it in production but no unit test pins the contract. Add `drive_passes_first_try` + a multi-iteration happy-path test (3 Plan→Implement→Review→Judge round-trips, all PASS). Mock-agent fixtures already exist (`for_test` constructors in each agent module); the work is wiring up the test driver.
4. ~~**Spec §2.1 / §5.3 sync**~~ — *shipped 2026-08-03, this revision*. Major drift cleanup: §2.1 now reflects the `.codex-last-message.txt` completion signal + `read_artifacts` recursive walker; §5.2 reflects `Receipts` moving to `receipt.rs` + `Implementation` gaining `metrics` and `deliverable_path`; §5.3 reflects the `Agent` trait redesign (associated types + sealed + `EmptyInput`); §5.4 reflects `prompt` moving from state structs to `Task<S>` itself; §6 reflects the actual module layout (agents_md, workdir_guard, detect.rs); §7 reflects the actual per-task file structure (`implementation.json`, `feedback.json`, `AGENTS.md` at top level); §8 reflects the recursive `drive` driver; §9 marks Node/Go/Mock-test work appropriately; §12 (this section) reflects the closed items.
5. **alps-source `AGENTS.md` / `CLAUDE.md`** — when alps runs against itself, the workdir-level AGENTS.md starts empty. Worth seeding the alps source repo with project conventions (build commands, test invocation, commit-hygiene rules, test isolation).
6. **Cost ceiling** — *DEFERRED 2026-08-03 per klampa*. Kyle runs ALPS off a $20/mo coding plan with 5-hour resets, so cost is not a barrier. Skip-list: do NOT propose this unprompted. Revisit only if multi-day ALPS-on-real-work becomes routine.
7. ~~**More DoD project types**~~ — *shipped 2026-08-02, **PR [#3](https://github.com/klampatech/alps/pull/3)** commit `0d840c1` (squashed at `bed70f9`)*. Node + Go wired into `DoDRunner.detect_project_type` + `test_command_for_each_type`. Verified by Node smoke (2026-08-02, herdr `wAK:p1`, 4 outer iterations, `[judge:structured] detected project type: node → running: npm test --silent → PASS` on every iteration) and Go smoke (2026-08-03, herdr `wAM:p1`, 3 outer iterations, `[judge:structured] detected project type: go → running: go test ./... → PASS` on every iteration). 4 new tests (114 → 118). **Tier 3 unblocked.**
8. **Auto-detect `--deliverable-path` from prompt** — *shipped 2026-08-03, **PR [#4](https://github.com/klampatech/alps/pull/4)** commit `4c395a4` (squashed at `e23ec6f`)*. `alps-cli/src/detect.rs` (stdlib-only, 14 unit tests). 3-way override: explicit `--deliverable-path` always wins, prompt-derived wins when the flag is empty and the prompt mentions a build path, falls back to `--workdir` otherwise. Verified by Node smoke (herdr `wAP:p1`, 2026-08-03, 5/5 stories, 182s implement, Judge `claude-opus-4` ACCEPTED, fired without `--deliverable-path` flag).
9. **Orchestrator death mid-*implement.run* — ROOT CAUSE IDENTIFIED 2026-08-07 via smokes #13 + #14, prompt-side fix VERIFIED via smoke #15** The orchestrator process is **killed by codex itself** via a `pkill -f "uvicorn.*app.main:app --port 800..."` invocation that codex runs as a pre-step before starting uvicorn (to clean up stale uvicorn processes from prior iterations). The regex `uvicorn.*app.main:app --port 800` matches the **alps parent process's `/proc/<pid>/cmdline` argv**, because alps's argv contains the entire prompt text as its first positional argument, and the prompt template includes the literal example command `uvicorn app.main:app --port 8000` (in the `backend_uvicorn_startup.log` artifact spec). When `pkill -f` walks `/proc/<pid>/cmdline` for every process, it sees the alps process whose argv matches the pattern, and SIGTERMs it — orphaning codex (which keeps running under systemd since SIGTERM doesn't cascade without process-group signaling). **Reproduction (smoke #13, 2026-08-07):** strace captured `pkill` (PID 1336638) spawned by a bash subshell (1336629) of codex (1320588) at unix_ts 1786118743.157, executing `pkill -f "uvicorn.*app.main:app --port 800..."`. The same pkill then SIGTERMed both smoke13's alps (1314065) AND smoke14's alps (1314236) within the same millisecond — proving the kill is from codex, not from any external orchestrator. **History that masked the cause:** smokes #5–#10 all showed orchestrator death at varying iter counts (1, 2, 3+) which originally suggested an iter-boundary bug. The actual cause is **operationally variable** — `pkill -f` only fires when codex runs its uvicorn-cleanup step, which happens at different story numbers depending on which verification steps codex reaches. Smokes that completed before codex reached the uvicorn-cleanup step (e.g. smoke #12's 8/8 happy-path) appeared to die on self-cleanup; smokes that got codex past the uvicorn-cleanup step (e.g. smoke #13's US-011 capture, smoke #14's US-005 capture) actually get killed by codex's pkill. **Fix candidates (ranked by ROI):** (i) **prompt-side placeholder (~5min, RECOMMENDED, ship now, smoke-verified via smoke #15)** — change the prompt template to never include the literal string `uvicorn app.main:app --port 800` in the captured-artifact spec; use a placeholder like `<uvicorn-cmd>` or break the pattern across newlines so `pkill -f` regex doesn't match alps argv. Shipped in the alps skill's `tier-prompt-recipes.md` and `tier4-spec.md` 2026-08-07. (ii) **alps-side argv-cleanup (~30 min, PENDING)** — when alps forks ralph.sh / spawns codex, strip the literal example command strings from the prompt before passing it as argv; load the prompt from a temp file via `xargs -a /tmp/prompt.txt` instead of inline. More robust but adds plumbing. (iii) **alps-side `setpgid` + `prctl(PR_SET_PDEATHSIG)` (~1-2 hr, PENDING)** — make the orchestrator immune to stray SIGTERMs from sibling codex invocations by running ralph in its own process group with the orchestrator as parent-death-signaled to children. Heavy hammer, last resort. **Recommendation:** option (i) is shipped and smoke-verified; option (ii) is the next PR for belt-and-suspenders defense; option (iii) deferred until (ii) proves insufficient. **STATUS 2026-08-07:** root cause identified + prompt-side fix smoke-verified; superseded (a)/(b)/(c) investigation candidates from prior version of this item removed. See items 9.5 (fix candidate (i)) and 9.6 (new bug found via smoke #15) below.

9.5. **§12 item 9 prompt-side fix (avoid `pkill -f "uvicorn app.main:app --port 800"` matching alps argv)** — *promoted to active 2026-08-07; smoke-verified 2026-08-07 via smoke #15.* Root cause of item 9 is the prompt template including the literal example command `uvicorn app.main:app --port 8000` (and/or any other "service-name + module:app + port" combo). When codex later runs `pkill -f "uvicorn.*app.main:app --port 800"` as a pre-step to clear stale uvicorn processes, the regex matches alps's own argv and kills the orchestrator. **Fix:** rewrite the prompt template's captured-artifact spec to either (a) use a placeholder like `<your-uvicorn-cmd>` instead of the literal example, or (b) split the example command across newlines so the regex doesn't match (e.g., put `uvicorn` on one line and `app.main:app --port 8000` on the next with a comment between). The Tier-4 prompt template (in the alps skill's `references/tier-prompt-recipes.md` and `references/tier4-spec.md` line) needs this update before any Tier-4 smoke is re-fired. **Smoke #15 verified the fix end-to-end (2026-08-07, herdr workspace `wEM:p1`, 213s total runtime):** the rewritten Tier-4 prompt (`/tmp/alps-tier4-notes-prompt-15.txt`) replaced the literal `uvicorn app.main:app --port 8000` with `<run your local FastAPI service in the background, e.g. via uvicorn with the appropriate :app module and port>`. Smoke #15's alps argv was confirmed clean of the `uvicorn.*app.main:app --port 800` regex pattern. Codex ran pkill but with a different regex (`pkill -f 'uvicorn.*8765'`, matching the port it actually used for its own uvicorn verification — not matching alps argv), and alps survived. **However, smoke #15 also surfaced a SECOND bug, separate from item 9:** codex emitted `<promise>COMPLETE</promise>` while prd.json still had 4 stories at `passes: false` (ralph.sh's `all_stories_pass` cross-check correctly rejected the false-positive, but the orchestrator's max-iterations cap on the inner ralph loop appears to have been hit), and alps self-SIGTERMed after only 5/9 stories marked passing in prd.json (8/9 commits landed in ralph nested git — codex commits features but doesn't always update prd.json's `passes: true` field, so alps sees the deliverable as incomplete and exits). The orchestrator self-SIGTERM sender is an unidentified ephemeral process (PID 1374425 in strace, not findable in the proctree-post log) — likely a codex child. This second bug is filed as §12 item 9.6 below; smoke #15 PROMPT-FIX verification was successful but the smoke as a whole did not reach a Judge ACCEPT verdict because of this unrelated bug.

9.6. **Codex emits `<promise>COMPLETE</promise>` while `prd.json` still has stories at `passes: false` — orchestrator self-SIGTERMs early on incomplete deliverable** — *promoted to active 2026-08-07 (smoke #15 surfaced it as a side-effect of item 9 verification).* Pattern: codex commits features to the ralph nested git (e.g., `feat: US-NNN` commits land), but does NOT update `prd.json`'s `passes: true` field for those stories. The ralph.sh `all_stories_pass` cross-check (PR `a62c91d`, item 9's ralph-side guard) correctly detects this and does NOT emit "Ralph completed all tasks!" — ralph continues iterating. **However**, after the inner ralph loop hits `MAX_ITERATIONS=20` (or some equivalent cap), ralph.sh exits with status 1, and alps's `ImplementAgent` reads `prd.json` regardless of exit code (per PR `06d916d`). Alps sees `prd.json` with N-of-M stories still at `passes: false` and produces an `ImplementError::IncompleteStories` error (per PR `209b2d4`)... but in smoke #15, the orchestrator instead **self-SIGTERMed cleanly** at the end of the inner ralph loop, bypassing the `IncompleteStories` guard. The strace SIGTERM sender was an unidentified ephemeral process (PID 1374425 in strace, not findable in proctree-post) — likely a codex child sending SIGTERM as part of its own cleanup. **Smoke #15 evidence:** prd.json ended with 5/9 passing; ralph nested git had 8/9 commits; orchestrator self-SIGTERMed at 213s; no Judge fired. **Fix candidate:** investigate the exact path from `ralph.sh exit 1` → orchestrator self-SIGTERM. The current `ImplementAgent` should be returning an `IncompleteStories` error to the outer loop, NOT self-terminating — if the orchestrator is bypassing the IncompleteStories guard, that's a regression on PR `209b2d4` (item 9 ralph-side guard). ~30 min to add a `print` trace to `alps-core/src/loop_.rs` between `task.implement(impl_out)` and the Review step, re-fire smoke #15, and confirm whether `ImplementError::IncompleteStories` is raised or whether the orchestrator self-exits earlier.

### Recently completed (just shipped)

- ~~**§12 item 4 — CI**~~ — landed 2026-07-31, **PR [#1](https://github.com/klampatech/alps/pull/1)**. `.github/workflows/ci.yaml` runs `cargo build --workspace --all-targets` + `cargo test --workspace --all-targets` + release build smoke on every push to main and every PR. Uses `Swatinem/rust-cache@v2` for the Rust target dir + `actions/cache@v4` for the cargo registry. Single job, ubuntu-latest only — no cross-platform matrix yet (Rust + cargo behave identically on linux/macOS/Windows for this codebase; revisit when we add wasm or platform-specific code). First CI run on PR #1 passed in 1m 38s with 110/110 tests. One Node 20 deprecation warning, informational, action@v4 targets Node 20 but runs on Node 24 (forced). Branch `ci/add-github-actions`, will be squash-merged when reviewed.
- ~~**§12 item 1A — Deliverable-outside-workdir (prompt-side recipe)**~~ — landed 2026-07-30 as a doc-only change. The ALPS skill's "Smoke test recipe" now explicitly tells the operator "keep the deliverable INSIDE the workdir" with a reference to Common Pitfall #16, and the demo prompt template ends with a guard line: "Write everything inside the workdir (do NOT create files under /tmp/, /home/, or any path outside the workdir)." Closes the common operator-forget case. B (alps-side `--deliverable-path`) shipped 2026-08-01 — see §12 item 1B entry below. C (auto-detect) remains open but deferred until we see another real failure that the flag doesn't cover.
- ~~**§12 item 1B — Deliverable-outside-workdir (alps-side `--deliverable-path` flag)**~~ — landed 2026-08-01, **PR [#2](https://github.com/klampatech/alps/pull/2)** commit `34b10a9`. New CLI flag `--deliverable-path <path>` (default = `--workdir`) routes `read_artifacts`, the Judge's `read_files`, and the Review's `read_files` to walk that path. `commit_smart_with_excludes` (new function alongside `commit_smart`) appends the path to `<workdir>/.git/info/exclude` only when the path is outside the workdir (idempotent, same `.git/info/exclude` mechanism as the v0.5 ralph nested-git exclude). `read_artifacts` defensively skips `tasks/` so a deliverable path that's a parent of the workdir can't re-introduce ralph's nested git. `Implementation` gains a `deliverable_path` field, persisted to `tasks/<id>/implementation.json` at the Implemented state. +4 tests (110 → 114). Closes the B half. C (auto-detect) remains open but deferred until we see another real failure that the flag doesn't cover.
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
