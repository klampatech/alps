//! ALPS CLI — `alps run "prompt"`.
//!
//! MVP behavior:
//!   1. Create `tasks/<task-id>/` workspace under the given workdir
//!   2. Initialize `Task<Idle>` with the prompt
//!   3. Drive the outer loop (Plan → Implement → Review → Judge)
//!   4. Persist + git commit at each state
//!   5. On Done, print markdown summary to stdout and write `receipts.json`
//!   6. Exit 0 on Done, 1 on AlpsError, 2 on Failed
//!
//! Resolves 2026-07-26:
//!   - Hybrid judge (structured DoD + LLM) — wired MVP stubs
//!   - Unbounded loop — no max attempts
//!   - stdout notifications — no Discord/polling
//!   - Markdown + JSON receipts — terminal summary + receipts.json

use std::path::PathBuf;
use std::sync::Arc;

use alps_core::domain::{Prompt, TaskId};
use alps_core::git_ops::{
    commit_smart_with_excludes, create_branch, CommitOutcome, GitOpsError,
};
use alps_core::implement::ImplementAgent;
use alps_core::judge::{DoDRunner, HermesLlmJudge, JudgeAgent};

mod detect;
use alps_core::loop_::drive;
use alps_core::persistence::TaskWorkspace;
use alps_core::plan::PlanAgent;
use alps_core::receipt::Receipts;
use alps_core::review::ReviewAgent;
use alps_core::task::{Done, Task};
use alps_core::elog;
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{error, info};

/// Install SIGTERM / SIGINT / SIGHUP handlers that write a marker + backtrace
/// to `$ALPS_SIGTERM_LOG` (default `/tmp/alps-sigterm.log`) BEFORE emulating
/// the default disposition. This is the diagnostic unlock for §12 item 9:
/// smoke #10 showed the orchestrator dies mid-`implement.run` with no panic
/// and no core dump — most likely external SIGTERM, but the source was not
/// visible. The handler captures (a) which signal arrived, (b) the exact
/// instant, (c) a backtrace of the current async task, so we can correlate
/// with `strace -f -e signal=all` to identify the sender PID.
///
/// We use `signal_hook::low_level` (raw `register`) rather than the high-
/// level iterator because we want to log on EACH signal, even if multiple
/// arrive before the runtime yields. Tokio's signal driver is NOT used —
/// it would defer handling until a future is `.await`ed, which is too late
/// if the orchestrator is mid-`process::exit` cleanup.
///
/// Set `ALPS_SIGTERM_LOG=<path>` to override the side-file path. If unset,
/// signals are still handled (default disposition: terminate) but no log
/// file is written.
#[cfg(unix)]
fn install_signal_handlers() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::low_level;

    let sig_log_path = std::env::var("ALPS_SIGTERM_LOG")
        .ok()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/alps-sigterm.log"));

    // Ensure parent dir exists (e.g. /tmp/alps-tier4-smoke-11/).
    if let Some(parent) = sig_log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Async-signal-safe stderr write. We avoid `libc::write` linkage
    // by inlining a `nix`-free wrapper — POSIX guarantees `write(2)` is
    // async-signal-safe.
    //
    // SAFETY: `write(2)` on FD 2 (stderr) is async-signal-safe per POSIX.1-2017
    // §2.4.3. The pointer arithmetic only dereferences bytes from the same
    // `&str` slice that was passed in, so it cannot escape the caller's
    // lifetime even if the caller goes out of scope mid-signal (the kernel
    // copies to its own buffer for queued signals; non-queued signals just
    // race and either succeed or fail).
    unsafe fn libc_write_stderr(s: &str) {
        extern "C" {
            fn write(fd: i32, buf: *const u8, count: usize) -> isize;
        }
        let fd = 2;
        let bytes = s.as_bytes();
        let mut written = 0;
        while written < bytes.len() {
            let n = write(fd, bytes.as_ptr().add(written), bytes.len() - written);
            if n <= 0 {
                break;
            }
            written += n as usize;
        }
    }

    // Write the marker for one signal arrival and re-raise with the default
    // disposition. Each signal gets its OWN closure (signal-hook 0.3 dropped
    // the `extern "C" fn(i32)` callback API in favor of `Fn()` closures that
    // don't receive the signal number — the trade-off is we register three
    // identical-but-tagged handlers instead of one dispatching handler).
    fn make_handler(sig_name: &'static str) -> impl Fn() + Send + Sync + 'static {
        move || {
            let payload = format!(
                "[alps-signal] received {} at unix_ts={}\n[alps-signal-backtrace]\n{:?}\n",
                sig_name,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                std::backtrace::Backtrace::force_capture(),
            );
            // Resolve target at signal time so a wrapper that sets the env
            // var AFTER process start still captures. Fall back to default.
            let target = std::env::var("ALPS_SIGTERM_LOG")
                .ok()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "/tmp/alps-sigterm.log".to_string());
            let _ = std::fs::write(&target, &payload);
            // SAFETY: see libc_write_stderr doc-comment.
            unsafe {
                libc_write_stderr(&payload);
            }
            // Re-raise with the default disposition so the process actually
            // terminates (and strace sees the canonical exit). Without this,
            // we'd eat the signal and the orchestrator would limp on.
            let sig_id = match sig_name {
                "SIGTERM" => SIGTERM,
                "SIGINT" => SIGINT,
                "SIGHUP" => SIGHUP,
                _ => SIGTERM,
            };
            low_level::emulate_default_handler(sig_id).ok();
        }
    }

    // Register one handler per signal we care about. A failure to register
    // is non-fatal — log to stderr and continue. The orchestrator will
    // still function, just without signal diagnostics.
    let handlers: [(i32, Box<dyn Fn() + Send + Sync>); 3] = [
        (SIGTERM, Box::new(make_handler("SIGTERM"))),
        (SIGINT, Box::new(make_handler("SIGINT"))),
        (SIGHUP, Box::new(make_handler("SIGHUP"))),
    ];
    for (sig, handler) in handlers {
        if let Err(e) = unsafe { low_level::register(sig, handler) } {
            eprintln!("[alps-diag] failed to register signal handler for {}: {}", sig, e);
        }
    }
    // Also write a one-time "handlers installed" line so the post-mortem
    // can confirm the diagnostic was active during the run (vs. the binary
    // being a pre-handler version).
    let install_marker = format!(
        "[alps-signal] handlers installed at unix_ts={} for SIGTERM/SIGINT/SIGHUP\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let _ = std::fs::write(&sig_log_path, install_marker);
}

#[cfg(not(unix))]
fn install_signal_handlers() {
    // No-op on non-Unix. The smoke harness is Linux-only anyway.
}

#[derive(Parser, Debug)]
#[command(name = "alps", version, about = "Agentic Loop Programming System")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a new task with the given prompt.
    Run {
        /// The prompt describing the work to do. May be omitted if
        /// --prompt-file is given (preferred for smoke harnesses — see
        /// §12 item 9.5 fix (ii) on alps-side argv cleanup).
        #[arg(conflicts_with = "prompt_file")]
        prompt: Option<String>,

        /// Read the prompt from this file instead of argv. The temp-file
        /// path appears in alps's /proc/<pid>/cmdline instead of the raw
        /// prompt text, so `pkill -f <keyword>` patterns emitted by codex
        /// (e.g. `pkill -f vite`) can't accidentally match alps argv.
        /// The wrapper creates the file with `mktemp -t alps-prompt.XXXXXX.txt`
        /// and alps will read+delete it on startup.
        #[arg(long, conflicts_with = "prompt", value_name = "PATH")]
        prompt_file: Option<String>,

        /// Working directory (defaults to current directory).
        #[arg(long, default_value = ".")]
        workdir: String,

        /// Bypass the workdir completion guard (allows re-invocation within
        /// the 5-second debounce window after a previous successful run).
        /// Use when you legitimately want to re-run immediately — e.g.,
        /// the previous run completed but you want to try a different prompt.
        #[arg(long)]
        force: bool,

        /// Where the deliverable actually lives. Used by `read_artifacts`
        /// and the Judge's `read_files` so the LLM review sees the
        /// actual deliverable code, not just ralph's nested workspace.
        ///
        /// Default: `--workdir`. Set this when the prompt says "build at
        /// `/tmp/foo/`" — point `--deliverable-path` at `/tmp/foo/` and
        /// alps will walk that tree for the Judge's source-files section.
        /// See SPEC §12 item 2 / Runtime Pitfall #16.
        #[arg(long, default_value = "")]
        deliverable_path: String,

        /// Optional path to a dedicated orchestrator-telemetry log file.
        ///
        /// When set, the orchestrator's `elog!` writes (e.g. `[plan] running`,
        /// `[implement] running`, `[done] accepted`) are also written to this
        /// file with `O_APPEND` semantics. This protects the orchestrator's
        /// stderr output from being overwritten by other writers that open the
        /// same file with `O_WRONLY` (notably `tee /dev/stderr` in ralph.sh,
        /// which would otherwise clobber the orchestrator's earlier lines
        /// starting at byte 0).
        ///
        /// Typical wrapper pattern: point both the wrapper's `2> file` redirect
        /// (catches codex's stderr via `tee /dev/stderr`) and `--telemetry-log`
        /// (catches the orchestrator's elog! writes) at the same path. Both
        /// streams land in the file, and the O_APPEND flag prevents
        /// cross-writer overwrites.
        ///
        /// If unset, telemetry goes only to stderr (FD 2), which works for
        /// TTY/pipe captures but loses data when other processes open the
        /// redirected file without O_APPEND.
        #[arg(long, default_value = "")]
        telemetry_log: String,
    },

    /// List tasks in the current workspace.
    List,

    /// Show receipts for a given task.
    Show {
        /// Task ID.
        task_id: String,
    },
}

/// Resolve the prompt text from CLI args. Three cases:
/// - `prompt` (argv) wins if both are given — clap's `conflicts_with`
///   blocks this at parse time, but the API still accepts both for tests.
/// - `--prompt-file <path>` reads the prompt from disk and (best-effort)
///   deletes the file after read. Failure to read returns an error.
/// - Neither → error.
///
/// Extracted from the inline match in `main()` so the logic is unit-testable.
fn resolve_prompt(prompt: Option<String>, prompt_file: Option<&str>) -> Result<String, String> {
    match (prompt, prompt_file) {
        (Some(p), _) => Ok(p),
        (None, Some(path)) => {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read --prompt-file {:?}: {}", path, e))?;
            let _ = std::fs::remove_file(path);
            Ok(contents)
        }
        (None, None) => Err("either `prompt` or --prompt-file is required".to_string()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // ── Process-group + parent-death hardening (SPEC §12 item 9.5 fix (iii)) ──
    // Smoke #15 root cause (2026-08-07): the orchestrator was SIGTERMed by
    // PID 1374425 — which is OUTSIDE alps's process tree (not a child of alps,
    // not visible in strace -f). The most likely sender is the herdr pane
    // babysitter: when a herdr pane sits idle for ~2 min, the babysitter
    // SIGTERMs the entire pgroup, taking out the wrapper bash AND alps with
    // it. The kill was overdue (codex finished 9/9 stories with
    // <promise>COMPLETE</promise> 2 min earlier; alps was sitting in a
    // futex waiting on the ralph.sh pipe).
    //
    // Two companion fixes:
    //   (a) setpgid(0, 0) — make alps the leader of its own process group.
    //       Then a pgroup-targeted SIGTERM (e.g. `kill -- -<pgid>`) hits
    //       only the pane group, not alps.
    //   (b) prctl(PR_SET_PDEATHSIG, SIGTERM) — when the parent process dies
    //       (e.g. the wrapper bash exits or is killed), the kernel
    //       automatically sends SIGTERM to alps. This is the SAFETY NET:
    //       if the wrapper dies for any reason, alps won't be orphaned
    //       eating resources / stuck in a futex.
    //
    // Both calls are no-ops in the smoke harness's view; the alps process
    // still terminates when expected. They just decouple alps from herdr
    // pgroup cleanup and ensure alps follows its parent.
    #[cfg(unix)]
    {
        // SAFETY: setpgid with (0,0) sets the current process as the leader
        // of its own process group. POSIX guarantees this can be done by
        // any process for itself.
        let pgid_result = unsafe { libc::setpgid(0, 0) };
        if pgid_result != 0 {
            eprintln!(
                "[alps-diag] setpgid(0,0) failed: errno={} (non-fatal; continuing)",
                std::io::Error::last_os_error()
            );
        } else {
            eprintln!(
                "[alps-diag] setpgid(0,0) ok; alps pgid={}",
                std::process::id()
            );
        }
        // SAFETY: PR_SET_PDEATHSIG / SIGTERM is the standard "die when
        // parent dies" pattern. The parent here is the wrapper bash — when
        // herdr SIGTERMs the wrapper, the kernel SIGTERMs alps too.
        let prctl_result = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
        if prctl_result != 0 {
            eprintln!(
                "[alps-diag] prctl(PR_SET_PDEATHSIG, SIGTERM) failed: errno={} (non-fatal)",
                std::io::Error::last_os_error()
            );
        } else {
            eprintln!("[alps-diag] prctl(PR_SET_PDEATHSIG, SIGTERM) ok");
        }
    }

    // Install SIGTERM/SIGINT/SIGHUP handlers BEFORE anything else (panic hook,
    // tracing, even args parsing). See §12 item 9 (2026-08-06): the orchestrator
    // dies mid-`implement.run` with no panic and no core dump — most likely an
    // external SIGTERM. The handlers write a marker + backtrace to
    // $ALPS_SIGTERM_LOG (default /tmp/alps-sigterm.log) and re-raise with the
    // default disposition so the process still terminates. Pair with
    // `strace -f -e signal=all -p <pid>` in the smoke wrapper to identify the
    // sender.
    install_signal_handlers();

    // Install a panic hook that writes panic info to a side file. The default
    // panic hook writes to stderr — fine for interactive use, but a smoke-test
    // wrapper's `2> file` redirect means the panic info is mixed with codex
    // output. A side file is also preserved even if the main log is truncated
    // or rotated. Set ALPS_PANIC_LOG=<path> in the wrapper to enable.
    //
    // This is the smoke #7 instrumentation (2026-08-06): the alps orchestrator
    // dies after ralph.sh returns (last elog! = "[implement] invoking Ralph",
    // .ralph-result.json written, but no "[implement] done" ever lands). With
    // the panic hook, any Rust panic in the post-ralph code path leaves a
    // backtrace + message in this file. If the file is empty, the orchestrator
    // was killed by a signal (not a panic).
    if let Ok(panic_log) = std::env::var("ALPS_PANIC_LOG") {
        let panic_log = std::path::PathBuf::from(panic_log);
        // Make sure parent dir exists
        if let Some(parent) = panic_log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let panic_log_for_hook = panic_log.clone();
        std::panic::set_hook(Box::new(move |panic_info| {
            // Write the panic message + backtrace to the side file. We can't
            // easily format the panic_info here, so write it raw and let the
            // operator inspect it with `strings` or similar.
            let bt = std::backtrace::Backtrace::force_capture();
            let payload = format!(
                "[alps-panic] {} at {}\n[alps-backtrace]\n{}\n",
                panic_info,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                bt
            );
            let _ = std::fs::write(&panic_log_for_hook, &payload);
            // Also try stderr as a fallback so operators not setting up
            // ALPS_PANIC_LOG still see something.
            eprintln!("[alps-panic] {}", panic_info);
        }));
    }

    tracing_subscriber::fmt::init();
    let args = Args::parse();

    match args.command {
        Command::Run { prompt, prompt_file, workdir, force, deliverable_path, telemetry_log } => {
            // Export the telemetry-log path as an env var so the `elog!` macro
            // (in alps-core/src/telemetry.rs) picks it up via `ALPS_TELEMETRY_LOG`.
            // The macro opens the file with O_APPEND in a OnceLock-cached handle,
            // so subsequent `elog!` calls in any module — including the loop
            // driver and child agent code — write to the same file with atomic
            // append semantics. This protects the orchestrator's stderr from
            // being clobbered by `tee /dev/stderr` (which opens the same file
            // with O_WRONLY without O_APPEND and would otherwise overwrite the
            // orchestrator's earlier writes from byte 0).
            if !telemetry_log.is_empty() {
                std::env::set_var("ALPS_TELEMETRY_LOG", &telemetry_log);
            }
            // Resolve the prompt: either read from --prompt-file (preferred for
            // smoke harnesses — §12 item 9.5 fix (ii)) or use argv directly.
            // When --prompt-file is provided, delete the file after reading
            // (best-effort, non-blocking on failure) so the prompt text isn't
            // left on disk indefinitely.
            let prompt = match resolve_prompt(prompt, prompt_file.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[alps-diag] {}", e);
                    std::process::exit(2);
                }
            };
            run_task(prompt, workdir, force, deliverable_path).await?;
        }
        Command::List => {
            elog!("alps list: not yet implemented");
            std::process::exit(1);
        }
        Command::Show { task_id } => {
            elog!("alps show {}: not yet implemented", task_id);
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn run_task(
    prompt: String,
    workdir: String,
    force: bool,
    deliverable_path: String,
) -> Result<()> {
    let task_id = TaskId::new();
    let workdir = PathBuf::from(workdir);

    // Resolve --deliverable-path. Default = workdir. If empty, we'll fall
    // through to workdir below so the Judge's read_files sees the workdir
    // tree (the same behavior as the legacy code). See SPEC §12 item 2.
    //
    // §12 item 1C (2026-08-03): if the operator passed an empty flag and
    // the prompt mentions a build path, auto-detect it. This closes the
    // common operator-forget case where `--deliverable-path` is missing
    // and Hermes rejects on "Source files section is empty" (Pitfall #16).
    let deliverable_path = if deliverable_path.trim().is_empty() {
        // Auto-detect: if the prompt mentions a build path, use it. Otherwise
        // the deliverable is the ralph nested git at
        // workdir/tasks/<task_id>/implementation/ralph/. Setting it to
        // workdir here would make the Judge's read_artifacts walk the
        // workdir root (which doesn't contain the code — the code lives in
        // tasks/<id>/implementation/ralph/). The legacy fallback inside
        // read_artifacts (deliverable_path empty → prd_path.parent() →
        // ralph_dir) is the right behavior here.
        match detect::detect(&prompt, &workdir) {
            Some(detected) => {
                elog!("[detect] auto-detected deliverable-path: {}", detected.display());
                detected
            }
            None => {
                let ralph_default = workdir.join("tasks").join(task_id.as_str()).join("implementation").join("ralph");
                elog!("[detect] no viable prompt-derived path; using ralph nested git: {}", ralph_default.display());
                ralph_default
            }
        }
    } else {
        PathBuf::from(&deliverable_path)
    };
    // Shadow for the second use (commit_smart_with_excludes). The original
    // binding is moved into ImplementConfig below.
    let deliverable_path_for_commit = deliverable_path.clone();

    // ── Workdir completion guard ──
    // Refuse to start if a previous run completed in this workdir within the
    // last 5 seconds. This is a defensive guard against the wrapping agent
    // (e.g. Claude TUI in a herdr pane) auto-re-invoking alps after seeing
    // "ALPS — Done" — observed 2026-07-27 in smoke6 (w9S:p1).
    // Use --force to bypass for legitimate immediate re-runs.
    use std::time::Duration;
    match alps_core::workdir_guard::check_recent_completion(
        &workdir,
        Duration::from_secs(alps_core::workdir_guard::DEFAULT_THRESHOLD_SECS),
    ) {
        Ok(()) => {}
        Err(alps_core::workdir_guard::WorkdirGuardError::RecentCompletion {
            task_id: prev,
            seconds_ago,
            threshold_secs,
        }) => {
            if !force {
                elog!(
                    "error: recent completion in workdir — task {} completed {}s ago (threshold {}s).",
                    prev, seconds_ago, threshold_secs
                );
                elog!(
                    "       wrapping agent (Claude TUI / shell) may have re-invoked alps."
                );
                elog!(
                    "       wait a few seconds, or pass --force to bypass this guard."
                );
                std::process::exit(2);
            }
            // --force: warn but proceed
            elog!(
                "warning: bypassing workdir guard (task {} completed {}s ago)",
                prev, seconds_ago
            );
        }
        Err(e) => {
            // Other errors (malformed sentinel, IO) — warn but proceed.
            // A malformed sentinel shouldn't block legitimate runs.
            elog!("warning: workdir guard check failed: {}", e);
        }
    }

    let workspace_root = workdir.join("tasks").join(task_id.as_str());
    let workspace = TaskWorkspace::new(&workspace_root);

    info!(target: "alps.cli", task_id = %task_id.as_str(), "starting task");

    // Surface the deliverable path so the operator can see which tree the
    // Judge will walk, especially when --deliverable-path differs from
    // --workdir. See SPEC §12 item 2.
    if deliverable_path != workdir {
        elog!(
            "[alps] deliverable path: {} (workdir: {})",
            deliverable_path.display(),
            workdir.display()
        );
    }

    // Create per-task branch in the workdir so receipts + plan + feedback are
    // tracked in git history. The user can review `alps/<task-id>` to see
    // what alps did for that run, then merge to main or discard.
    //
    // If --workdir is not inside a git repo (e.g. `/tmp/foo` for an
    // ephemeral smoke), skip the branch creation silently rather than
    // warning. Tier 4 smokes burn through /tmp/alps-tier4-notes-workdir
    // per-run and don't need per-task branch isolation; the orchestrator's
    // own git init (in implement.rs:run_git) creates a repo inside
    // tasks/<id>/implementation/ralph/ where ralph's commits live.
    let branch = format!("alps/{}", task_id.as_str());
    if is_inside_git_repo(&workdir) {
        match create_branch(&workdir, &branch) {
            Ok(()) => elog!("[alps] on branch: {}", branch),
            Err(GitOpsError::Git { op, msg }) => {
                elog!("warning: per-task branch '{}' failed at {}: {}", branch, op, msg);
                elog!("warning: continuing on current branch (no per-task isolation)");
            }
            Err(e) => {
                elog!("warning: per-task branch failed: {}", e);
                elog!("warning: continuing on current branch (no per-task isolation)");
            }
        }
    }
    // else: --workdir is not a git repo (e.g. /tmp ephemeral smoke).
    // Per-task branch isolation is intentionally unavailable here; the
    // orchestrator's own git init covers ralph's commits. Silent skip.

    let task = Task::<alps_core::task::Idle>::new(
        task_id.clone(),
        workspace.root.clone(),
        Prompt::new(prompt),
    );

    // Resolve the ALPS repo root (where scripts/ and the vendored ralph.sh live).
    // We use the binary's own path: target/{debug,release}/alps → ../../..
    let alps_root = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let ralph_path = alps_root.join("scripts/ralph.sh");
    let claude_prompt_path = alps_root.join("scripts/CLAUDE.md");

    let plan = PlanAgent::new("claude-sonnet-4");
    let implement = ImplementAgent::new(
        workspace.root.clone(),
        alps_core::implement::ImplementConfig {
            ralph_path,
            claude_prompt_path,
            deliverable_path,
            ..Default::default()
        },
    );
    let review = ReviewAgent::default();
    let judge = JudgeAgent::new(
        Arc::new(DoDRunner::new()),
        Arc::new(HermesLlmJudge::default()),
    );

    match drive(task, &plan, &implement, &review, &judge, &workspace).await {
        Ok(done) => {
            // Write receipts.json (the durable artifact)
            alps_core::persistence::persist_task(&done, &workspace)
                .map_err(|e| anyhow::anyhow!("persistence failed: {}", e))?;

            // Auto-commit only if there are changes. Most ALPS runs produce
            // work in tasks/<id>/ which is gitignored, so this is a no-op
            // in practice. The smart check prevents a noisy warning.
            match commit_smart_with_excludes(
                &workdir,
                &format!("done: {}", task_id.as_str()),
                if deliverable_path_for_commit != workdir {
                    Some(deliverable_path_for_commit.as_path())
                } else {
                    None
                },
            ) {
                Ok(CommitOutcome::NothingToCommit) => {} // silent — expected
                Ok(CommitOutcome::Committed) => {
                    elog!("[done] auto-committed final state");
                }
                Ok(CommitOutcome::CommitFailed(msg)) => {
                    elog!("warning: git commit failed: {}", msg);
                }
                Err(GitOpsError::Git { op, msg }) => {
                    elog!("warning: git {} error: {}", op, msg);
                }
                Err(e) => {
                    elog!("warning: auto-commit skipped: {}", e);
                }
            }

            // Print markdown summary to stdout
            print_markdown(&done);

            // Mark the workdir as having a recently completed task. This
            // is the OTHER HALF of the workdir guard — combined with the
            // check at startup, it prevents the wrapping agent (Claude TUI
            // / shell) from auto-re-invoking alps within the debounce
            // window after seeing "ALPS — Done" (observed 2026-07-27).
            if let Err(e) =
                alps_core::workdir_guard::mark_complete(&workdir, task_id.as_str())
            {
                elog!("warning: failed to mark workdir complete: {}", e);
            }

            Ok(())
        }
        Err(e) => {
            error!(target: "alps.cli", error = %e, "task failed");
            elog!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_markdown(task: &Task<Done>) {
    let r: &Receipts = task.receipts();
    println!();
    println!("# ALPS — Done");
    println!();
    println!("**Task ID:** `{}`", task.id.as_str());
    println!("**Attempts:** {}", task.attempts());
    println!();
    println!("## Receipts");
    println!();
    println!("- **Plan summary:** {}", r.plan_summary);
    println!("- **Stories:** {}/{} passed", r.implement_metrics.stories_passed, r.implement_metrics.stories_total);
    println!("- **Implement iterations:** {}", r.implement_metrics.iterations);
    println!("- **Implement elapsed:** {}s", r.implement_metrics.elapsed_secs);
    println!("- **Review findings:** {} ({} critical)",
        r.review_summary.findings_count, r.review_summary.critical_findings);
    println!("- **Review assertions:** {}/{} passed",
        r.review_summary.assertions_passed, r.review_summary.assertions_total);
    println!("- **Judged at:** {}", r.judged_at.to_rfc3339());
    println!("- **Judge model:** {}", r.judge_model);
    println!();
    println!("Receipts written to `tasks/{}/receipts.json`", task.id.as_str());
}

/// Check whether `dir` is inside a git work tree. Used to gate the
/// per-task branch creation in `run_task` — when `--workdir` points at
/// an ephemeral directory like `/tmp/alps-tier4-notes-workdir`, there
/// is no parent repo to branch from, and the warning that the CLI used
/// to emit (`warning: per-task branch 'alps/...' failed ...`) was
/// noise. The orchestrator's own `git init` (in
/// `implement.rs::run_git`) covers ralph's commits inside the task
/// dir, so per-task branch isolation is genuinely not needed there.
fn is_inside_git_repo(dir: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_inside_git_repo;
    use super::resolve_prompt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Generate a unique temp file path under std::env::temp_dir() so parallel
    /// tests don't collide. Uses an atomic counter — `mktemp`-like behavior
    /// without pulling in the `tempfile` crate.
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    fn unique_tmp_path(suffix: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("alps-test-{}-{}-{}{}", pid, n, std::any::type_name::<()>(), suffix))
    }

    #[test]
    fn prompt_file_path_with_valid_content() {
        // Wrapper writes a 5KB prompt to a file, resolve_prompt reads it,
        // the contents reach run_task unchanged.
        let path = unique_tmp_path(".txt");
        let body = "Build a full-stack notes app at /tmp/x.\n".repeat(100); // ~5KB
        std::fs::write(&path, &body).unwrap();

        let result = resolve_prompt(None, Some(path.to_str().unwrap()));
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        assert_eq!(result.unwrap(), body, "file contents must round-trip unchanged");

        // File is deleted after read.
        assert!(!path.exists(), "file should be deleted after read");
    }

    #[test]
    fn prompt_file_path_with_missing_file() {
        // Wrapper points at a non-existent path; resolve_prompt returns
        // an error containing the path and the io error.
        let path = unique_tmp_path("-missing.txt");

        let result = resolve_prompt(None, Some(path.to_str().unwrap()));
        assert!(result.is_err(), "expected Err for missing file");
        let err = result.unwrap_err();
        assert!(err.contains("--prompt-file"), "error must mention --prompt-file: {}", err);
        assert!(err.contains(path.to_str().unwrap()), "error must include the path: {}", err);
        // Main() maps this to exit code 2 — verified by reading main.rs flow.
    }

    #[test]
    fn prompt_and_prompt_file_both_given() {
        // Both flags present: prompt (argv) wins. clap's conflicts_with
        // prevents this at the CLI level, but the API is permissive so
        // callers don't have to think about it.
        let argv_prompt = Some("from argv".to_string());
        let result = resolve_prompt(argv_prompt.clone(), Some("/tmp/some-file"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "from argv", "argv prompt should win when both given");
    }

    #[test]
    fn prompt_file_deleted_after_read() {
        // Pin the post-condition: the file is removed (best-effort) after
        // resolve_prompt returns. This protects against prompt text being
        // left on disk indefinitely in /tmp.
        let path = unique_tmp_path(".txt");
        std::fs::write(&path, "delete me after read").unwrap();
        assert!(path.exists(), "precondition: file must exist before resolve");

        let _ = resolve_prompt(None, Some(path.to_str().unwrap()));
        assert!(!path.exists(), "file must be deleted after resolve_prompt returns Ok");
    }

    #[test]
    fn resolve_prompt_with_neither_returns_error() {
        // Defensive: neither flag → error (clap should reject this upstream
        // because prompt is required, but the API must be defensive too).
        let result = resolve_prompt(None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("required"));
    }

    #[test]
    fn is_inside_git_repo_returns_true_for_alps_repo() {
        // The alps repo itself is a git work tree (we run these tests
        // from its root). Sanity-check the helper from a known-good dir.
        let cwd = std::env::current_dir().unwrap();
        assert!(
            is_inside_git_repo(&cwd),
            "expected cwd ({}) to be inside a git work tree",
            cwd.display()
        );
    }

    #[test]
    fn is_inside_git_repo_returns_false_for_ephemeral_tmp_dir() {
        // /tmp is intentionally not a git repo on this host. If this
        // ever fails (e.g. someone ran `git init` in /tmp), the
        // per-task branch creation will start succeeding for /tmp/
        // workdirs and create orphan branches — worth noticing.
        let tmp = std::env::temp_dir();
        // Sanity: temp dir exists and is writable.
        assert!(tmp.is_dir(), "temp_dir should be a directory");
        assert!(
            !is_inside_git_repo(&tmp),
            "expected {} to NOT be inside a git work tree (the CLI's silent-skip behavior depends on this)",
            tmp.display()
        );
    }
}
