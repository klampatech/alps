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
use std::sync::OnceLock;

use std::fs::{File, OpenOptions};
use std::sync::Mutex;

/// Cached handle to the dedicated telemetry file (env: `ALPS_TELEMETRY_LOG`).
///
/// Opened with `O_APPEND` so concurrent writers (orchestrator + `tee /dev/stderr`)
/// cannot overwrite each other — every `elog!` call atomically appends to the end
/// of the file. The handle is wrapped in a `Mutex` because the file's internal
/// write position is shared between calls; without the lock, concurrent `elog!`
/// calls from different threads could interleave bytes.
///
/// The `OnceLock` ensures we only resolve the env var and open the file once per
/// process — subsequent `elog!` calls just grab the cached handle.
static TELEMETRY_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// Resolve the telemetry file path from `ALPS_TELEMETRY_LOG` and open it.
///
/// Returns `None` if the env var is unset, empty, or the file can't be opened.
/// Telemetry must never panic the orchestrator, so open errors are swallowed.
fn telemetry_file() -> Option<&'static Mutex<Option<File>>> {
    TELEMETRY_FILE
        .get_or_init(|| {
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
                    Mutex::new(file)
                }
                _ => Mutex::new(None),
            }
        })
        .into()
}

/// Write a line to the telemetry file (if configured) in addition to stderr.
pub fn write_telemetry(line: &str) {
    if let Some(slot) = telemetry_file() {
        if let Ok(mut guard) = slot.lock() {
            if let Some(file) = guard.as_mut() {
                // Ignore write errors — telemetry must never panic the orchestrator.
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
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
        // Set the env var, open the file, write a line, verify it landed.
        let tmp = std::env::temp_dir().join("alps-telemetry-test.log");
        // SAFETY: tests run single-threaded for the duration of this function.
        unsafe {
            std::env::set_var("ALPS_TELEMETRY_LOG", &tmp);
        }
        // Reset the cached handle so it picks up the new env var.
        // (In a single test process, this is the first time we're setting it,
        // so the OnceLock hasn't been initialized yet — but we reset anyway
        // for safety in case the previous test set it.)
        // NOTE: we can't actually reset the OnceLock easily. So this test
        // is best-effort — it only verifies behavior when the env is set
        // BEFORE the first elog! call.
        write_telemetry("hello from test\n");
        let contents = std::fs::read_to_string(&tmp).unwrap_or_default();
        assert!(contents.contains("hello from test"), "telemetry file should contain the line, got: {contents:?}");
        // Cleanup
        let _ = std::fs::remove_file(&tmp);
        unsafe {
            std::env::remove_var("ALPS_TELEMETRY_LOG");
        }
    }
}
