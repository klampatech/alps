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
| 2026-08-08 → 08-09 | `cbfd59a` ... `63b876a` ... `8eca6a7` ... `d92ad99` | **ralph.sh → in-process Rust library port** (9 commits on `refactor/alps-ralph-rust-port`). `alps-core/src/ralph.rs` (~895 lines) line-for-line port of bash: same state-file locations, same `OpenOptions::append(true)` for `tee -a /dev/stderr`, same `all_user_stories_pass` phantom-COMPLETE guard, same `sleep 2` between iterations, same `.ralph-result.json` write-on-every-exit-path. Stdin fix (`63b876a`) — spawn each tool with `Stdio::from(File)` instead of `Stdio::piped() + write_all + drop`. Tool-CWD fix (`74fe9f9`) — all three tool backends now set `.current_dir(&ralph_dir)`. Structured-DoD monorepo walk (`d92ad99`) — `detect_project_type` recurses one level into monorepo subdirs, skipping vendor/build dirs. Rate-limit/quota-awareness P1/P2/P3 follow-ups filed in SPEC §12 (`8eca6a7`). |
| 2026-08-09 | `2242916` | **PR #13 MERGED** — `Merge pull request #13 from klampatech/refactor/alps-ralph-rust-port` (klampatech merge, 5h43m open). 1905/254/11, build+test `pass 1m55s`. ralph.sh → in-process Rust library port. PR body includes wrapper stream separation (P0#1), receipts.json contract pin, structured-DoD monorepo recursion (`d92ad99`, +6 tests 169→175), tool-CWD fix, rate-limit/quota-awareness P1/P2/P3 follow-ups. Plan/Ralph 9-vs-10 mismatch (P0#2) kept separate per "What's NOT in this PR." |
| 2026-08-09 | `5c5de77` | **Post-merge cleanup** — dropped `scripts/ralph.sh` (282 lines), collapsed `enum RalphMode { Rust, Shell }` to `{ Rust }`, replaced `ImplementConfig::ralph_path` with `scripts_dir`, renamed `copy_ralph_files` → `copy_prompt_files`. Test count unchanged at 175 (144 lib + 4 + 4 integration + 23 alps-binary). Smoke #21 was the runtime-verified exit criterion per the commit body. |
| 2026-08-10 | `3211bed` | **`drive_rejects_twice_then_passes_accumulates_agents_md` integration test** — closes §12 item 3 multi-iter accumulation contract. 7 assertions pin: drive() returns Ok(done) after reject→reject→pass, Plan/Judge each called exactly 3 times, iter-2 Plan sees iter-1 feedback, iter-3 Plan sees BOTH iter-2 (latest) AND iter-1 feedback (accumulated), all 3 Plan prompts distinct, iter-1 Plan has no feedback, AGENTS.md accumulates ralph's `progress.txt` patterns across iterations. Test count 175 → 176. SPEC §12 item 3 entry rewritten to reflect actual state. |
| 2026-08-10 | `d60fa33` | **§12 P1 — surface tool exit code on every Ralph iteration.** All three tool branches (codex / claude / amp) now log the child's `ExitStatus` via `elog!`. Previously `let _ = child.wait().await;` (codex) and `wait_with_output().ok()` (claude / amp) silently dropped the exit code — on a 429 / quota burn the only signal was the generic "iteration failed, retrying" line. Fix: capture ExitStatus, log `[ralph] codex exited with code N on iteration I/M (stderr mirror at <ralph_dir>/.ralph-stderr.log)`. The stderr mirror was already working; now the exit code is visible too. Acceptance test (`quota_exceeded_stderr_lands_in_ralph_stderr_log_and_loop_continues`) drives `ralph::run` with a fake codex that emits `429 rate_limit_error: quota exceeded` on stderr + exits 1; asserts the marker lands in `<ralph_dir>/.ralph-stderr.log` AND the loop continues past the non-zero exit (preserves bash's `\|\| true` semantics). Test count 176 → 177. |
| 2026-08-10 | (smoke #22) | **Tier-1 smoke GREEN** (post-P1 verification) — 1 attempt, 2/2 stories passed, 0 critical findings, 6/6 review assertions, Judge `claude-opus-4` ACCEPTED, 128s implement. `fib.py` + `test_fib.py` + `uv.lock` delivered; `pytest -q` reports `1 passed`. **P1 line confirmed in live log:** `[ralph] codex exited 0 on iteration 1/20` + `[ralph] codex exited 0 on iteration 2/20`. Receipts at `tasks/2026-08-10T161429-471d4618a6844d03830ba72d1b5459b2/receipts.json`. |
| 2026-08-10 | (smoke #22-tier4) | **Tier-4 smoke PARTIAL** — iter 1 succeeded (8/8 stories, 9 commits, 56 artifacts, 600s elapsed); Judge REJECTED on structured-DoD (`pytest -q` exit Some(2), likely CWD-related — the actual pytest run inside codex reported 10 passed); iter 2 plan expanded scope to 12 stories; codex killed mid-iter 2 reconnaissance phase (external SIGTERM — cause unconfirmed, possibly herdr babysitter). **Deliverable complete on disk:** `/tmp/alps-tier4-notes-22/` with full backend (FastAPI + Postgres + JWT + Alembic + bcrypt) + frontend (Vite + React + TS + Zustand + Vitest) + all 15 captured artifacts (5 Playwright screenshots, `backend_pytest_output.txt` showing `10 passed, 15 warnings`, `db_schema_dump.sql`, `users_in_db.txt`, `notes_in_db.txt`, `curl_flow.txt`, `filesystem_inventory.txt`, `tmp_listing.txt`). **P1 line confirmed:** `[ralph] codex exited 0 on iteration 1/20`. The post-`d92ad99` structured-DoD monorepo recursion fired correctly — that's what caught the `pytest -q` exit code mismatch between the Judge's structured stage and codex's runtime verification. |
| 2026-08-10 | `6c1fadf` | **Tier-4 cwd regression fix — Judge's structured-DoD stage now runs the test command from the matched subdir, not the deliverable root.** `detect_project_type(dir)` now returns `(ProjectType, PathBuf)` — the `PathBuf` is the dir where the marker was found (root-level markers → input dir; monorepo subdirs → the matched subdir). The Judge's `run_cmd_with_timeout` call now uses the returned `PathBuf` as cwd, so pytest runs from `backend/` (where `pyproject.toml` + `.venv` live) instead of the deliverable root. 9 existing test assertions updated to the new tuple shape; 2 new integration tests pin the Tier-4 monorepo case (positive: pytest passes from subdir; negative: pytest fails AND Judge reports FAIL — guards against the fix accidentally turning "wrong cwd" into "always PASS"). Live verification in tests shows the new path resolution: `[judge:structured] detected project type: python (test_root: /tmp/.../backend)`. **Test count: 177 → 179 (147 lib + 23 alps-binary + 5 + 4).** Build clean, no new warnings. **Surfaced by smoke #22-tier4 (2026-08-10):** Judge REJECTED on `pytest -q` exit Some(2) while codex's runtime pytest reported 10 passed. Root cause = `ModuleNotFoundError: No module named 'sqlalchemy'` because pytest ran from the deliverable root (no venv) and recursed into `backend/tests/` which imports sqlalchemy. |


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

ALPS is the **outer orchestrator**. Ralph is the **inner implement loop**, implemented as an in-process Rust library (`alps-core/src/ralph.rs`) so the orchestrator hot path never crosses a bash↔Rust IPC boundary. Composition contract:

- ALPS writes `prd.json` (a 1:1 mapping from `Plan.stories` to Ralph's `userStories` format) and `progress.txt` (with `## Codebase Patterns` header) into `tasks/<id>/implementation/ralph/<workdir>`.
- ALPS calls `alps_core::ralph::run(cfg: RalphConfig)` in-process. Ralph runs its own loop: read PRD → pick story → invoke tool (codex / claude / amp) → implement → test → commit → loop. Ralph exits with `completed: true` in `.ralph-result.json` when `<promise>COMPLETE</promise>` lands in `.codex-last-message.txt` (the codex-specific completion extraction; commits `799067d`–`752c41a`).
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
├── scripts/                       # Vendored Ralph prompt templates
│   ├── AGENTS.md                  # Ralph's Codex prompt (read by codex --AGENTS.md)
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
        ├── AGENTS.md              # Ralph's Codex prompt (read by codex --AGENTS.md)
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
3. **Mock-agent happy-path test** — *promoted to active 2026-08-03; partial progress 2026-08-10*. The `drive_passes_first_try` family (3 variants: basic pass-on-first-call, receipts.json-on-disk, AGENTS.md propagation) plus the multi-iteration `drive_rejects_twice_then_passes_accumulates_agents_md` (2026-08-10) now pin both the single-iter happy path AND the cross-iteration accumulation contract. Test count 175 → 176. The original "multi-iteration happy-path (3 Plan→Implement→Review→Judge round-trips, all PASS)" framing was structurally wrong — `drive()` terminates on first `Judgment::Pass`, so a multi-iter happy path is impossible by design. What multi-iter actually needs is the accumulation test (ralph patterns + feedback must reach the next Plan call), which is now in place.
4. ~~**Spec §2.1 / §5.3 sync**~~ — *shipped 2026-08-03, this revision*. Major drift cleanup: §2.1 now reflects the `.codex-last-message.txt` completion signal + `read_artifacts` recursive walker; §5.2 reflects `Receipts` moving to `receipt.rs` + `Implementation` gaining `metrics` and `deliverable_path`; §5.3 reflects the `Agent` trait redesign (associated types + sealed + `EmptyInput`); §5.4 reflects `prompt` moving from state structs to `Task<S>` itself; §6 reflects the actual module layout (agents_md, workdir_guard, detect.rs); §7 reflects the actual per-task file structure (`implementation.json`, `feedback.json`, `AGENTS.md` at top level); §8 reflects the recursive `drive` driver; §9 marks Node/Go/Mock-test work appropriately; §12 (this section) reflects the closed items.
5. **alps-source `AGENTS.md` / `CLAUDE.md`** — when alps runs against itself, the workdir-level AGENTS.md starts empty. Worth seeding the alps source repo with project conventions (build commands, test invocation, commit-hygiene rules, test isolation).
6. **Cost ceiling** — *DEFERRED 2026-08-03 per klampa*. Kyle runs ALPS off a $20/mo coding plan with 5-hour resets, so cost is not a barrier. Skip-list: do NOT propose this unprompted. Revisit only if multi-day ALPS-on-real-work becomes routine.
7. ~~**More DoD project types**~~ — *shipped 2026-08-02, **PR [#3](https://github.com/klampatech/alps/pull/3)** commit `0d840c1` (squashed at `bed70f9`)*. Node + Go wired into `DoDRunner.detect_project_type` + `test_command_for_each_type`. Verified by Node smoke (2026-08-02, herdr `wAK:p1`, 4 outer iterations, `[judge:structured] detected project type: node → running: npm test --silent → PASS` on every iteration) and Go smoke (2026-08-03, herdr `wAM:p1`, 3 outer iterations, `[judge:structured] detected project type: go → running: go test ./... → PASS` on every iteration). 4 new tests (114 → 118). **Tier 3 unblocked.**
8. **Auto-detect `--deliverable-path` from prompt** — *shipped 2026-08-03, **PR [#4](https://github.com/klampatech/alps/pull/4)** commit `4c395a4` (squashed at `e23ec6f`)*. `alps-cli/src/detect.rs` (stdlib-only, 14 unit tests). 3-way override: explicit `--deliverable-path` always wins, prompt-derived wins when the flag is empty and the prompt mentions a build path, falls back to `--workdir` otherwise. Verified by Node smoke (herdr `wAP:p1`, 2026-08-03, 5/5 stories, 182s implement, Judge `claude-opus-4` ACCEPTED, fired without `--deliverable-path` flag).
9. ~~**Orchestrator death mid-*implement.run* — codex's `pkill -f` matching alps argv**~~ — **RESOLVED 2026-08-07** via smokes #13-17 + **VERIFIED-STABLE via smoke #18 (2026-08-07)**. Three layered fixes: (a) prompt-side placeholder rewrite to remove `uvicorn app.main:app --port 8000` from the prompt template; (b) alps-side `setpgid(0,0)` + `prctl(PR_SET_PDEATHSIG, SIGTERM)` hardening (commit `91c747b`) — makes alps immune to herdr-pane-babysitter SIGTERMs AND ties alps's life to its parent shell; (c) `--prompt-file <PATH>` CLI arg (commit `2630e44`) — the STRUCTURAL fix. With `--prompt-file`, alps's `/proc/<pid>/cmdline` is ~50 chars (just the temp file path), so ANY `pkill -f <keyword>` pattern emitted by codex (`pkill -f vite`, `pkill -f uvicorn`, `pkill -f fastapi`, etc.) cannot match alps argv. See §12 items 9.5 and 9.7 below for the structural fix details. **Smoke #17 (2026-08-07, herdr workspace `wEP:p1`, 46min runtime, iter 1→reject→iter 2 accept):** prompt template uses `{{DELIVERABLE_PATH}}` placeholders, wrapper substitutes the actual `--deliverable-path` at launch; alps argv confirmed clean of `vite`/`uvicorn`/`npm`/`react`/`fastapi` keywords; 8/8 stories passed, 60 artifacts captured, Judge `claude-opus-4` ACCEPTED. **`receipts.json` written to `/tmp/alps-tier4-17-preserved/receipts.json` with `implement_metrics: 8/8 stories, 1 iteration, 710s elapsed, review_summary: 12/12 assertions, 0 critical findings`.** **Smoke #18 (2026-08-07, 60min runtime, iter 1→reject→iter 2 reject→iter 3 accept, fired via generic `alps-tier4-smoke-wrapper.sh`):** second consecutive Judge ACCEPT verdict using the new `--prompt-file` argv path; 7/7 stories, 12/12 review assertions, opus-4 judge; `receipts.json` at `/tmp/alps-tier4-18-preserved/receipts.json`. Strace negative-correlation: alps PID 1620833 was NEVER a `kill()` target across 5 `pkill` invocations spawned by codex (4× `pkill -f run.py`, 1× `pkill -f vite`); the `pkill -f vite` invocation (PID 1562137) made exactly 2 kill() calls targeting its own bash wrapper children, NOT alps. Alpid survived ~310s after `pkill -f vite` fired before exiting cleanly with `exit_group(0)`.

9.5. ~~**§12 item 9 prompt-side fix (avoid `pkill -f "uvicorn app.main:app --port 8000"` matching alps argv)**~~ — **SHIPPED 2026-08-07** (smoke #15 verified the prompt-side placeholder rewrite). But this turned out to be **insufficient on its own** because codex emits `pkill -f` patterns for ANY long-running service it spawns (vite, uvicorn, npm, fastapi, react-dev-tools, etc.) — and the prompt template's body still contains keywords like `vite`, `uvicorn`, `npm`, `react`, `fastapi` as legitimate package/tool names. The structural fix lives in §12 item 9.7.

9.6. ~~**Codex emits `<promise>COMPLETE</promise>` while `prd.json` still has stories at `passes: false`**~~ — **MISDIAGNOSED, REJECTED 2026-08-07**. Smoke #15 forensics (commits `91c747b` + `f65945d` history) revealed: codex DID update prd.json (9/9 stories `passes: true` in smoke #15's case, 8/8 in smoke #16), DID emit `<promise>COMPLETE</promise>`, and ralph.sh DID exit 0. The orchestrator was sitting idle in a futex for 2+ minutes after codex finished, then was SIGTERMed by an external PID (1374425 in smoke #15's strace) — NOT inside `implement.run`'s post-ralph path at all. The orchestrator never reached `ImplementError::IncompleteStories` because it died BEFORE ralph.sh's `status()` returned. The real killer was a herdr pane babysitter SIGTERM (item 9.5 above), not a prd.json update gap.

9.7. **§12 item 9.5 fix (ii) — `--prompt-file <PATH>` CLI arg (alps-side argv cleanup)** — **SHIPPED 2026-08-07** (commit `2630e44`). The structural fix for codex's `pkill -f` patterns. New CLI flag `--prompt-file <PATH>` reads the prompt from a temp file instead of argv. Wrapper creates the file with `mktemp -t alps-prompt.NN.XXXXXX.txt`, alps reads it on startup and (best-effort) deletes it. With this flag, alps's `/proc/<pid>/cmdline` contains only the file path (~50 chars) — NO prompt text, NO `vite`/`uvicorn`/`npm`/`react`/`fastapi` keywords. `pkill -f <keyword>` patterns emitted by codex have nothing to match. **Smoke #17 verification (2026-08-07):** `/proc/1535495/cmdline` confirmed clean of all 5 dangerous keywords; orchestrator survived 46min runtime including 2 outer iterations and reached a Judge ACCEPT verdict.

9.8. **Smoke wrapper deduplication — generic `alps-tier4-smoke-wrapper.sh <smoke-N>` template (SHIPPED 2026-08-07).** Replaced 5 per-smoke wrappers (`smoke-{13,14,15,16,17}-wrapper.sh`, each ~258 lines, 95% identical — differed only on smoke number, log prefix, workdir/deliverable paths, prompt file) with a single parameterized wrapper at `/tmp/alps-tier4-smoke-wrapper.sh`. Wrapper takes 5 required flags: `--smoke-number`, `--workdir`, `--deliverable-path`, `--prompt-template`, `--log-prefix`. All diagnostic machinery (strace attach, process tree snapshots, journalctl deltas, dmesg, panic/signal-handler side files, receipts preservation) is identical across smokes — only the flag values change. The `PRESERVE_DIR` is auto-derived from `LOG_PREFIX` via `${LOG_PREFIX%-stderr}-preserved`. **Smoke #18 verification (2026-08-07):** wrapper fired against the canonical prompt template (symlinked at `/tmp/alps-tier4-notes-prompt.txt` → `notes-prompt-17.txt`), received Judge ACCEPT verdict on iter 3 of 3 outer iterations (8/8 stories, 12/12 review assertions, opus-4 judge, 60-min wall clock). `/tmp/alps-prompt.18.waqhZo.txt` (7860 bytes) was created via mktemp, read by alps on startup, best-effort deleted. Prompt substitution: 359 occurrences of `--deliverable-path` value in stderr, 0 leaks of the old hardcoded path. Strace attached 17.2M lines over the 60-min run. `repos/alps` working tree: 5 new `resolve_prompt` unit tests in `alps-cli/src/main.rs` (commit `8079e98`) + 8 new bash tests in `tests/test_prompt_substitution.sh` cover the prompt-substitution + prompt-file contracts end-to-end. Workspace test count: 128 → 133 (with the known telemetry flake).

9.10. **§12 item 9.10 — `ralph.sh` → in-process Rust library port (IMPLEMENTED LOCALLY 2026-08-08; PR NOT YET RAISED; core commit `cbfd59a`, smoke #20 verification + stdin-fix commit `63b876a`).** Local branch: `refactor/alps-ralph-rust-port`. Ralph Wiggum (`scripts/ralph.sh`, 282 lines of bash) is the inner implement loop that drives `ALPS`'s `ImplementAgent::run`. Two consecutive production smokes burned us at the bash↔Rust IPC boundary: smoke #15 (2026-08-07) — orchestrator SIGTERM after ralph returned because the signal sender lived outside alps's process tree, requiring `setpgid(0,0)` + `PR_SET_PDEATHSIG` on both sides; smoke #18 (2026-08-07) — argv-leak, requiring the `--prompt-file` flag. Both workarounds are symptoms of the same root cause: the orchestrator hot path crosses a bash↔Rust IPC boundary via a subprocess. **The port eliminates that boundary.** New module `alps-core/src/ralph.rs` (~895 lines, including `RalphConfig` + `RalphResult` + `read_prd` + `all_user_stories_pass` + `run` + helpers) is a line-for-line port of the bash: same state-file locations in `ralph_dir` (not `script_dir` — the `test-state-file-location.sh` guard ported verbatim), same `OpenOptions::append(true)` for the `tee -a /dev/stderr` FIX #6 (preserves the orchestrator's earlier `elog!` writes), same `all_user_stories_pass` phantom-COMPLETE guard (the §12 item 9 ralph-side guard from commit `a62c91d`), same `sleep 2` between iterations, same `.ralph-result.json` write-on-every-exit-path semantics. New public API: `alps_core::ralph::run(cfg: RalphConfig) -> Result<RalphResult, RalphError>` (orchestrator's `ImplementAgent::run` now calls this in-process when `ImplementConfig::ralph_mode == RalphMode::Rust`, the default). Operator-facing CLI parity: `alps ralph --tool codex --max-iter 5 --ralph-dir /tmp/foo/` (the new subcommand in `alps-cli/src/main.rs`) is a thin wrapper over the same library function — no subprocess, no argv leak, no orchestrator-death window. **The Shell escape hatch (`RalphMode::Shell` → `exec scripts/ralph.sh`) is intentionally kept for one release as a rollback safety net** — set `ralph_mode = Shell` on `ImplementConfig` to use it. Removal is a follow-up commit after smoke #19 verifies the Rust path.


*Resolution (2026-08-09, post-merge cleanup):* The "follow-up commit after smoke #19 verifies the Rust path" referenced above was deferred past the PR #13 merge (smoke #19 was the RED-then-GREEN that proved the Rust path; smoke #21 verified it under Tier-4 load — 1,114s wall-clock, 8/8 stories, Judge `claude-opus-4` ACCEPT first try). Now that the Rust path is the only production-verified mode, the legacy escape hatch is gone:

- `enum RalphMode` has been collapsed to a single variant `Rust`. The `Shell` variant is deleted.
- `ImplementConfig::ralph_path` (the path to `scripts/ralph.sh`) is replaced by `scripts_dir` (the path to the alps-internal `scripts/` directory containing the vendored `AGENTS.md` / `CLAUDE.md` prompt files). The dispatch (`implement.rs::run`) now passes `scripts_dir` directly as `RalphConfig::script_dir`.
- `scripts/ralph.sh` (the 282-line bash) is deleted.
- `copy_ralph_files` (which copied `ralph.sh` + `AGENTS.md` + `CLAUDE.md` into the ralph workspace) is renamed `copy_prompt_files` (only copies the prompts). The `ralph.sh` step is gone.
- `SKIP_FILES` no longer mentions `ralph.sh`.
- The 3 ralph-exit-code tests (`implement_returns_partial_implementation_when_ralph_exits_nonzero`, `implement_returns_full_implementation_when_ralph_exits_zero`, `implement_errors_when_ralph_exits_nonzero_and_prd_missing`) previously drove a fake `ralph.sh` via `RalphMode::Shell`. They now drive `alps_core::ralph::run` via a new `cfg(test)`-only `test_ralph_runner` hook on `ImplementAgent` — same observable behavior, no fake script, no subprocess.
- The `alps-cli` `run_task` no longer needs `ralph_path` — it sets `scripts_dir = alps_root.join("scripts")`.
- Workspace test count unchanged at 175 (144 lib + 4 + 4 integration + 23 alps-binary). Compile green, all tests pass.

**Smoke verification remains load-bearing:** `RalphMode::Rust` is the only mode ALPS ships, and it's the path smoke #21 verified end-to-end. The smoke wrapper at `/tmp/alps-tier4-smoke-wrapper.sh` is unchanged. If a future ALPS change needs to test the legacy subprocess semantics, the old `RalphMode::Shell` implementation can be reintroduced from git history (commit `d92ad99` predates this cleanup; the `Shell` variant and `scripts/ralph.sh` exist there).

**Smoke #19 verification (2026-08-08, RED, then fixed via commit `63b876a`):** `/tmp/alps-tier4-smoke-wrapper.sh --smoke-number 19` launched against the canonical Tier-4 notes-app prompt with `ralph_mode = Rust` (the new default). After 20/20 iterations at 24:36 wall-clock, `implementation.json` showed `0/11 stories_passed` and Ralph correctly reported `completed: false`. **Root cause:** the port used `Stdio::piped()` + `stdin.write_all(...)` + `drop(stdin)` to push the prompt bytes then close the FD; bash's `ralph.sh` line 207 used `codex ... < "$RALPH_AGENTS"` which inherits the file FD as stdin (stays open until codex exits). Codex's tool router detects a closed-stdin session at startup and refuses to run with `write_stdin failed: stdin is closed for this session; rerun exec_command with tty=true to keep stdin open`. **Fix (commit `63b876a`):** spawn each tool (codex, claude, amp) with `Stdio::from(File)` pointing at the prompt file. Same semantics as bash's `< file` shell redirect — FD is open, tool reads from it directly. Applied to all three tool branches preemptively. `cargo build` clean, `cargo test` 168 passed (no new warnings).

**Smoke #20 verification (2026-08-08, GREEN-with-caveats):** post-fix smoke (`--smoke-number 20`) launched in herdr pane `wE1:p4`. Final state: **alps ran for 5744s total** (~95:44), **0 SIGTERM markers**, **0 panic markers**, **155M strace lines** captured. **Inner Rust Ralph succeeded**: `.ralph-result.json` shows `{"iterations": 16, "elapsed_secs": 4475, "completed": true}`; `implementation.json` shows **10 commits** (US-001..US-009 + initial setup, exactly the Tier-4 spec story breakdown) and **56 artifacts** captured. **Deliverable**: `/tmp/alps-tier4-20-preserved/deliverable.tar.gz` is 59MB and contains all 17 Tier-4 artifacts (`backend_pytest_output.txt`, `backend_uvicorn_startup.log`, `frontend_build_output.txt`, `frontend_dev_startup.log`, `db_schema_dump.sql`, `users_in_db.txt`, `notes_in_db.txt`, `curl_flow.txt`, 5 Playwright screenshots, `filesystem_inventory.txt`, `tmp_listing.txt`, `frontend_test_output.txt`, `frontend_typecheck_output.txt`).

**Smoke #20 caveats vs. smoke #18 (1:1 parity claim):** (1) wall-clock 5744s vs 3580s (1.6×); (2) the trustworthy filesystem/`[alps-diag]` reconstruction shows **2 outer iterations**, not the ~8 iterations initially inferred from the raced log: first pass implemented 9/9 and reached a legitimate Judge Reject; the second Plan produced a fresh 10-story PRD and its Ralph run ended 0/10, triggering `AlpsError::Implement(IncompleteStories)`; (3) no `receipts.json` was expected because the run ended on that error rather than `Ok(done)`. Therefore smoke #20 proves **structural parity of the Rust Ralph hot path**, but not successful end-to-end outer-loop convergence or wall-clock parity. The earlier 8-iteration / repeated-ACCEPT interpretation is retracted; it came from the dual-writer log race documented below.

**Resolution (2026-08-08, post-investigation):**

*Receipts.json bug — RESOLVED, no actual bug.* `drive()` returns `Ok(done)` → `persist_task(&done, workspace)` → `workspace.write_receipts(&self.state.receipts)` at `persistence.rs:115` (single line: `std::fs::write(self.receipts_path(), json)?`). The `?` propagates errors so the file write either succeeds or `AlpsError::Persistence` returns to the CLI which exits non-zero. **Test added (`drive_passes_first_try_writes_receipts_json_on_disk` in `alps-core/src/loop_.rs`) confirms:** with a scripted `Judgment::Pass`, `drive()` returns `Ok(done)` AND `workspace.receipts_path()` exists on disk with valid JSON containing the canned `task_id` and `judge_model`. The contract is pinned. Test count: 168 → 169.

The smoke #19 + #20 "missing receipts" was **not** a code bug — it was the orchestrator **correctly** exiting via `AlpsError::Implement(IncompleteStories)` (final iteration had `0/10 stories_passed != 10/10`, implement-completion guard caught it at `loop_.rs:107-117`). With that Err, `drive()` never reaches `Ok(done)`, so `write_receipts` is never called. The smoke wrapper's "no receipts.json found" warning is the **correct** behavior for an Err termination.

*Dual-writer log race — ROOT CAUSE OF CONFUSION.* `/tmp/alps-tier4-smoke-wrapper.sh` (line 184, 194) sets `ALPS_TELEMETRY_LOG="${STDERR_LOG}"` and redirects stderr to the same file via `2>> "${STDERR_LOG}"`. The `elog!` macro (`telemetry.rs:150-172`) writes both to stderr (via the shell `2>>`) AND directly to the telemetry file via `OpenOptions::append(true)`. **Two writers, one file, different open semantics.** POSIX `O_APPEND` guarantees intra-writer atomicity but **not** inter-writer ordering. The result: `[done] accepted` markers (from `loop_.rs:162`) appeared 31 times in `STDERR_LOG` for smoke #20, while the file system shows ZERO `Ok(done)` events — every `Ok(done)` line in the log is a write-order artifact from the dual-writer race. This makes line-number-based log analysis unreliable. **Resolution:** analyze the orchestrator's actual sequence via file-system state (mtimes of `feedback.json`, `plan.json`, `implementation.json`, `review.json`, `receipts.json`) and `[alps-diag]` lines (which use `eprintln!` only, no dual-write), not the `elog!` markers.

*Actual smoke #20 sequence (reconstructed from file mtimes + `[alps-diag]` traces):* (1) outer iteration 1: Plan → Implement (Ralph shipped 9/9 stories) → Review (writes review.json @16:02) → Judge → Reject (writes feedback.json @16:02); (2) outer iteration 2: Plan → Implement (Ralph ran 20/20 iter on a fresh 10-story prd.json, 0/10 passing) → implement-completion guard FAILED → `AlpsError::Implement(IncompleteStories)` → CLI exits non-zero with the error message at `loop_.rs:47111`. **Total: 2 outer iterations, 1 Reject, 0 `Ok(done)`.** The 31 `[done] accepted` log lines are noise from the dual-writer race.

*Stability follow-up investigation results (2026-08-09):*

1. **P0 — wrapper stream separation: FIXED + LOCALLY VERIFIED.** `/tmp/alps-tier4-smoke-wrapper.sh` now keeps `${STDERR_LOG}` for process FD-2 and writes structured `elog!` output to a distinct `${LOG_PREFIX}-telemetry.log`; both `ALPS_TELEMETRY_LOG` and `--telemetry-log` target the latter. Metadata reports both marker counts and both paths. `/tmp/alps-tier4-smoke-wrapper-test.sh` is a deterministic guard: uniquely numbered markers must appear exactly once and in order in their own streams, with no cross-contamination, and static assertions reject any future wiring back to `${STDERR_LOG}`. `bash -n` and the deterministic guard pass. **Remaining acceptance gate:** use this wrapper layout on the next Tier-4 smoke and confirm the run is reconstructable directly from the separated logs.

2. **P0 — outer-loop Plan→Implement story-contract: ROOT-CAUSED; the 9→10 change was legitimate, the Rust tool CWD was the bug.** Smoke #20 pass 1's nine stories were an implementation-oriented decomposition; Judge then rejected three missing proof obligations (`frontend_test_output.txt`, `tmp_listing.txt`, and DB rows synchronized with `curl_flow.txt`). Pass 2 correctly replanned into ten stories by splitting the formerly combined proof work into `US-9` (frontend Vitest execution) and `US-10` (E2E/runtime evidence). `plan.json` and generated `prd.json` agree exactly on all ten IDs, so there is **no Plan→PRD count mismatch and no smoke-template drift**. The failure happened after that: the Rust Ralph port spawned codex/claude/amp without `.current_dir(&ralph_dir)`, while the Shell path did set that CWD and the vendored `AGENTS.md` explicitly promises `prd.json`, `progress.txt`, and `.git/` are in CWD. Smoke #20's own progress log recorded this discrepancy. On pass 2 codex emitted COMPLETE from the ALPS source-repo CWD without updating the fresh ten-story PRD, leaving 0/10 and correctly tripping `IncompleteStories`.

   **Fix + regression:** all three Rust tool branches now set `.current_dir(&ralph_dir)`. New integration test `tool_backend_runs_with_ralph_dir_as_cwd` uses a fake codex that records `pwd`; RED observed `/home/kyle/Development/alps/alps-core`, GREEN observes the unique Ralph workspace. This restores Shell/Rust behavioral parity and makes relative PRD/progress/git access deterministic.

These fixes serve ALPS's primary engineering goal: **continually reduce ambiguous state, nondeterministic diagnostics, and cross-stage contract drift until failures are explicit, reproducible, and safely contained.** The `IncompleteStories` guard remains unchanged and load-bearing; it correctly contained this CWD regression.

*Smoke #21 verification (2026-08-09, GREEN):* post-fix Tier-4 smoke fired in herdr workspace `wER` ("alps-tier4-smoke-21"), pane `wER:p1`. ALPS PID 2576244 ran 1,114 s end-to-end (vs smoke #20's 5,744 s and smoke #18's 3,580 s baseline). Inner Rust Ralph shipped all 8 stories in a single iteration in 817 s with 10 commits and 57 artifacts; outer loop closed on a single pass with Judge `claude-opus-4` `[done] accepted`. Review: 15 findings, 0 critical, 11/11 assertions. **Real `receipts.json` written this time** (was previously inferred absent due to the dual-writer log race and the CWD regression). `task_id 2026-08-09T131447-60150f203cb747ce96ab10b8cc031aee`, `verdict pass`, `stories 8/8`, `review 11/11`. 0 SIGTERM markers, 0 panic events, no `pkill` artifacts in `dmesg`. Deliverable `/tmp/alps-tier4-notes-21-preserved/deliverable.tar.gz` (64 MB, 8,193 files) contains the full Tier-4 spec artifact set (15 required files: `backend_pytest_output.txt`, `backend_uvicorn_startup.log`, 5× Playwright screenshots, `curl_flow.txt`, `db_schema_dump.sql`, `users_in_db.txt`, `notes_in_db.txt`, `frontend_build_output.txt`, `frontend_dev_startup.log`, `filesystem_inventory.txt`, `tmp_listing.txt`). Separated logging verified: telemetry log is 947 B / 13 markers, in order, with zero cross-contamination against the 328 KB stderr file. **Structural parity with smoke #18 is now demonstrated end-to-end.** Rust Ralph port is ready to PR.

*New follow-up (2026-08-09):* smoke #21's structured-DoD stage logged `[judge:structured] detected project type: unknown / no project type detected, skipping DoD checks`. Same behavior was observed in smoke #18. `dod_runner.detect_project_type` walks `Implementation.deliverable_path` (the deliverable), but with the deliverable at `/tmp/alps-tier4-notes-21/` the detector returned `unknown` despite `backend/pyproject.toml` and `frontend/package.json` being present. The LLM Judge is the load-bearing verifier while this regression stays open. **Acceptance gate:** a Tier-4 smoke whose Judge runs both the structured DoD stage AND the LLM stage in a single iteration (or a one-line regression test that asserts `detect_project_type` finds `python` / `node` against a `deliverable_path` containing `pyproject.toml` / `package.json`). Until that gate passes, every ACCEPT relies entirely on the LLM Judge's source-file review.

*Resolution (2026-08-09, monorepo depth-1 walk):* `detect_project_type` only inspected the deliverable root, never recursed. PR #3 (commit `0d840c1`) fixed the *which-root* problem (ralph_dir → deliverable_path) but didn't address the *recurse-into-the-root* problem. Smoke #21's deliverable was a monorepo: `backend/pyproject.toml` (Python, depth 1) + `frontend/package.json` (Node, depth 1), nothing matching at the root. Detector returned `Unknown` → short-circuit → LLM Judge alone. Fix: detect now walks one level of immediate subdirs after exhausting root-level markers, skipping vendor/build dirs (`node_modules`, `.git`, `target`, `dist`, `build`, `.next`, `__pycache__`, `.venv`, `venv`, `.cache`, `.tox`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`). Subdirs are sorted for deterministic classification — `backend/` wins alphabetically over `frontend/`, so Tier-4's structured DoD fires `pytest -q` against `backend/`. Priority contract: root-level markers win over nested (matters for ALPS running against itself — alps source has `frontend/` subprojects with `package.json` but should still be classified as Rust). 5 new tests pin the contract: `detect_python_project_in_nested_subdir_monorepo`, `detect_node_project_in_nested_subdir_monorepo`, `detect_does_not_walk_into_heavy_subdirs`, `detect_tier4_fullstack_monorepo_layout` (exact mirror of smoke #21), `detect_root_marker_wins_over_nested_marker`. Full workspace green: 144 lib + 8 integration + 23 alps-binary = **175 tests passing** (was 169 before this PR; +6 net). Acceptance gate met via unit regression tests; no need to re-smoke #22. The LLM Judge is no longer the sole verifier for monorepo deliverables — Tier-4's structured DoD now runs `pytest -q` against `backend/` automatically.

*Smoke #22 (2026-08-09, in-flight at 10:07 CDT)* — operator-side quota burn mid-smoke exposed three missing rate-limit / cost-awareness features in ALPS. Smoke launched in `wER:p1` with the rebuilt binary (initial run was against the pre-`d92ad99` binary and the Judge correctly caught a port mismatch — rebuild + restart). 12 stories (Plan agent expanded scope after seeing smoke #21's reject feedback), 5/12 passing at mid-flight (US-001 through US-005 done, US-006 frontend scaffold in progress). Inner-Ralph Rust path still clean.

*New follow-ups (rate-limit / quota awareness, P1 + P2 + P3 — open as of 2026-08-09):*

1. **P1 — ~~Surface tool exit code + stderr on every Ralph iteration.~~** — **SHIPPED 2026-08-10** (`d60fa33`). All three tool branches (codex / claude / amp) now capture `ExitStatus` and emit `[ralph] <tool> exited with code N on iteration I/M (stderr mirror at <ralph_dir>/.ralph-stderr.log)` via `elog!`. The stderr mirror to `.ralph-stderr.log` was already working — now the exit code is visible too. Acceptance test (`quota_exceeded_stderr_lands_in_ralph_stderr_log_and_loop_continues`) pins both contracts: a fake codex that emits `429 rate_limit_error: quota exceeded` on stderr + exits 1 lands the marker in `.ralph-stderr.log` AND the loop continues past the non-zero exit (preserves bash's `|| true` semantics). **Verified live in Tier-1 smoke (2026-08-10):** `[ralph] codex exited 0 on iteration 1/20` + `[ralph] codex exited 0 on iteration 2/20` appeared in the orchestrator's stderr. **Verified live in Tier-4 smoke iter 1:** same line format. **Test count 176 → 177.** P1 closed.

2. **P2 — `--budget <duration>` CLI flag for the outer loop.** Surface: alps fires `claude` / `codex` / `amp` calls and the operator has no way to say "abort after N minutes, don't care how many stories pass." When the operator hits a quota wall mid-smoke, the orchestrator happily retries within `max_iterations` until it hits `IncompleteStories` and exits non-zero — by which point several more LLM calls have been billed. **Fix:** new `--budget <hms>` CLI flag on `alps run`; wraps the `loop_.rs::drive` call in `tokio::time::Instant::now() + budget` and on each iteration checks `Instant::now() >= deadline`, returning `AlpsError::BudgetExceeded` if crossed. **Acceptance gate:** a unit test that drives `drive()` with `Instant::now() + 100ms` budget and a scripted Judge that takes 200ms per call; assert `drive()` returns `Err(BudgetExceeded)` after the budget elapses and writes no `receipts.json` (matches the `AlpsError::Implement(IncompleteStories)` pattern — clean Err exit, no orphan artifacts). P2 because the next quota wall is also the moment the operator most wants the abort.

3. **P3 — Tool cooldown / fallback on persistent 429.** Current behavior: when codex 429s three iterations in a row, Ralph just iterates 4, 5, 6... burning more quota for the same outcome. **Fix:** a per-tool cooldown state in `RalphConfig`: if a tool returned 429 in the last N iterations (configurable, default 3), switch to the next tool declared in `RalphConfig::tool_fallback_chain` (e.g. `codex → claude → amp`). Also surface a clear `[ralph] switching tool: codex → claude after 3 consecutive 429s` line in the telemetry log so the operator can see it happen. **Acceptance gate:** a unit test that drives `ralph::run` with a fake codex that 429s 4 times in a row and a fake claude that succeeds; assert the deliverable ships via claude with the fallback line in the log. P3 because it's the most invasive change (touches the tool-dispatch hot path), but it's the only one that *actively prevents* quota burn rather than just detecting it.

*Acceptance contract for all three:* none of them weaken the `IncompleteStories` guard, the tool-CWD fix from `74fe9f9`, the wrapper stream separation, or the structured-DoD monorepo walk. All three are additive — strictly more visibility / more safety, no regressions to existing load-bearing behavior.

4. **P4 — Judge venv-aware test invocation (monorepo Python + `.venv`).** Surface: with the cwd fix from PR #18 (`cc800f9` + `6c1fadf`) in place, the Judge's structured-DoD stage correctly runs `python3 -m pytest -q` from `<test_root>/backend/` (where the marker file `pyproject.toml` lives). But `Command::new("python3")` resolves via `$PATH`, which returns the system `/usr/bin/python3` — and the system Python has no `sqlalchemy`. The venv at `<test_root>/backend/.venv/bin/python` has all the deps installed (`sqlalchemy` v2.0.51 confirmed). Result: `ImportError: No module named 'sqlalchemy'` from `tests/conftest.py:18` → exit Some(4) → Judge REJECT → repeat-loop burns quota. **Verified live on 2026-08-10 in smoke #25** (Tier-4 post-PR-#18-merge stability check): log shows `judge:structured] detected project type: python (test_root: /tmp/alps-tier4-notes-25/backend)` ✓ (cwd fix working) but `[judge:structured] FAIL (exit Some(4))` because the venv wasn't activated. codex's runtime pytest (called via `uv run pytest -q` or with `.venv/bin/python` activated) reports 13 passed — so the deliverable is actually fine, the Judge's verifier just can't see it. **Surfaced by:** smoke #22-tier4 (2026-08-10, originally misdiagnosed as a cwd bug), confirmed by smoke #23 + smoke #24-baseline + smoke #25 (the cwd bug and the venv bug are independent). **Why this wasn't caught earlier:** Tier-1 / Tier-2 / Tier-3 don't hit the Python+monorepo+venv triple — Tier-1 is single-dir Python (system pytest on `$PATH`), Tier-2 is Go (`go test .`), Tier-3 is Node (`npm test`). The bug is specific to the (Python + monorepo + .venv) combo. Tier-4 is the first smoke tier that exercises it consistently.

**Fix (Option C — preferred):** change `fn test_command_for(project_type: &ProjectType) -> (&'static str, Vec<&'static str>)` in `alps-core/src/judge.rs:1009` to take a third parameter `test_root: &Path` and resolve Python specially:

```rust
fn test_command_for(project_type: &ProjectType, test_root: &Path) -> (&'static str, Vec<&'static str>) {
    match project_type {
        ProjectType::Rust => ("cargo", vec!["test", "--quiet"]),
        ProjectType::Python => python_test_cmd(test_root),
        ProjectType::Node => ("npm", vec!["test", "--silent"]),
        ProjectType::Go => ("go", vec!["test", "./..."]),
        ProjectType::Unknown => ("", vec![]),
    }
}

fn python_test_cmd(test_root: &Path) -> (&'static str, Vec<&'static str>) {
    // Prefer the project's local venv if it exists. Operator already
    // set it up via `uv venv` / `python -m venv .venv` / `uv sync`, so
    // we should use it.
    for venv_subdir in [".venv", "venv"] {
        let venv_python = test_root.join(venv_subdir).join("bin/python");
        if venv_python.exists() {
            return (BOX_LEAK, vec!["-m", "pytest", "-q"]);  // leaks a `PathBuf` lifetime-safe str
        }
    }
    // No venv → fall back to system `python3 -m pytest -q` (current behavior,
    // works for global-install projects like Tier-1 single-dir).
    ("python3", vec!["-m", "pytest", "-q"])
}
```

The caller line in `DoDRunner::check` (currently `let (cmd, args) = test_command_for(&project_type);` at judge.rs:835) becomes `let (cmd, args) = test_command_for(&project_type, &test_root);`. Update the existing 4 `test_command_for_each_type` assertions to pass `&test_root` and assert the new behavior (Python with `.venv` returns the venv path; Python without returns `python3`). Add 2 new unit tests: `test_command_for_python_prefers_venv_when_present` and `test_command_for_python_falls_back_to_python3_when_no_venv`.

**Why Option C over alternative options:**

| Option | Mechanism | Tradeoff |
|---|---|---|
| **A — `uv run pytest -q`** | Replace `python3 -m pytest` with `uv run pytest` | Requires `uv` on `$PATH`; fails on legacy Python projects without `pyproject.toml` (e.g., a `setup.py` repo with `pip install --user`). Adds a new tool dependency for the Judge. |
| **B — Activate `.venv` before `run_cmd_with_timeout`** | `source .venv/bin/activate && python3 -m pytest -q` via a shell wrapper | Shell activation doesn't propagate through `Command::new` — each subprocess needs its own activation, but activation is per-subshell. Adds bash indirection. |
| **C — Use `<test_root>/.venv/bin/python` directly** ✅ | Resolve the venv interpreter path explicitly | No new tool dep; matches what the operator actually set up (their `.venv/`); clean fall-through for projects without a venv. |

Option C is local (no new tooling), has zero new failure modes, and the fix is a single-file change to `judge.rs`.

**Acceptance gates (test additions):**

1. `test_command_for_python_uses_venv_python_when_venv_exists` — synthetic dir with `<dir>/.venv/bin/python` stub (just an existing file, no exec needed); assert `test_command_for(&ProjectType::Python, &dir)` returns the venv path.
2. `test_command_for_python_falls_back_to_python3_when_no_venv` — synthetic dir with no `.venv/`; assert it returns `python3`.
3. `dod_runner_python_with_venv_uses_venv_interpreter` (CI-safe, skip if no system python3) — full end-to-end runner test against a synthetic Python monorepo with a venv; assert the Judge's log shows the venv path AND pytest actually ran from the venv (proven by exit-0 with a trivial `assert True` test inside `backend/tests/`).
4. The existing `dod_runner_runs_pytest_from_monorepo_subdir_not_root` (already skipped on systems without pytest) still passes — but now without the `ModuleNotFoundError: sqlalchemy` failure on hosts that DO have pytest (because the venv is used).

**P4 priority:** higher than P3 for our current work because Tier-4 smokes are unusable end-to-end without it. Lower than P1/P2 because we don't currently hit quota walls (today was the first quota-aware session; we're pre-empting, not patching an active bleed). Recommend: P4 lands before the next Tier-4 smoke that wants a clean `[done] accepted` verdict without codex needing to manually re-invoke pytest for the Judge.

**P4 implementation (lands in same PR as this SPEC entry):** change `fn test_command_for(project_type: &ProjectType) -> (&'static str, Vec<&'static str>)` in `alps-core/src/judge.rs:1009` to:

```rust
fn test_command_for(
    project_type: &ProjectType,
    test_root: &Path,
) -> (Cow<'static, str>, Vec<Cow<'static, str>>) {
    match project_type {
        ProjectType::Rust => (Cow::Borrowed("cargo"), vec![Cow::Borrowed("test"), Cow::Borrowed("--quiet")]),
        ProjectType::Python => python_test_cmd(test_root),
        ProjectType::Node => (Cow::Borrowed("npm"), vec![Cow::Borrowed("test"), Cow::Borrowed("--silent")]),
        ProjectType::Go => (Cow::Borrowed("go"), vec![Cow::Borrowed("test"), Cow::Borrowed("./...")]),
        ProjectType::Unknown => (Cow::Borrowed(""), vec![]),
    }
}

fn python_test_cmd(test_root: &Path) -> (Cow<'static, str>, Vec<Cow<'static, str>>) {
    for venv_subdir in [".venv", "venv"] {
        let venv_python = test_root.join(venv_subdir).join("bin/python");
        if venv_python.exists() {
            return (Cow::Owned(venv_python.to_string_lossy().into_owned()),
                    vec![Cow::Borrowed("-m"), Cow::Borrowed("pytest"), Cow::Borrowed("-q")]);
        }
    }
    (Cow::Borrowed("python3"), vec![Cow::Borrowed("-m"), Cow::Borrowed("pytest"), Cow::Borrowed("-q")])
}
```

`Cow<'static, str>` replaces `&'static str` so the venv path (allocated per-call) doesn't need to be leaked via `Box::leak`. Caller line becomes `let (cmd, args) = test_command_for(&project_type, &test_root);`; then `let args_str: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();` to deref the Cows for `run_cmd_with_timeout` (`cmd: &str, args: &[&str]`).

**Tests (in same PR):**
- Update `test_command_for_each_type` to pass `&test_root` to the function (8 assertions total).
- New `test_command_for_python_uses_venv_python_when_venv_exists` — synthetic `<dir>/.venv/bin/python` stub; asserts the venv path is returned.
- New `test_command_for_python_prefers_dotvenv_over_venv` — creates both `.venv/bin/python` and `venv/bin/python`, asserts `.venv` wins (priority order: `.venv` is `uv` default, `venv` is stdlib `python -m venv` default).
- New `test_command_for_python_falls_back_to_python3_when_no_venv` — no venv dirs, asserts fall-through.

Test count: 180 → 183 (no skip patterns needed; these are pure data-tests, no pytest dependency).



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
