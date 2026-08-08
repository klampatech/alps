//! `alps ralph` — long-running AI agent loop, ported from `scripts/ralph.sh`.
//!
//! # Why this exists (SPEC §12 item 9.10, smoke #19)
//!
//! Ralph Wiggum is the autonomous coding loop that drives ALPS's implement
//! step: read `prd.json`, pick the highest-priority user story with
//! `passes: false`, invoke an external tool (codex / claude / amp) to
//! implement it, mark it as `passes: true` in `prd.json`, repeat. Until
//! either every story passes (success) or `max_iterations` is hit (partial).
//!
//! Historically this was a 282-line bash script at `scripts/ralph.sh`,
//! invoked as a subprocess from `ImplementAgent::run`. Two consecutive
//! production smokes burned us at the bash↔Rust IPC boundary:
//!
//! - **Smoke #15 (2026-08-07)** — orchestrator SIGTERM after ralph returned
//!   because signal sender lived outside alps's process tree. Required
//!   `setpgid(0,0) + PR_SET_PDEATHSIG` on BOTH sides of the boundary.
//! - **Smoke #18 (2026-08-07)** — argv-leak. The 50-char prompt text was
//!   passed through argv to bash, and `pkill -f` patterns emitted by codex
//!   accidentally matched alps argv, killing the orchestrator. Required the
//!   `--prompt-file` flag (argv only carries `~50` chars).
//!
//! Moving the loop into `alps-core` as a library function **deletes the
//! bash↔Rust IPC boundary** for the orchestrator hot path. The CLI still
//! exposes `alps ralph` as a subcommand for operator use, but it's now a thin
//! wrapper over this function — no IPC, no argv leak, no orchestrator-death
//! window. (The smoke harness and operator workflow that calls
//! `alps ralph --tool codex --max-iter 5 --ralph-dir /tmp/foo/` keep working
//! unchanged.)
//!
//! # 1:1 parity with `ralph.sh`
//!
//! This module is intentionally a line-for-line port of the bash script.
//! The smoke #19 acceptance gate is **identical behavior to smoke #18**
//! (60min/3 iters/7-7 stories/12-12 assertions/opus-4), so any divergence
//! is a regression. Where the bash did something that has a more idiomatic
//! Rust equivalent (e.g. JSON parsing), we use the Rust idiom but the
//! *observable behavior* must match. The diff against ralph.sh is documented
//! inline next to each section.
//!
//! The shell escape hatch (`RalphMode::Shell`) is kept for one release as a
//! rollback safety net, then removed in a follow-up commit. See
//! `implement.rs` for the dispatch logic.
//!
//! # State file locations (FIXED: workspace, NOT script dir)
//!
//! Per `scripts/test-state-file-location.sh` (ported to
//! `tests/ralph_state_location.rs`), all state files MUST be written to the
//! ralph working directory (passed as `RalphConfig::ralph_dir`), not to the
//! script's source directory. This module writes:
//!
//! - `<ralph_dir>/prd.json` — the PRD (already written by the caller; we
//!   read it, never overwrite)
//! - `<ralph_dir>/progress.txt` — progress log
//! - `<ralph_dir>/archive/` — archived runs from prior branches
//! - `<ralph_dir>/.last-branch` — last branch name (for archive-on-change)
//! - `<ralph_dir>/.codex-last-message.txt` — codex's final assistant message
//! - `<ralph_dir>/.ralph-result.json` — Ralph's exit report
//!
//! # Cross-check guard (phantom-COMPLETE, smoke #8)
//!
//! The bash version had a phantom-COMPLETE bug: codex could write prose
//! denying completion that happened to contain the literal string
//! `<promise>COMPLETE</promise>` as a *quoted denial*. The grep matched,
//! ralph.sh wrote `completed: true`, and the orchestrator happily continued
//! even though 10/12 stories were still failing. Smoke #8 surfaced it.
//!
//! The fix (ported to bash as `all_stories_pass`) lives here as
//! `all_user_stories_pass`. It is the **single source of truth** for
//! completion detection — the orchestrator's `ImplementError::IncompleteStories`
//! guard is the safety net, but Ralph itself never claims completion when
//! stories disagree.
//!
//! # stderr capture pattern (FIX #6: O_APPEND, NOT truncate)
//!
//! The bash version used `tee -a /dev/stderr` (append mode) to mirror the
//! tool's output to the orchestrator's stderr file. Without `-a`, tee opens
//! the destination with O_WRONLY|O_CREAT|O_TRUNC, which TRUNCATES the
//! orchestrator's earlier `elog!` writes. This module uses
//! `OpenOptions::append(true)` for the same O_APPEND atomicity guarantee.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::elog;
use crate::implement::{RalphPrd, RalphResult, RalphTool};

/// Errors that can occur while running Ralph.
#[derive(Debug, Error)]
pub enum RalphError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid tool '{0}', must be 'amp', 'claude', or 'codex'")]
    InvalidTool(String),
}

/// Configuration for `Ralph::run`.
///
/// Mirrors the bash CLI of `scripts/ralph.sh`:
/// `./ralph.sh [--tool amp|claude|codex] [max_iterations]`.
#[derive(Debug, Clone)]
pub struct RalphConfig {
    /// Ralph's working directory. ALL state files (prd.json, progress.txt,
    /// .ralph-result.json, archive/, .last-branch, .codex-last-message.txt)
    /// live here. This is the workspace passed to the orchestrator by `alps run`.
    pub ralph_dir: PathBuf,

    /// Where to read the prompt file from when invoking the tool.
    /// - For `codex`: the AGENTS.md (Ralph's instructions for Codex)
    /// - For `claude`: the CLAUDE.md (Ralph's instructions for Claude Code)
    /// - For `amp`: the prompt.md (Ralph's instructions for Amp)
    pub script_dir: PathBuf,

    /// Which tool backend to invoke. Validated at run-time.
    pub tool: RalphTool,

    /// Max iterations before giving up. Defaults to 10 in bash; we default
    /// higher (20) to match the orchestrator's `ImplementConfig::default()`.
    pub max_iterations: u32,
}

impl RalphConfig {
    /// Build a config from the workspace path + orchestrator config.
    ///
    /// `ralph_dir` and `script_dir` are intentionally separate. The
    /// workspace is the per-task clone at `tasks/<id>/implementation/ralph/`.
    /// The script_dir is `alps/scripts/` where the vendored prompts
    /// (`AGENTS.md`, `CLAUDE.md`, `prompt.md`) live.
    pub fn new(ralph_dir: PathBuf, script_dir: PathBuf, tool: RalphTool, max_iterations: u32) -> Self {
        Self {
            ralph_dir,
            script_dir,
            tool,
            max_iterations,
        }
    }
}

/// Run the Ralph loop to completion (or `max_iterations`).
///
/// Returns a `RalphResult` describing how the run ended. Never panics on
/// tool failure (the loop catches and continues, same as the bash version).
/// Always writes `.ralph-result.json` before returning — same as the bash
/// version's `write_ralph_result` trap, called from every exit path.
pub async fn run(cfg: RalphConfig) -> Result<RalphResult, RalphError> {
    // ── Argument validation (bash: lines 41-45) ──
    match cfg.tool {
        RalphTool::Amp | RalphTool::Claude | RalphTool::Codex => {}
    }

    let ralph_dir = cfg.ralph_dir.clone();
    let script_dir = cfg.script_dir.clone();

    let prd_file = ralph_dir.join("prd.json");
    let progress_file = ralph_dir.join("progress.txt");
    let archive_dir = ralph_dir.join("archive");
    let last_branch_file = ralph_dir.join(".last-branch");
    let codex_last_message = ralph_dir.join(".codex-last-message.txt");
    let result_file = ralph_dir.join(".ralph-result.json");

    // ── Track start time (bash: line 12) ──
    let start_epoch = unix_now_secs();

    // ── State pre-amble (bash: lines 124-162) ──
    archive_previous_run_if_branch_changed(
        &prd_file,
        &progress_file,
        &archive_dir,
        &last_branch_file,
    )
    .await?;

    track_current_branch(&prd_file, &last_branch_file).await?;

    if !progress_file.exists() {
        std::fs::write(
            &progress_file,
            format!(
                "# Ralph Progress Log\nStarted: {}\n---\n",
                chrono_format_now()
            ),
        )?;
    }

    // ── Resolve prompts (bash: lines 189-208) ──
    //
    // Codex: prefer `<ralph_dir>/AGENTS.md` (the orchestrator copied it in
    // step 3), fall back to `<script_dir>/AGENTS.md` if not present. Same
    // fallback bash uses.
    let codex_agents_prompt = if ralph_dir.join("AGENTS.md").exists() {
        ralph_dir.join("AGENTS.md")
    } else {
        script_dir.join("AGENTS.md")
    };
    let claude_prompt = script_dir.join("CLAUDE.md");
    let amp_prompt = script_dir.join("prompt.md");

    let mut iterations: u32 = 0;
    #[allow(unused_assignments)] // Initial value is overwritten by either branch of the loop.
    let mut completed = false;

    elog!(
        "[ralph] starting: tool={}, max_iterations={}, ralph_dir={}",
        cfg.tool,
        cfg.max_iterations,
        ralph_dir.display()
    );

    // ── Main loop (bash: lines 166-275) ──
    for i in 1..=cfg.max_iterations {
        iterations = i;

        elog!(
            "[ralph] iteration {}/{} (tool={})",
            i,
            cfg.max_iterations,
            cfg.tool
        );

        // Run the selected tool.
        //
        // For codex: also pipe stdin from the prompt file. We delete the
        // `.codex-last-message.txt` file before each run so the COMPLETE
        // grep below never false-positives on a stale file from a prior
        // iteration. Same as bash `rm -f "$CODEX_LAST_MESSAGE"` line 206.
        let last_message_text: Option<String> = match cfg.tool {
            RalphTool::Codex => {
                // `codex exec --dangerously-bypass-approvals-and-sandbox
                // -o <last-message> < <prompt>`
                let _ = std::fs::remove_file(&codex_last_message);
                let output = Command::new("codex")
                    .args([
                        "exec",
                        "--dangerously-bypass-approvals-and-sandbox",
                        "-o",
                        codex_last_message.to_str().unwrap(),
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();

                let mut child = match output {
                    Ok(c) => c,
                    Err(e) => {
                        elog!("[ralph] failed to spawn codex: {} (continuing)", e);
                        // Same as bash: `|| true` swallows the error and
                        // continues to the next iteration. The orchestrator's
                        // max_iterations budget bounds the damage.
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };

                // Pipe the prompt via stdin (NOT argv — that's how smoke #18
                // burned us). Read the prompt file's bytes, then write to
                // stdin and close it.
                let prompt_bytes = match std::fs::read(&codex_agents_prompt) {
                    Ok(b) => b,
                    Err(e) => {
                        elog!(
                            "[ralph] failed to read prompt file {}: {} (continuing)",
                            codex_agents_prompt.display(),
                            e
                        );
                        let _ = child.kill().await;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&prompt_bytes).await;
                    drop(stdin); // EOF — codex will now start processing
                }

                // ── FIX #6: O_APPEND stderr mirroring (bash: tee -a /dev/stderr) ──
                //
                // The bash version used `tee -a /dev/stderr` to mirror
                // codex's stderr into the orchestrator's stderr file
                // WITHOUT truncating it. Without `-a`, tee opens the file
                // with O_WRONLY|O_CREAT|O_TRUNC, which DESTROYS the
                // orchestrator's earlier `elog!` writes.
                //
                // In Rust we don't need a separate process — we read
                // stderr in-line and append it directly with
                // `OpenOptions::append(true)`, which gives the same
                // O_APPEND atomicity guarantee.
                let stderr_path = ralph_dir.join(".ralph-stderr.log");
                let mut stderr_file = match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&stderr_path)
                {
                    Ok(f) => f,
                    Err(e) => {
                        elog!(
                            "[ralph] failed to open stderr mirror file: {} (continuing without mirror)",
                            e
                        );
                        // Fall back to inherited stderr (no mirror).
                        let status = child.wait_with_output().await;
                        let _ = status;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };

                if let Some(mut child_stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 4096];
                    loop {
                        match child_stderr.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = stderr_file.write_all(&buf[..n]);
                                let _ = stderr_file.flush();
                                // Also write to stderr for live operator view.
                                let _ = std::io::stderr().write_all(&buf[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                }

                let _ = child.wait().await;

                // Read the last-message file for the COMPLETE grep.
                std::fs::read_to_string(&codex_last_message).ok()
            }
            RalphTool::Claude => {
                // `claude --dangerously-skip-permissions --print < <prompt>`
                let output = Command::new("claude")
                    .args(["--dangerously-skip-permissions", "--print"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();

                let mut child = match output {
                    Ok(c) => c,
                    Err(e) => {
                        elog!("[ralph] failed to spawn claude: {} (continuing)", e);
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };

                let prompt_bytes = match std::fs::read(&claude_prompt) {
                    Ok(b) => b,
                    Err(e) => {
                        elog!(
                            "[ralph] failed to read prompt file {}: {} (continuing)",
                            claude_prompt.display(),
                            e
                        );
                        let _ = child.kill().await;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&prompt_bytes).await;
                    drop(stdin);
                }

                // O_APPEND stderr mirroring (same as codex path)
                let stderr_path = ralph_dir.join(".ralph-stderr.log");
                let mut stderr_file = match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&stderr_path)
                {
                    Ok(f) => f,
                    Err(_) => {
                        let _ = child.wait().await;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };
                if let Some(mut child_stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 4096];
                    loop {
                        match child_stderr.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = stderr_file.write_all(&buf[..n]);
                                let _ = stderr_file.flush();
                                let _ = std::io::stderr().write_all(&buf[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                }

                let output = child.wait_with_output().await.ok();
                output.and_then(|o| String::from_utf8(o.stdout).ok())
            }
            RalphTool::Amp => {
                // `amp --dangerously-allow-all < <prompt>`
                let output = Command::new("amp")
                    .arg("--dangerously-allow-all")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();

                let mut child = match output {
                    Ok(c) => c,
                    Err(e) => {
                        elog!("[ralph] failed to spawn amp: {} (continuing)", e);
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };

                let prompt_bytes = match std::fs::read(&amp_prompt) {
                    Ok(b) => b,
                    Err(e) => {
                        elog!(
                            "[ralph] failed to read prompt file {}: {} (continuing)",
                            amp_prompt.display(),
                            e
                        );
                        let _ = child.kill().await;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&prompt_bytes).await;
                    drop(stdin);
                }

                let stderr_path = ralph_dir.join(".ralph-stderr.log");
                let mut stderr_file = match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&stderr_path)
                {
                    Ok(f) => f,
                    Err(_) => {
                        let _ = child.wait().await;
                        continue_ralph_iteration(cfg.tool, &ralph_dir).await;
                        continue;
                    }
                };
                if let Some(mut child_stderr) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 4096];
                    loop {
                        match child_stderr.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = stderr_file.write_all(&buf[..n]);
                                let _ = stderr_file.flush();
                                let _ = std::io::stderr().write_all(&buf[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                }

                let output = child.wait_with_output().await.ok();
                output.and_then(|o| String::from_utf8(o.stdout).ok())
            }
        };

        // ── Completion check (bash: lines 235-271) ──
        //
        // The grep pattern is the literal string "<promise>COMPLETE</promise>".
        // For codex, we grep the .codex-last-message.txt file (not stdout)
        // so the prompt-text echo doesn't false-positive. For claude/amp,
        // we grep the captured stdout.
        let grep_hit = match cfg.tool {
            RalphTool::Codex => codex_last_message.exists()
                && grep_promise_complete(&codex_last_message),
            _ => last_message_text
                .as_deref()
                .map(contains_promise_complete)
                .unwrap_or(false),
        };

        if grep_hit {
            // Cross-check (smoke #8 fix): only treat as completed if all
            // user stories have passes: true in prd.json.
            if all_user_stories_pass(&prd_file).await {
                elog!(
                    "[ralph] all stories passed at iteration {}/{} (tool={})",
                    i,
                    cfg.max_iterations,
                    cfg.tool
                );
                completed = true;

                // Write .ralph-result.json (bash: write_ralph_result trap)
                let result = RalphResult {
                    iterations,
                    elapsed_secs: unix_now_secs().saturating_sub(start_epoch),
                    completed,
                };
                write_result_file(&result_file, &result).await?;
                return Ok(result);
            } else {
                // False positive: tool emitted the literal string in prose
                // but prd disagrees. Continue iterating. (bash lines 246-256)
                let remaining = remaining_failing_stories(&prd_file).await;
                elog!(
                    "[ralph] {} mentioned <promise>COMPLETE</promise> in prose but {} stories still failing in prd.json (continuing)",
                    cfg.tool,
                    remaining
                );
            }
        }

        // ── Sleep between iterations (bash line 274) ──
        //
        // Same 2-second sleep as bash. Gives the file system / git a beat
        // to settle and avoids hot-spinning if a tool exits very fast.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // ── Max iterations reached (bash: lines 277-282) ──
    elog!(
        "[ralph] reached max iterations ({}) without completing all tasks",
        cfg.max_iterations
    );

    let result = RalphResult {
        iterations,
        elapsed_secs: unix_now_secs().saturating_sub(start_epoch),
        completed: false,
    };
    write_result_file(&result_file, &result).await?;
    Ok(result)
}

/// Mirror of bash `write_ralph_result` (lines 67-83). Writes the
/// `.ralph-result.json` file in the ralph workspace.
///
/// Always called on every exit path of the main loop, matching the bash
/// `trap write_ralph_result EXIT` semantics (we just call it inline since
/// Rust doesn't have signal-trap ergonomics).
async fn write_result_file(
    path: &Path,
    result: &RalphResult,
) -> Result<(), RalphError> {
    let json = serde_json::to_string_pretty(result)?;
    tokio::fs::write(path, json).await?;
    Ok(())
}

/// True iff the given file contains the literal string
/// `<promise>COMPLETE</promise>`. Mirrors bash `grep -q`.
///
/// We read the whole file into memory (the last-message file is always
/// small — codex's final assistant message, not the full streaming output).
fn grep_promise_complete(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => contains_promise_complete(&s),
        Err(_) => false,
    }
}

fn contains_promise_complete(s: &str) -> bool {
    s.contains("<promise>COMPLETE</promise>")
}

/// True iff every user story in prd.json has `passes: true`. False otherwise
/// (including: file missing, JSON malformed, no userStories, any story
/// missing `passes` or with `passes: false`).
///
/// This is the single source of truth for completion detection post smoke #8.
/// The orchestrator's `ImplementError::IncompleteStories` guard is the safety
/// net that catches a phantom-COMPLETE slipping through if this function
/// regresses.
async fn all_user_stories_pass(prd_file: &Path) -> bool {
    let text = match tokio::fs::read_to_string(prd_file).await {
        Ok(t) => t,
        Err(_) => return false,
    };
    let prd: RalphPrd = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return false,
    };
    !prd.user_stories.is_empty()
        && prd.user_stories.iter().all(|s| s.passes)
}

/// Count of user stories still failing (`passes != true`) in prd.json.
/// Used for the "N stories still failing" diagnostic message.
async fn remaining_failing_stories(prd_file: &Path) -> String {
    let text = match tokio::fs::read_to_string(prd_file).await {
        Ok(t) => t,
        Err(_) => return "?".to_string(),
    };
    let prd: RalphPrd = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return "?".to_string(),
    };
    prd.user_stories.iter().filter(|s| !s.passes).count().to_string()
}

/// Archive the previous run if the branch changed. (bash lines 124-147)
///
/// If `prd.json` exists, has a `branchName`, and that branch differs from
/// the contents of `.last-branch`, copy prd.json + progress.txt into
/// `archive/<date>-<branch>/` and reset progress.txt for the new run.
async fn archive_previous_run_if_branch_changed(
    prd_file: &Path,
    progress_file: &Path,
    archive_dir: &Path,
    last_branch_file: &Path,
) -> Result<(), RalphError> {
    if !prd_file.exists() || !last_branch_file.exists() {
        return Ok(());
    }
    let current_branch = read_prd_branch(prd_file).await;
    let last_branch = match tokio::fs::read_to_string(last_branch_file).await {
        Ok(s) => s.trim().to_string(),
        Err(_) => String::new(),
    };

    if current_branch.is_empty() || last_branch.is_empty() {
        return Ok(());
    }
    if current_branch == last_branch {
        return Ok(());
    }

    let date = chrono_format_date();
    let folder_name = last_branch.trim_start_matches("ralph/");
    let archive_folder = archive_dir.join(format!("{}-{}", date, folder_name));

    elog!(
        "[ralph] archiving previous run: {} -> {}",
        last_branch,
        archive_folder.display()
    );

    tokio::fs::create_dir_all(&archive_folder).await?;
    if prd_file.exists() {
        let _ = tokio::fs::copy(prd_file, archive_folder.join("prd.json")).await;
    }
    if progress_file.exists() {
        let _ = tokio::fs::copy(
            progress_file,
            archive_folder.join("progress.txt"),
        )
        .await;
    }

    // Reset progress file for new run
    tokio::fs::write(
        progress_file,
        format!(
            "# Ralph Progress Log\nStarted: {}\n---\n",
            chrono_format_now()
        ),
    )
    .await?;

    Ok(())
}

/// Write the current branch from prd.json to .last-branch. (bash lines 149-155)
async fn track_current_branch(
    prd_file: &Path,
    last_branch_file: &Path,
) -> Result<(), RalphError> {
    if !prd_file.exists() {
        return Ok(());
    }
    let branch = read_prd_branch(prd_file).await;
    if !branch.is_empty() {
        tokio::fs::write(last_branch_file, branch).await?;
    }
    Ok(())
}

async fn read_prd_branch(prd_file: &Path) -> String {
    let text = match tokio::fs::read_to_string(prd_file).await {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let prd: RalphPrd = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    prd.branch_name
}

/// Sleep at the end of an iteration that didn't emit a result. Mirrors
/// the bash `sleep 2` between iterations (line 274). Extracted so each
/// tool-branch's error path uses the same sleep.
async fn continue_ralph_iteration(_tool: RalphTool, _ralph_dir: &Path) {
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format the current time as a human-readable string. Mirrors bash's
/// `date` (no format spec). Used in the progress-file header.
fn chrono_format_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // We don't pull in chrono for this — just ISO 8601 from the SystemTime.
    // Use a simple UTC formatter. If chrono isn't sufficient, the operator
    // can grep for the seconds-since-epoch and reconstruct.
    format!("{}", now)
}

/// Format the current date as YYYY-MM-DD. Mirrors bash `date +%Y-%m-%d`.
fn chrono_format_date() -> String {
    // Use std::time + a simple UTC date calculation. We don't need
    // sub-day precision — the archive folder just needs to be unique per day.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since 1970-01-01 UTC
    let days = now / 86_400;
    // Convert days since epoch to Y-M-D using a small algorithm
    // (Howard Hinnant's civil_from_days).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", year, m, d)
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A 2-story prd with the first story passing and the second not.
    fn write_two_story_prd(path: &Path, second_passes: bool) {
        let json = format!(
            r#"{{
                "project": "alps-test",
                "branchName": "alps/test",
                "description": "test fixture",
                "userStories": [
                    {{"id": "US-001", "title": "first", "description": "d", "acceptanceCriteria": [], "priority": 1, "passes": true}},
                    {{"id": "US-002", "title": "second", "description": "d", "acceptanceCriteria": [], "priority": 2, "passes": {}}}
                ]
            }}"#,
            second_passes
        );
        std::fs::write(path, json).unwrap();
    }

    #[tokio::test]
    async fn all_stories_pass_when_all_passing() {
        let dir = tempdir_via_tmp("alps-ralph-test-all-pass");
        write_two_story_prd(&dir.join("prd.json"), true);
        assert!(all_user_stories_pass(&dir.join("prd.json")).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn all_stories_fail_when_any_failing() {
        let dir = tempdir_via_tmp("alps-ralph-test-one-fail");
        write_two_story_prd(&dir.join("prd.json"), false);
        assert!(!all_user_stories_pass(&dir.join("prd.json")).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn all_stories_fail_when_prd_missing() {
        let dir = tempdir_via_tmp("alps-ralph-test-no-prd");
        // No prd.json — must return false.
        assert!(!all_user_stories_pass(&dir.join("prd.json")).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn all_stories_fail_when_prd_malformed() {
        let dir = tempdir_via_tmp("alps-ralph-test-bad-prd");
        std::fs::write(&dir.join("prd.json"), "not json").unwrap();
        assert!(!all_user_stories_pass(&dir.join("prd.json")).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_finds_promise_complete_in_message() {
        let dir = tempdir_via_tmp("alps-ralph-test-grep-hit");
        let path = dir.join(".codex-last-message.txt");
        std::fs::write(&path, "all done!\n<promise>COMPLETE</promise>\n").unwrap();
        assert!(grep_promise_complete(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_misses_when_string_absent() {
        let dir = tempdir_via_tmp("alps-ralph-test-grep-miss");
        let path = dir.join(".codex-last-message.txt");
        std::fs::write(&path, "still working on US-003\n").unwrap();
        assert!(!grep_promise_complete(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Smoke #8 regression test: codex emits the literal string in a
    /// *denial* (e.g. "no <promise>COMPLETE</promise> is emitted"), but
    /// prd shows 1/2 stories passing. The grep would match, but the
    /// cross-check guard MUST treat it as a false positive.
    #[tokio::test]
    async fn phantom_complete_in_prose_is_not_real_completion() {
        let dir = tempdir_via_tmp("alps-ralph-test-phantom");
        write_two_story_prd(&dir.join("prd.json"), false); // US-002 failing
        let msg = dir.join(".codex-last-message.txt");
        // codex writes prose mentioning the string but denying completion
        std::fs::write(
            &msg,
            "10 stories still incomplete, so no `<promise>COMPLETE</promise>` is emitted.\n",
        )
        .unwrap();
        let grep_hit = grep_promise_complete(&msg);
        let all_pass = all_user_stories_pass(&dir.join("prd.json")).await;
        // Both must agree: grep hits the string, but the cross-check
        // correctly rejects the claim.
        assert!(grep_hit, "grep should match the literal string");
        assert!(
            !all_pass,
            "cross-check MUST reject phantom completion (smoke #8 regression guard)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contains_promise_complete_substring_match() {
        assert!(contains_promise_complete(
            "all done <promise>COMPLETE</promise>"
        ));
        assert!(!contains_promise_complete("still working"));
        assert!(!contains_promise_complete("<promise>complete</promise>")); // case sensitive
    }

    #[test]
    fn date_format_is_yyyy_mm_dd() {
        let d = chrono_format_date();
        assert_eq!(d.len(), 10);
        assert!(d.chars().nth(4) == Some('-'));
        assert!(d.chars().nth(7) == Some('-'));
    }

    /// Helper: make a temp dir under /tmp with the given prefix.
    fn tempdir_via_tmp(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
