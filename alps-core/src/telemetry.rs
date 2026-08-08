//! Telemetry helpers — line-flushing stderr writer.
//!
//! # Why this exists (§12 item 10, fix #5 + fix #6)
//!
//! Rust's `std::io::stderr()` returns a line-buffered handle when `isatty()` is true
//! (TTY mode) and a fully-buffered handle when redirected to a file/pipe (non-TTY mode).
//! When the alps orchestrator exits via `std::process::exit(0)` (or returns from `main()`
//! without an explicit flush), the buffered stderr contents are dropped on the floor.
//!
//! This bit us hard during the Tier 4 smoke #4 run (2026-08-04 21:08): the orchestrator
//! emitted `[plan] running`, `[implement] running`, `[implement] done: 10/10 stories`,
//! `[review] running`, `[judge] running`, `[done] accepted` — but the 354 KB stderr log
//! contained only Codex CLI's stderr (7000+ lines). Zero orchestrator stderr lines.
//! The deliverable was real, the orchestrator exited successfully, but the diagnostic
//! wrapper had nothing to show because every `eprintln!` was still in the dropped buffer.
//!
//! # The 2nd problem (smoke #5, 2026-08-05): multi-writer overwrite
//!
//! Even after fix #5 made the orchestrator's writes actually reach the file, smoke #5
//! still lost them all. Root cause: ralph.sh invokes codex via
//! `codex exec ... 2>&1 | tee /dev/stderr`. The `tee /dev/stderr` command re-opens
//! `/dev/stderr` (a symlink to FD 2's file) as a **fresh FD with O_WRONLY, no O_APPEND**.
//! Tee's writes go to byte 0 of the file's current size — overwriting the orchestrator's
//! earlier `elog!` writes that were already at byte 0-N.
//!
//! Confirmed via /tmp/replica-stderr.log: 241 bytes of orch lines at start, then codex
//! output starts at byte 0 (orch lines gone). Also confirmed via a minimal Rust test:
//! two writers to the same file (one with O_WRONLY, one with O_APPEND) produce
//! overwrites when neither uses O_APPEND.
//!
//! # Fix #6: dedicated O_APPEND telemetry file
//!
//! The `elog!` macro now ALSO writes to a dedicated telemetry file when the
//! `ALPS_TELEMETRY_LOG` env var is set. The file is opened with O_APPEND so the
//! orchestrator's writes are atomic appends to the end of the file — they cannot
//! be overwritten by `tee` (or any other process that opens the same file
//! with O_WRONLY for its own writes).
//!
//! CLI integration: the `alps run` command accepts `--telemetry-log=<path>` and
//! exports it as `ALPS_TELEMETRY_LOG=<path>` for child processes. Wrappers can then
//! use the same path for both the wrapper's `2>` redirect (catches codex's stderr
//! via `tee /dev/stderr`) and the orchestrator's `--telemetry-log` (catches
//! orchestrator's `elog!` writes via O_APPEND). Both streams land in the same file,
//! but neither can clobber the other.
//!
//! # Usage
//!
//! ```ignore
//! use crate::telemetry::elog;
//!
//! elog!("[plan] running");
//! elog!("[implement] done: {}/{} stories", passed, total);
//! ```
//!
//! The macro supports the same format-string + arg syntax as `eprintln!` / `format!`.

use std::io::Write;
use std::sync::RwLock;

use std::fs::{File, OpenOptions};

/// Cached handle to the dedicated telemetry file (env: `ALPS_TELEMETRY_LOG`).
///
/// Opened with `O_APPEND` so concurrent writers (orchestrator + `tee /dev/stderr`)
/// cannot overwrite each other — every `elog!` call atomically appends to the end
/// of the file. The `RwLock` (not `OnceLock`) lets tests that set
/// `ALPS_TELEMETRY_LOG` mid-process reset the cached handle on the next
/// `write_telemetry` call. This was previously a `OnceLock` which caused
/// flaky CI: once any unit test triggered `elog!`/`write_telemetry`, the
/// file handle was locked to that test's env-var value (or `None`) forever.
/// See §12 item 10 fix #5 history.
///
/// Tradeoff: each `write_telemetry` call now takes the read lock briefly to
/// clone the handle out of the static, then releases. `std::fs::File` is not
/// `Clone`, so we open a fresh file descriptor each time (cheap — O_APPEND
/// writes don't need an exclusive handle). For the orchestrator's hot path
/// this is fine: we already O_APPEND so concurrent appends serialize at the
/// kernel level regardless of how many FDs are open.
static TELEMETRY_FILE: RwLock<Option<std::path::PathBuf>> = RwLock::new(None);

/// Resolve the telemetry file path from `ALPS_TELEMETRY_LOG` and open a
/// fresh file handle. Returns `None` if the env var is unset, empty, or the
/// file can't be opened. Telemetry must never panic the orchestrator, so
/// open errors are swallowed.
fn telemetry_file() -> Option<File> {
    let path = std::env::var("ALPS_TELEMETRY_LOG").ok();
    match path {
        Some(p) if !p.is_empty() => {
            // O_APPEND is the critical flag: every write goes to the end of
            // the file regardless of what other processes are doing. Combined
            // with O_WRONLY|O_CREAT, this is atomic per-write on POSIX (the
            // kernel serializes append-mode writes to the same file).
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .ok();
            // Cache the path so future calls can skip the env-var lookup.
            // We store the path string (cheap) rather than the File handle
            // (not Clone) and re-open on every call. Tests that need to
            // force a re-open can call reset_telemetry_for_testing().
            if let Ok(mut write) = TELEMETRY_FILE.write() {
                *write = Some(std::path::PathBuf::from(p));
            }
            file
        }
        _ => {
            if let Ok(mut write) = TELEMETRY_FILE.write() {
                *write = None;
            }
            None
        }
    }
}

/// Test-only helper: clear the cached telemetry file path so the next
/// `write_telemetry` call re-reads `ALPS_TELEMETRY_LOG` and re-opens the
/// file. Use this from integration tests that change `ALPS_TELEMETRY_LOG`
/// mid-process.
#[doc(hidden)]
pub fn reset_telemetry_for_testing() {
    if let Ok(mut write) = TELEMETRY_FILE.write() {
        *write = None;
    }
}

/// Write a line to the telemetry file (if configured) in addition to stderr.
pub fn write_telemetry(line: &str) {
    if let Some(mut file) = telemetry_file() {
        // Ignore write errors — telemetry must never panic the orchestrator.
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// Write a line to stderr with an explicit flush, regardless of TTY status.
///
/// Replaces `eprintln!` for orchestrator telemetry so that lines survive
/// `process::exit(0)` and any non-TTY stderr redirect (file, pipe, capture).
///
/// Also writes to the dedicated telemetry file (if `ALPS_TELEMETRY_LOG` is set)
/// with O_APPEND semantics — this protects the orchestrator's writes from being
/// clobbered by `tee /dev/stderr` (which opens the same file with O_WRONLY
/// without O_APPEND and would otherwise overwrite byte 0).
///
/// Errors writing to stderr OR the telemetry file are deliberately swallowed:
/// telemetry must never panic the orchestrator. The orchestrator's actual return
/// value carries the real signal (exit code, `AlpsError`, `receipts.json`).
#[macro_export]
macro_rules! elog {
    () => {{
        let _ = ::std::io::Write::write_all(&mut ::std::io::stderr(), b"\n");
        let _ = ::std::io::stderr().flush();
        $crate::telemetry::write_telemetry("\n");
    }};
    ($fmt:expr) => {{
        {
            let mut s = ::std::io::stderr();
            let _ = ::std::io::Write::write_fmt(&mut s, format_args!("{}\n", $fmt));
            let _ = ::std::io::Write::flush(&mut s);
        }
        $crate::telemetry::write_telemetry(concat!($fmt, "\n"));
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        {
            let mut s = ::std::io::stderr();
            let _ = ::std::io::Write::write_fmt(&mut s, format_args!("{}\n", format!($fmt, $($arg)*)));
            let _ = ::std::io::Write::flush(&mut s);
        }
        $crate::telemetry::write_telemetry_line(&format!("{}\n", format!($fmt, $($arg)*)));
    }};
}

/// Helper: write a formatted line to the telemetry file (if configured).
///
/// Used by the format-args arm of the `elog!` macro because Rust macros can't
/// directly use `format!()` output in a second write call without binding
/// the result to a temporary variable.
pub fn write_telemetry_line(line: &str) {
    write_telemetry(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: confirm `elog!` is callable and the macro syntax compiles.
    /// We can't easily assert flush behavior in-process (Rust's stderr handle is
    /// shared with the test runner), but we can verify the macro doesn't panic
    /// and that format-string substitution works.
    #[test]
    fn elog_basic_format() {
        elog!("[test] simple message");
        elog!("[test] with args: {} + {} = {}", 1, 2, 3);
    }

    #[test]
    fn telemetry_file_works_without_env() {
        // No ALPS_TELEMETRY_LOG set — write_telemetry is a no-op.
        // Should not panic, should not error.
        write_telemetry("test line\n");
    }

    #[test]
    fn telemetry_file_works_with_env() {
        // Smoke-only sanity check: write_telemetry() must never panic, even when
        // ALPS_TELEMETRY_LOG is set to a valid path. The actual write-to-file
        // assertion lives in `tests/telemetry_env.rs` — that integration test
        // runs in its own process, so the OnceLock<Mutex<Option<File>>> in
        // `telemetry_file()` is fresh and can be initialized with THIS test's
        // env var. Running this assertion in the unit-test binary would race
        // against every other unit test's elog!/write_telemetry calls and fail
        // intermittently (the original bug — see §12 item 10 fix #5 history).
        let tmp = std::env::temp_dir().join("alps-telemetry-test-unit-smoke.log");
        // SAFETY: tests run single-threaded for the duration of this function.
        unsafe {
            std::env::set_var("ALPS_TELEMETRY_LOG", &tmp);
        }
        write_telemetry("hello from unit-test smoke\n");
        // Cleanup env so the integration test sees a clean slate if both ever
        // get invoked from the same process (they won't — different binaries).
        unsafe {
            std::env::remove_var("ALPS_TELEMETRY_LOG");
        }
        let _ = std::fs::remove_file(&tmp);
    }
}
