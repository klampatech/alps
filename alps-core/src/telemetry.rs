//! Telemetry helpers — line-flushing stderr writer.
//!
//! # Why this exists (§12 item 10, fix #5)
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
//! The `elog!` macro below replaces `eprintln!` everywhere in the orchestrator. It uses
//! `std::io::Write` directly (unbuffered writes) and explicitly flushes after every line.
//! Cost: one extra syscall per line (negligible — the orchestrator emits ~10 lines per
//! smoke). Benefit: any operator wrapper can now grep for `[plan|implement|review|judge|done|rejected]`
//! in the orchestrator's stderr and find the exact death point.
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

/// Write a line to stderr with an explicit flush, regardless of TTY status.
///
/// Replaces `eprintln!` for orchestrator telemetry so that lines survive
/// `process::exit(0)` and any non-TTY stderr redirect (file, pipe, capture).
///
/// Errors writing to stderr are deliberately swallowed: telemetry must never
/// panic the orchestrator. The orchestrator's actual return value carries the
/// real signal (exit code, `AlpsError`, `receipts.json`).
#[macro_export]
macro_rules! elog {
    () => {{
        // Empty line — still flush so a trailing newline lands on disk.
        let _ = ::std::io::Write::write_all(&mut ::std::io::stderr(), b"\n");
        let _ = ::std::io::stderr().flush();
    }};
    ($fmt:expr) => {{
        let mut s = ::std::io::stderr();
        let _ = ::std::io::Write::write_fmt(&mut s, format_args!("{}\n", $fmt));
        let _ = ::std::io::Write::flush(&mut s);
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        let mut s = ::std::io::stderr();
        let _ = ::std::io::Write::write_fmt(&mut s, format_args!("{}\n", format!($fmt, $($arg)*)));
        let _ = ::std::io::Write::flush(&mut s);
    }};
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
}
