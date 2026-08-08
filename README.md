# ALPS — Agentic Loop Programming System

<p align="center">
  <img src="docs/alps-logo.png" alt="ALPS — Agentic Loop Programming System" width="720">
</p>

<p align="center">
  <strong>A four-step orchestrator that drives a high-stakes prompt from idea to verified, tested, shipped work.</strong>
</p>

<p align="center">
  <code>Plan → Implement → Review → Judge</code> &nbsp;·&nbsp; adversarial review &nbsp;·&nbsp; structured receipts &nbsp;·&nbsp; failure-driven replanning &nbsp;·&nbsp; type-safe by construction
</p>

<p align="center">
  <a href="#status">Status</a> &nbsp;·&nbsp;
  <a href="#architecture">Architecture</a> &nbsp;·&nbsp;
  <a href="#install">Install</a> &nbsp;·&nbsp;
  <a href="#usage">Usage</a> &nbsp;·&nbsp;
  <a href="#how-it-works">How it works</a> &nbsp;·&nbsp;
  <a href="#layouts--artifacts">Layout</a> &nbsp;·&nbsp;
  <a href="#development">Development</a> &nbsp;·&nbsp;
  <a href="#license">License</a>
</p>

---

## What is ALPS?

ALPS turns a single natural-language prompt into a verified, tested, committed implementation — and it does this by composing four specialized agents into a closed loop:

| Step | Agent | What it does |
| --- | --- | --- |
| **1. Plan** | Claude Code | Breaks the prompt into atomic, verifiable stories with explicit DoD criteria. |
| **2. Implement** | Ralph + Codex | Runs the inner implementation loop: pick a story → write code → test → commit → repeat until `COMPLETE`. |
| **3. Review** | Claude Code (adversarial) | Reads the diff and looks for ways the implementation could be wrong — file:line evidence required. |
| **4. Judge** | Hermes (hybrid) | Runs the verifiable DoD (cargo test / pytest / npm test / go test) **and** an LLM verdict. Both must clear for PASS. |

If the Judge **rejects**, the loop restarts at Plan with the feedback appended to the prompt. If it **passes**, ALPS writes a markdown summary and JSON receipt, auto-commits to a per-task branch, and exits.

The key invariant: **the type system encodes the state machine.** Invalid transitions (`Plan → Judge` skipping `Implement`) are compile errors. You can't ship a malformed orchestrator.

```
                    ┌──────────────────────────┐
                    │  Kyle (human)            │
                    │  prompt ──▶  verify ◀──   │
                    └────────────┬─────────────┘
                                 ▼
                    ┌──────────────────────────┐
                    │  ALPS Outer Loop         │  ◀── recursive, type-state safe
                    └────────────┬─────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              ▼                  ▼                  ▼
        ┌──────────┐      ┌─────────────┐      ┌──────────┐
        │  Plan    │ ──▶  │ Implement   │ ──▶  │  Review  │
        │  Claude  │      │ Ralph+Codex │      │  Claude  │
        └──────────┘      └─────────────┘      └────┬─────┘
              ▲                                       │
              │ feedback                              ▼
              └──────────────────────────────────── │ Judge │
                                                  │ Hermes│
                                                  └────┬───┘
                                                       │ PASS
                                                       ▼
                                                  ┌────────┐
                                                  │  Done  │
                                                  │ receipts│
                                                  └────────┘
```

---

## Status

**v0.7.3, 2026-08-07** — Iteration-2 orchestrator-death bug fixed + smoke wrapper dedup. **Two consecutive Tier-4 Judge ACCEPT verdicts** (smokes #17 + #18, full-stack notes app with FastAPI + Postgres + JWT + Vite + React + TS + Zustand, 8/8 and 7/7 stories respectively, 12/12 review assertions, opus-4 judge, opus-4 judge via MiniMax-M3 wiring). §12 items 9 + 9.5 + 9.6 + 9.7 + 9.8 + 10 closed. 156 tests passing (138 unit + 14 integration + 4 new bash tests for prompt substitution).

- ✅ **Plan** → Claude Code (`--dangerously-bypass-approvals-and-sandbox -p`) with atomic stories + verifiable DoD
- ✅ **Implement** → `ralph.sh --tool codex` subprocess, idempotent dir setup, per-task branch, real iteration metrics
- ✅ **Review** → Adversarial Claude with strict JSON output, file:line evidence required
- ✅ **Judge** → Hybrid `DoDRunner` (cargo/pytest/npm/go test, 120s timeout) + `HermesLlmJudge` (Claude Code, default model `claude-opus-4` via the Opus alias → MiniMax-M3; was `claude-sonnet-4` prior to 2026-07-30 swap — see SPEC §11.1)
- ✅ **Reject path** → Restarts at Plan with feedback appended (no feedback-loop loss)
- ✅ **Per-task branches** → `alps/<task-id>` off `main`, every artifact committed
- ✅ **AGENTS.md propagation** → Ralph-learned patterns feed back into Review / Judge / next-Plan
- ✅ **Workdir completion guard** → Blocks auto-reinvoke within 5s of success (defensive against Claude TUI re-runs); `--force` bypasses
- ✅ **Recursive artifact collection** → `read_artifacts` walks `ralph_dir` recursively so Rust `src/lib.rs`, Go `pkg/*.go`, and other non-root source reach the Judge
- ✅ **Parse-failure retry** → All three LLM agents (Plan / Review / Judge) retry up to 3× on JSON parse failure
- ✅ **`--prompt-file <PATH>` argv cleanup** → Reads the prompt from a temp file instead of argv. Defeats codex's `pkill -f <keyword>` patterns matching alps's `/proc/<pid>/cmdline` (smoke #13 + #14 root-cause; smoke #17 + #18 verify-stable). See [§ argv cleanup with `--prompt-file`](#argv-cleanup-with---prompt-file).
- ✅ **Orchestrator death hardening** → `setpgid(0,0)` + `prctl(PR_SET_PDEATHSIG, SIGTERM)` makes alps immune to herdr-pane-babysitter SIGTERMs and ties its life to its parent shell.
- ✅ **`elog!` macro end-to-end** → Flushed, O_APPEND-guarded, single-source for orchestrator stderr writes. Reaches FD-2 from every agent path (verified via 156 `[alps-diag]` lines in smoke #18's stderr).
- ✅ **SIGTERM/SIGINT/SIGHUP handlers** → On signal, write a marker + backtrace to `ALPS_SIGTERM_LOG` before exiting, so the wrapper can diagnose post-mortem without racing the orchestrator.

**Verified end-to-end** (see [`SPEC.md`](SPEC.md) §0 + §12 for the full smoke log):

- 7 successful happy-path smokes (`# ALPS — Done` on the first attempt)
- 1 Rust DoD smoke (`cargo test --quiet` exit 0, 4/4 stories, 8/8 review assertions)
- 1 multi-iteration ralph smoke (5 ralph iterations, 4/7 stories → Judge rejected correctly → restart with feedback)
- 1 real reject-path smoke (CRUD FastAPI app, 4 outer iterations, 3 rejects catching distinct real defects, 4th accepted; SPEC §12 item 1 closed)
- 6 Tier-4 full-stack smokes (#13 through #18, FastAPI + Postgres + JWT + Vite + React + TS + Zustand). Smokes #17 + #18 = Judge ACCEPT (the structural fix is stable as of 2026-08-07). Smokes #13-#16 root-caused + verified the iteration-2 orchestrator-death bug.
- Workdir guard re-verified on every smoke

**156/156 tests passing** (23 alps-cli unit + 129 alps-core unit + 4 alps-core integration + 8 bash tests in `tests/test_prompt_substitution.sh`).

---

## Architecture

### The four agents

Each agent is an `impl Agent` in its own module — `plan.rs`, `implement.rs`, `review.rs`, `judge.rs`. The `Agent` trait is **sealed**: only `alps-core` types implement it. This guarantees the orchestrator can never call an external implementer that doesn't ship with the type-state guarantees.

The trait takes an opaque `Input` and returns a typed `Output` that the next state expects. No `Option<T>` smoothing — the type system makes sure a `Task<Implemented>` literally cannot transition to `Task<Judged>` without going through `Review`.

### The type-state machine

```rust
// alps-core/src/task.rs (excerpt)
pub struct Task<State> { /* private */ state: State, prompt: Prompt }

impl Task<Idle>     { pub fn plan(self, plan: Plan)           -> Task<Planned>      { ... } }
impl Task<Planned>  { pub fn implement(self, imp: Implementation) -> Task<Implemented> { ... } }
impl Task<Implemented> { pub fn review(self, rev: Review)    -> Task<Reviewed>      { ... } }
impl Task<Reviewed> { pub fn judge(self, j: Judgment)        -> Task<Done> | Task<Rejected> { ... } }
impl Task<Rejected> { pub fn reset(self)                     -> Task<Idle>          { ... } }
```

That's the whole orchestrator. `loop_::drive` is a recursive function, not a `loop { }` — because inside a real loop, `task = task.method(...)` doesn't shadow and the state stays stale. Recursion gets you fresh bindings every iteration.

### Compose boundary with Ralph

ALPS owns the **outer** loop. Ralph owns the **inner** implement loop. ALPS treats Ralph as a black-box subprocess — it does **not** reimplement Ralph's loop logic:

1. ALPS writes `prd.json` (1:1 mapping from `Plan.stories` to Ralph's `userStories` format) + `progress.txt` (with the `## Codebase Patterns` header) into `tasks/<id>/implementation/ralph/`.
2. ALPS spawns `ralph.sh --tool codex <max_iterations>` with stdin/stdout inherited. Ralph runs its own loop.
3. Ralph exits when `<promise>COMPLETE</promise>` appears in `.codex-last-message.txt` (the Codex-specific completion extraction).
4. ALPS reads back `prd.json` (stories now have `passes: true`), `progress.txt`, and `git log` → typed `Implementation`.

If Ralph exits with code 1 (e.g. it hit the 20-iteration safety net with partial progress), ALPS no longer dies — `ImplementAgent::run` reads `prd.json` regardless of exit code, and the partial progress flows into Judge, which rejects, which restarts the loop with feedback.

### Hybrid Judge (Structured + LLM)

The Judge runs in two stages, both must clear for PASS:

1. **Structured DoD** (`DoDRunner`) — auto-detects project type from manifest files:
   - `Cargo.toml` → `ProjectType::Rust` → `cargo test --quiet`
   - `pyproject.toml` / `pytest.ini` → `ProjectType::Python` → `pytest -q`
   - `package.json` → `ProjectType::Npm` → `npm test --silent`
   - `go.mod` → `ProjectType::Go` → `go test ./...`
   120s timeout, exit code + stderr captured.
2. **LLM Judge** (`HermesLlmJudge`) — focused verdict prompt against the file tree + diff + review findings.

If the structured runner FAILS, the rejection reason is the canned string `"verifiable DoD criteria failed"` and the `evidence` field carries the test exit code + first 1000 chars of stderr. If structured PASSES and LLM REJECTs, the reason is whatever Hermes wrote — typically a long natural-language complaint about missing artifacts. **That distinction is your diagnostic.** See [`SPEC.md`](SPEC.md) §"Runtime Pitfall #14".

---

## Install

### Prerequisites

- **Rust** (stable, edition 2021+) — install via [rustup](https://rustup.rs/)
- **Claude Code CLI** — for Plan + Review agents
- **Codex CLI** — for Implement agent (Ralph's default tool)
- **Git** ≥ 2.42 — for per-task branches and nested-repo handling
- **herdr** *(optional, recommended for smoke testing)* — [agent-aware terminal multiplexer](https://github.com/lampak/herdr) for structured output capture

### Build from source

```bash
git clone https://github.com/klampatech/alps.git
cd alps
cargo build --workspace --release

# Add to PATH (or symlink into ~/.local/bin/)
export PATH="$PWD/target/release:$PATH"
alps --version
```

> ⚠️ **Important:** `cargo test --workspace --no-run` does **NOT** produce the `alps` CLI binary. If you use `cargo test` as a "ready" signal and then try to run the CLI, you'll hit `alps: command not found` ~90s into a smoke. Always `cargo build --workspace` before smoke runs that depend on the binary.

### Run a task

```bash
# Single-task, single-prompt run. The deliverable must land INSIDE the workdir
# (or the recursive artifact walker won't see it and Hermes will reject).
alps run "Create a Python file fib.py with a function fib(n) that returns the first n Fibonacci numbers as a list. Also create test_fib.py with a pytest test."

# Specify a workdir (defaults to cwd)
alps run "..." --workdir /path/to/workdir

# Bypass the workdir completion guard (for legitimate immediate retries)
alps run "..." --force
```

CLI flags:

| Flag | Purpose |
| --- | --- |
| `--workdir <path>` | Where tasks land. Default: `.` |
| `--force` | Bypass the workdir completion guard |
| `--deliverable-path <path>` | Where the deliverable actually lives. Default: `--workdir`. See [§ Deliverable outside the workdir](#deliverable-outside-the-workdir). |
| `--prompt-file <path>` | Read the prompt from this file instead of argv. **Preferred for smoke harnesses** — keeps the prompt text out of alps's `/proc/<pid>/cmdline` so `pkill -f <keyword>` patterns emitted by codex (e.g. `pkill -f vite`, `pkill -f uvicorn`) can't accidentally kill alps. See [§ argv cleanup with `--prompt-file`](#argv-cleanup-with---prompt-file). |
| `--telemetry-log <path>` | Write `elog!` lines to this file with `O_APPEND` semantics. Default: off. The smoke wrapper uses the same path as its `2>` redirect so orchestrator + codex stderr streams coexist without clobbering each other. |

---

## Usage

### The 30-second tour

```bash
$ alps run "Add a /healthz endpoint to my FastAPI app that returns {status: ok}"

[plan] running (Claude)
  ↳ 1 story: US-001: add GET /healthz returning JSON
[plan] complete in 32s → tasks/2026-07-27T132311-.../plan.json

[implement] running (Ralph + Codex, max_iterations=20, stories=1)
  ↳ ralph iteration 1: US-001 → tests pass → commit
  ↳ <promise>COMPLETE</promise>
[implement] complete in 88s → tasks/.../implementation/ralph/prd.json (1/1 passes)

[review] running (Claude, adversarial)
  ↳ 4 assertions, 0 critical findings, 1 minor style note
[review] complete in 167s → tasks/.../review.json

[judge:structured] detected project type: python
[judge:structured] pytest -q → exit 0, 3 passed
[judge:structured] PASS
[judge:llm]     PASS (verdict aligned with structured)
[judge] complete in 4s → tasks/.../feedback.json

[done] accepted
# ALPS — Done
- Task: 2026-07-27T132311-...
- Branch: alps/2026-07-27T132311-...
- Stories: 1/1 passed
- Review: 4/4 assertions, 0 critical
- Verdict: PASS
- Receipts: tasks/2026-07-27T132311-.../receipts.json
```

### Anatomy of a run

Every run creates a per-task directory and a per-task branch:

```
tasks/
└── 2026-07-27T132311-63cc87d845654cc39e55da8d8b42bc32/   # the task workspace
    ├── prompt.md                                       # original prompt (verbatim)
    ├── plan.json                                       # Plan agent output
    ├── AGENTS.md                                       # accumulated codebase patterns
    ├── review.json                                     # Review agent findings + assertions
    ├── feedback.json                                   # Judge verdict (or rejection reason)
    ├── receipts.json                                   # final assemble (verdict: pass | reject)
    └── implementation/
        └── ralph/                                      # Ralph's nested git workspace
            ├── prd.json                                # user stories with passes flags
            ├── progress.txt                            # ralph's running notes + ## Codebase Patterns
            └── .codex-last-message.txt                 # codex completion signal
```

The per-task branch `alps/<task-id>` is created off `main` and contains the same `tasks/<id>/` artifacts (gitignored on `main`, tracked on the branch). You can review exactly what ALPS did for one run independently:

```bash
git fetch origin
git checkout alps/2026-07-27T132311-63cc87d845654cc39e55da8d8b42bc32
ls tasks/2026-07-27T132311-63cc87d845654cc39e55da8d8b42bc32/
```

### Reading the receipts

`receipts.json` is the canonical truth — what got done, who did it, what tests ran:

```json
{
  "task_id": "2026-07-27T132311-63cc87d845654cc39e55da8d8b42bc32",
  "verdict": "pass",
  "plan": { "stories": [{"id": "US-001", "title": "...", "dod": "..."}] },
  "implement": {
    "iterations": 2,
    "elapsed_secs": 88,
    "files_changed": ["app/main.py", "tests/test_main.py"],
    "ralph_commits": 3
  },
  "review": {
    "assertions": [{"id": "A1", "claim": "...", "evidence": "..."}],
    "findings": [{"severity": "minor", "file": "app/main.py", "line": 42}]
  },
  "judge": {
    "structured": { "project_type": "python", "command": "pytest -q", "exit": 0, "tests_passed": 3 },
    "llm": { "verdict": "pass", "reason": "..." }
  }
}
```

### Handling a rejection

If Judge rejects, the loop restarts at Plan with feedback appended. To inspect why a run rejected, look at `tasks/<id>/feedback.json`:

```bash
cat tasks/<id>/feedback.json | jq .reason
```

- `"reason": "verifiable DoD criteria failed"` → the structured runner (cargo / pytest / npm / go test) is the failure point. Check the `evidence` field for exit code + stderr.
- `"reason": "..."` (a long paragraph) → structured passed; the LLM Judge rejected for missing artifacts / context. Check that `read_artifacts` walked recursively and your source files are not in a `SKIP_DIRS` directory.

### Deliverable outside the workdir

If the prompt asks for the deliverable at a path outside `--workdir` (e.g., "build at `/tmp/foo/`"), pass `--deliverable-path /tmp/foo/` so the Judge walks that tree instead of the ralph nested workspace. Without this flag, the LLM Judge sees an empty source-files section and rejects with "Source files section is empty". Added in v0.7.

```bash
alps run "build a Python FastAPI app at /tmp/foo/" \
    --workdir /tmp/alps-smoke \
    --deliverable-path /tmp/foo/
```

### argv cleanup with `--prompt-file`

When alps runs codex (the Implement agent), codex periodically emits `pkill -f <keyword>` patterns to clean up stale processes between iterations. The `<keyword>` is usually a long-running-service name: `vite`, `uvicorn`, `npm`, `fastapi`, `react-dev-tools`, etc.

**The bug:** when the prompt was passed as the first positional argv, alps's own `/proc/<pid>/cmdline` contained those keywords literally. `pkill -f vite` would then match alps's cmdline and SIGTERM the orchestrator. Smoke #13 + #14 (2026-08-07) both died from this at the same `unix_ts=1786118743.378`.

**The fix:** pass the prompt via `--prompt-file <PATH>` instead of argv. The wrapper creates a temp file with `mktemp -t alps-prompt.NN.XXXXXX.txt`, passes that path as `--prompt-file`, and alps reads + (best-effort) deletes it on startup. With this flag, alps's `/proc/<pid>/cmdline` is ~50 chars (just the temp file path) — `pkill -f <anything>` cannot match.

```bash
# Smoke-harness pattern — REQUIRED for any prompt that mentions a long-running
# service (vite, uvicorn, npm, fastapi, react-dev-tools, etc.):
PROMPT_FILE=$(mktemp -t alps-prompt.NN.XXXXXX.txt)
cat > "$PROMPT_FILE" << 'EOF'
Build a full-stack notes app at /tmp/notes with FastAPI + Postgres + JWT + Vite ...
EOF
alps run --prompt-file "$PROMPT_FILE" --workdir /tmp/alps-smoke --deliverable-path /tmp/notes
rm -f "$PROMPT_FILE"  # alps deletes it too, but just in case
```

The smoke-harness reference wrapper (`/tmp/alps-tier4-smoke-wrapper.sh`) handles temp-file creation, log-prefix → PRESERVE_DIR derivation, strace attachment, journalctl + dmesg captures, and receipts preservation — see the [Tier-4 smoke recipe](#tier-4-full-stack-smoke-recipe) below.

### The workdir completion guard

ALPS refuses to re-invoke in the same workdir within 5 seconds of a prior success. This blocks the bug class where Claude TUI / shell auto-re-runs `alps run` after seeing `# ALPS — Done`.

```bash
# exit 0, accepted
alps run "..." --workdir /tmp/alps-smoke

# exit 2, blocked:
# error: recent completion in workdir — task <id> completed 0s ago (threshold 5s)
alps run "..." --workdir /tmp/alps-smoke

# warning: bypassing workdir guard, proceeds normally
alps run "..." --workdir /tmp/alps-smoke --force

# >5s after success: works normally
sleep 6 && alps run "..." --workdir /tmp/alps-smoke
```

---

## How it works

### End-to-end flow

1. **Bootstrap** — ALPS creates a per-task branch `alps/<task-id>` off `main` and a per-task directory `tasks/<id>/`. Writes `<workdir>/.git/info/exclude` with `tasks/*/implementation/ralph/` so ralph's nested `.git/` doesn't fatal `git add -A` on git ≥2.42.

2. **Plan** — `PlanAgent` invokes `claude --dangerously-skip-permissions -p --model claude-sonnet-4` with a JSON-output system prompt. Output is `Plan { stories: Vec<Story> }` where each Story has `id`, `title`, `dod: Vec<String>`, and `files: Vec<String>`. If the JSON fails to parse, retry up to 3×.

3. **Implement** — `ImplementAgent`:
   - Writes `prd.json` (mapping `Plan.stories` → Ralph's `userStories` format) into `tasks/<id>/implementation/ralph/`.
   - Spawns `scripts/ralph.sh --tool codex <max_iterations>` with the ralph dir as cwd.
   - Waits for `<promise>COMPLETE</promise>` in `.codex-last-message.txt` **or** max-iterations exit.
   - Reads back `prd.json` (regardless of exit code), `progress.txt`, `git log`.
   - Recursively walks `ralph_dir` for artifacts (with `SKIP_DIRS` for `target/`, `node_modules/`, `.git/`, `__pycache__/`, `.gradle/`, `.cargo/`, `dist/`, `build/`, `.pytest_cache/`, `.mypy_cache/`).
   - Returns `Implementation { stories, artifacts, commits, metrics }`.

4. **Review** — `ReviewAgent` invokes Claude Code adversarially. Output schema requires `assertions: [{ id, claim, evidence: { file, line, snippet } }]` and `findings: [{ severity: critical|major|minor, file, line, message }]`. If parse fails, retry up to 3×.

5. **Judge** — `JudgeAgent`:
   - **Structured** (`DoDRunner`): detects project type from manifest files, runs the appropriate test command with 120s timeout, captures exit + stderr.
   - **LLM** (`HermesLlmJudge`): if structured passed, calls Claude Code (`claude --dangerously-skip-permissions -p --model claude-opus-4`) with a focused verdict prompt over the file tree + diff + review findings. As of 2026-07-30 the Judge model is the Opus alias (→ MiniMax-M3 on this host) for the dedicated judgment slot; Plan + Review stay on Sonnet for cheaper sub-agent work. When the wiring was first set (2026-07-26 §11.1), the implementation chose Claude Code over a separate Hermes CLI (which doesn't exist as-shipped) and the `HermesLlmJudge` struct-name stuck for backwards compat. Receipts record `judge_model: "claude-opus-4"` (was `claude-sonnet-4` pre-swap).
   - Returns `Judgment::Pass` only if both clear. Otherwise `Judgment::Reject(Feedback { reason, evidence })`.

6. **Loop** — `loop_::drive` is a recursive function:
   ```rust
   pub async fn drive(prompt: Prompt, workdir: &Path) -> Result<Done, AlpsError> {
       let task = Task::<Idle>::new(prompt, workdir)?;
       let task = task.plan(PlanAgent::run(task.prompt).await?)?;
       let task = task.implement(ImplementAgent::run(&task.plan, workdir).await?)?;
       let task = task.review(ReviewAgent::run(&task.implementation).await?)?;
       match JudgeAgent::run(&task.review, &task.implementation).await? {
           Judgment::Pass => task.accept(receipts),
           Judgment::Reject(fb) => drive(task.with_feedback(fb).reset().prompt, workdir).await,
       }
   }
   ```

7. **Done** — On PASS, ALPS writes `receipts.json`, calls `commit_smart` (auto-commit on the per-task branch; silent if nothing changed), and prints `# ALPS — Done` markdown summary to stdout.

### AGENTS.md propagation

Ralph writes `## Codebase Patterns` to its `progress.txt` as it learns. The orchestrator extracts that section and appends it to `tasks/<id>/AGENTS.md`. Review, Judge, and Plan-on-retry see `AGENTS.md` content in their prompts.

Verified end-to-end: smoke 2026-07-27 produced 5 patterns that flowed into the Review's adversarial assessment, making the Review specific to the actual codebase rather than generic.

---

## Layouts & artifacts

### Repository layout

```
alps/                              # Cargo workspace
├── SPEC.md                        # Full design, type design, MVP decisions
├── README.md                      # You are here
├── docs/                          # Logos, HTML/Mermaid diagrams (open in browser)
│   ├── alps-logo.svg              # the brand
│   ├── diagram-happy-path.html
│   ├── diagram-rejection-restart.html
│   └── diagram-state-machine.html
├── alps-core/                     # Rust library (the actual orchestrator)
│   └── src/
│       ├── lib.rs                 # Re-exports
│       ├── task.rs                # Type-state Task<S> + state structs + transitions
│       ├── loop_.rs               # Outer loop driver — recursive, not loop{}
│       ├── plan.rs                # PlanAgent — real claude -p invocation
│       ├── implement.rs           # ImplementAgent — Ralph subprocess
│       ├── review.rs              # ReviewAgent — adversarial Claude with JSON schema
│       ├── judge.rs               # JudgeAgent — hybrid (DoDRunner + HermesLlmJudge)
│       ├── agents_md.rs           # Task-level AGENTS.md read/write/append + extract_patterns
│       ├── git_ops.rs             # commit_smart + ensure_ralph_excluded, create_branch
│       ├── receipt.rs             # Receipts, ImplementMetrics, ReviewSummary
│       ├── persistence.rs         # Per-state Persistable impls + TaskWorkspace helpers
│       ├── error.rs               # AlpsError taxonomy (thiserror)
│       ├── agent.rs               # Sealed Agent trait + EmptyInput
│       ├── workdir_guard.rs       # v0.4 sentinel debounce against auto-reinvoke
│       └── domain.rs              # Plan, Review, Implementation, Judgment, etc.
├── alps-cli/                      # Binary entry: `alps run "prompt"`
│   └── src/main.rs                # clap CLI, call into alps-core::loop_
├── scripts/                       # Vendored from snarktank/ralph
│   ├── ralph.sh                   # Ralph loop runner (must be executable)
│   └── CLAUDE.md                  # Ralph's Claude Code prompt
└── tasks/                         # Per-task workspaces, git-committed per state
```

### Per-task artifact map

| File | Owned by | Purpose |
| --- | --- | --- |
| `tasks/<id>/prompt.md` | ALPS bootstrap | Original prompt verbatim |
| `tasks/<id>/plan.json` | Plan agent | Granular stories + DoD |
| `tasks/<id>/AGENTS.md` | Orchestrator | Accumulated codebase patterns |
| `tasks/<id>/implementation/ralph/prd.json` | Ralph | Story completion flags |
| `tasks/<id>/implementation/ralph/progress.txt` | Ralph | Running notes + `## Codebase Patterns` |
| `tasks/<id>/review.json` | Review agent | Adversarial findings + assertions |
| `tasks/<id>/feedback.json` | Judge agent | Verdict (pass or reject reason) |
| `tasks/<id>/receipts.json` | ALPS done | Final assemble — the receipt |
| `tasks/<id>/implementation/ralph/.git/` | Ralph | Nested repo (excluded via `.git/info/exclude`) |

---

## Development

### Build & test

```bash
cargo build --workspace            # Build core + cli
cargo test --workspace --all-targets  # 156 tests passing as of v0.7.3 (23 alps-cli unit + 129 alps-core unit + 4 alps-core integration)
cargo run --bin alps -- --version
```

### Pre-flight for smoke runs

```bash
# 0. Pre-flight: build the binary and ensure PATH (mandatory)
cargo build --workspace
export PATH="$PWD/target/debug:$PATH"
which alps && alps --version   # fail loud if the binary isn't there
```

`cargo test --workspace --no-run` compiles test binaries under `target/debug/deps/` but does **NOT** produce `target/debug/alps`. Always `cargo build --workspace` before a smoke that depends on the binary.

### Smoke test recipe (Tier 1 — single-story fib task)

```bash
# 1. Fresh herdr workspace for the test
herdr workspace create --cwd /home/kyle/Development/alps --label "alps-smoke"
# capture pane_id from .result.root_pane.pane_id (e.g. "w9X:p1")

# 2. Write the prompt to a file. **Never inline multi-line prompts with
#    nested quotes in `herdr pane run`** — the escaping gets lost through
#    herdr's dispatch layer. And keep the deliverable INSIDE the workdir —
#    the recursive artifact walker only sees files under tasks/<id>/implementation/ralph/.
#    For Tier 1 this works because fib.py + test_fib.py are tiny and codex
#    won't emit pkill -f patterns. For anything bigger, ALWAYS use --prompt-file.
cat > /tmp/alps-smoke-prompt.txt << 'EOF'
Create a Python file fib.py with a function fib(n) that returns the
first n Fibonacci numbers as a list. fib(10) should be [0,1,1,2,3,5,8,13,21,34].
Also create test_fib.py with one pytest test that asserts fib(10) equals
that list. The test must pass when run with pytest.

Write everything inside the workdir (do NOT create files under /tmp/,
/home/, or any path outside the workdir).
EOF

# 3. Wrapper script (avoids nested-quote issues through herdr pane run).
cat > /tmp/alps-smoke-wrapper.sh << 'EOF'
#!/bin/bash
set -e
export PATH="/home/kyle/Development/alps/target/debug:$PATH"
cd /home/kyle/Development/alps
exec alps run "$(cat /tmp/alps-smoke-prompt.txt)" --workdir /tmp/alps-smoke
EOF
chmod +x /tmp/alps-smoke-wrapper.sh

# 4. Fire the wrapper via herdr. `2>&1 | tee` keeps the log for postmortem.
herdr pane run <pane_id> "clear; /tmp/alps-smoke-wrapper.sh 2>&1 | tee /tmp/alps-smoke.log"

# 5. Wait for completion (anchored regex — substring matching on "Done" is
#    too loose and matches incidental lines).
herdr wait output <pane_id> --match "^# ALPS — Done$" --timeout 600000
```

Expected timing (Codex backend, 2-story fib task): ~5 min wall clock total — Plan 30s, Implement 90s, Review 3 min, Judge 5s.

### Tier-4 full-stack smoke recipe

For prompts that mention long-running services (vite, uvicorn, npm, fastapi, etc.) — i.e. anything bigger than the Tier-1 fib — you MUST use `--prompt-file` so alps's `/proc/<pid>/cmdline` is immune to codex's `pkill -f <keyword>` patterns (see [§ argv cleanup](#argv-cleanup-with---prompt-file)). The reference smoke wrapper at `/tmp/alps-tier4-smoke-wrapper.sh` handles the full ceremony:

```bash
# Pre-flight (mandatory — see Pitfall #21 in ~/.hermes/skills/projects/alps/SKILL.md)
cargo build --workspace
export PATH="/home/kyle/Development/alps/target/debug:$PATH"

# Close any stale alps-labeled herdr workspaces from prior smokes
herdr workspace list | python3 -c "
import json, sys
d = json.load(sys.stdin)
for w in d['result']['workspaces']:
    if 'alps' in w.get('label','').lower() and w.get('agent_status') != 'working':
        print(w['workspace_id'])
" | xargs -r -n1 herdr workspace close

# Fresh herdr workspace
herdr workspace create --cwd /home/kyle/Development/alps --label "alps-tier4-smoke-NN-..."
WID=<result.workspace.workspace_id>   # e.g. "wEQ" (for cleanup later)
PID=<result.root_pane.pane_id>        # e.g. "wEQ:p1" (for `herdr pane run`)

# Fire via the generic wrapper. Five required flags:
#   --smoke-number     Smoke # (e.g. 18)
#   --workdir          Where tasks land (e.g. /tmp/alps-tier4-notes-18-workdir)
#   --deliverable-path Where the deliverable lives (e.g. /tmp/alps-tier4-notes-18)
#   --prompt-template  Path to the canonical prompt (with {{DELIVERABLE_PATH}} placeholders)
#   --log-prefix       Path prefix for stderr/strace/journalctl/dmesg outputs
herdr pane run $PID "/tmp/alps-tier4-smoke-wrapper.sh \
    --smoke-number 18 \
    --workdir /tmp/alps-tier4-notes-18-workdir \
    --deliverable-path /tmp/alps-tier4-notes-18 \
    --prompt-template /tmp/alps-tier4-notes-prompt.txt \
    --log-prefix /tmp/alps-tier4-18-stderr 2>&1 | tee /tmp/alps-tier4-18-wrapper-output.log"

# Monitor progress (poll the meta log)
tail -f /tmp/alps-tier4-18-stderr-meta.log

# On exit, the wrapper preserves receipts to ${LOG_PREFIX%-stderr}-preserved/
# (= /tmp/alps-tier4-18-preserved/), captures post-alps process tree + journalctl,
# and reports verdict + orchestrator marker counts.
```

Expected timing (Tier 4, 7-8 story full-stack app, opus-4 judge): ~45-60 min wall clock. Verify-stable = 2 consecutive ACCEPT verdicts with the same prompt template (smoke #17 + #18 in 2026-08-07 achieved this).

### Defensive smoke ritual

After the stdout markers, verify the workdir guard works:

1. **`exit 0` on the first run** (matched `# ALPS — Done`). Don't move on if not.
2. **Immediately re-invoke in same workdir WITHOUT `--force`** in the same pane. Expect exit code **2** and the stderr line `error: recent completion in workdir — task <id> completed 0s ago (threshold 5s)`. If you see exit 0, the guard is missing — **ship nothing**.
3. **With `--force`** — expect `warning: bypassing workdir guard …` and a fresh Plan attempt.
4. **Wait >5s and re-invoke** — guard should no longer fire.

This catches the wrapping-agent-auto-reinvocation bug class (Claude TUI / shell re-typing the command after seeing "Done").

### After cleanup

```bash
rm -rf /tmp/alps-smoke /tmp/alps-smoke-workdir
git worktree remove --force /tmp/alps-smoke-workdir  # if you used one
git branch -D <orphan-branch-name>  # only if it has no commits worth keeping
```

---

## Spec & design notes

- **[SPEC.md](SPEC.md)** — Full design, type design, MVP vs Phase 2/3, resolved decisions, smoke log
- **Diagrams**:
  - [Happy path](docs/diagram-happy-path.html) — outer loop runs once
  - [Rejection restart](docs/diagram-rejection-restart.html) — judge rejects, feedback loop
  - [State machine](docs/diagram-state-machine.html) — all states and transitions

### Design principles

1. **Start simple, scale later.** MVP is single-task, file-system state, type-state in core.
2. **Git is the main history.** Each task is a subdirectory of `tasks/`. Every artifact is committed.
3. **Strict typing.** State machine encoded in the type system. Invalid transitions are compile errors.
4. **Ralph is a subprocess, not a library.** ALPS owns the outer loop; Ralph owns the inner implement loop.
5. **Strict separation of concerns.** Plan / Implement / Review / Judge are independent agents.

### Resolved MVP decisions (2026-07-26)

- **Judge** — hybrid (`StructuredJudge` + `LlmJudge`), both must clear for PASS
- **Max attempts** — unbounded ("brute force development", must succeed eventually)
- **Notifications** — stdout only, no Discord / polling / file watch in MVP
- **Receipts** — markdown (stdout) + JSON (`tasks/<id>/receipts.json`)

For full rationale, see [`SPEC.md`](SPEC.md) §11.

### Known limitations / Open items

- **Tier-5 candidates (SPEC §9)** — full-stack with auth is now stable as of 2026-08-07 (smokes #17 + #18). Candidate Tier-5 directions: PDF/Excel deliverables (new DoD detector for binary structure), monorepo (Turborepo/Nx, multi-package coordination), infra (Terraform + Ansible, needs LocalStack or dry-run mode), mobile (React Native + Expo, adb/emulator toolchain). Pick one and we'll scope a smoke harness.
- **`for_test` mock coverage** — happy-path tests for `drive_passes_first_try` + `drive_returns_error_on_plan_failure` + `drive_returns_error_on_judge_failure` + `drive_returns_error_when_implement_completes_with_less_than_all_stories_passing` all landed via PR #6 (squashed `81737c1`). Plan/Implement/Review/Judge orchestration is unit-testable end-to-end without shelling out. LLM-driven agent tests still shell out (necessary evil — the prompt-schema parse path is the load-bearing contract).
- **Token-budget vs max-iterations detection** — `ImplementAgent::run` reads `prd.json` regardless of exit code, so Ralph exit code 1 (max iterations hit, partial progress) flows into Judge correctly. Token-budget exhaustion is theoretically distinguishable from "max iterations hit" by checking Codex's stderr for budget-exhausted markers, but no smoke has surfaced this yet.
- **Smoke wrapper location** — `/tmp/alps-tier4-smoke-wrapper.sh` is the load-bearing tool for future smokes. Consider promoting to `tests/scripts/` in-repo so it's version-controlled.

---

## Contributing

This project lives at [`klampatech/alps`](https://github.com/klampatech/alps). For significant changes:

1. Open an issue describing the agent/feature you want to add.
2. Branch from `main`.
3. Add deterministic tests where possible — `for_test` constructors + mock agent handlers keep the test suite fast (<100ms per orchestration test).
4. Run the smoke recipe above before opening a PR. The workdir guard defensive ritual is part of the review checklist.

Architectural notes for new agents: see the **Compose boundary with Ralph** and **Type-state machine** sections above. New agents must implement the sealed `Agent` trait and be added to `alps-core/src/lib.rs` re-exports.

---

## Acknowledgements

ALPS is built on the shoulders of:

- [**Ralph**](https://github.com/snarktank/ralph) — the inner implement loop, vendored at `scripts/ralph.sh`
- [**Claude Code**](https://claude.ai/code) — Plan + Review agents
- [**Codex CLI**](https://github.com/openai/codex) — Ralph's default execution backend
- [**Hermes Agent**](https://hermes-agent.nousresearch.com/) — the LLM Judge backbone (Hybrid judge stage 2)

---

## License

MIT — see [`Cargo.toml`](Cargo.toml) for the workspace metadata.