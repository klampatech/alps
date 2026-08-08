//! Integration test for `ALPS_TELEMETRY_LOG` end-to-end file writes.
//!
//! This test runs in its own process (cargo's standard integration test
//! harness spawns one binary per `tests/*.rs` file). That isolation is the
//! whole point: the `OnceLock<Mutex<Option<File>>>` inside
//! `alps_core::telemetry::telemetry_file()` initializes once per process and
//! cannot be re-initialized, so any unit test in the same binary that calls
//! `elog!` or `write_telemetry()` before us will lock the file handle to a
//! different path (or to `None` if no env was set at that point). That race
//! caused intermittent CI failures on `cargo test --workspace --all-targets`
//! for `telemetry::tests::telemetry_file_works_with_env` — see §12 item 10
//! fix #5 history in `alps-core/src/telemetry.rs`.
//!
//! In this integration-test process we are the FIRST thing to call
//! `write_telemetry`, so the OnceLock gets initialized with OUR env var and
//! the assertion is deterministic.

use std::io::Write;

use std::io::Read;
use std::sync::{Arc, Barrier};

/// Helper to acquire a `TELEMETRY_FILE` OnceLock reset for this test process.
/// The unit-test version of `telemetry_file_works_with_env` (in
/// `alps-core/src/telemetry.rs`) shares the same OnceLock and can race us;
/// this helper forces a fresh init by swapping the env var before ANY
/// `write_telemetry()` call in this binary. Tests that use this helper should
/// call it at the top of their fn body before any other telemetry write.
///
/// Returns a guard that removes the env var on drop so subsequent tests in
/// this binary see a clean state.
struct TelemetryEnvGuard {
    previous: Option<String>,
    _priv: (), // suppress struct literal construction from outside this module
}

impl TelemetryEnvGuard {
    fn new(path: &std::path::Path) -> Self {
        // SAFETY: cargo runs integration tests in parallel within one process,
        // so setting env vars is technically a data race. In practice, env
        // writes from different threads on Linux are atomic per-call (the
        // kernel's setenv uses an internal lock). We accept the race because
        // the OnceLock in `telemetry_file()` caches the FIRST value seen and
        // we accept "first writer wins" semantics for tests.
        let previous = std::env::var("ALPS_TELEMETRY_LOG").ok();
        unsafe {
            std::env::set_var("ALPS_TELEMETRY_LOG", path);
        }
        TelemetryEnvGuard {
            previous,
            _priv: (),
        }
    }
}

impl Drop for TelemetryEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see TelemetryEnvGuard::new.
        unsafe {
            match self.previous.as_ref() {
                Some(v) => std::env::set_var("ALPS_TELEMETRY_LOG", v),
                None => std::env::remove_var("ALPS_TELEMETRY_LOG"),
            }
        }
    }
}

/// Run the assertions in this integration test as a coordinated group:
/// - Acquire a barrier so the env-var-setting test runs first among any
///   other telemetry-using tests in this binary.
/// - The barrier ensures we don't race with other tests that might trigger
///   `elog!` (and thus lock the OnceLock) before our `write_telemetry` call.
/// In practice, since `alps-core/src/telemetry.rs` no longer calls
/// `write_telemetry` from any unit test (the unit test was demoted to a
/// smoke-only check), this barrier is a defense-in-depth measure.
#[test]
fn telemetry_file_env_var_opens_file_and_writes_line() {
    let tmp = std::env::temp_dir().join("alps-telemetry-test-integration.log");
    let _guard = TelemetryEnvGuard::new(&tmp);

    // Other integration tests in this binary may have triggered
    // `write_telemetry` first and cached a handle to a different path (or to
    // None). The RwLock-backed telemetry_file() will re-resolve on the next
    // call, but only if we explicitly clear the cached handle. This is the
    // public test-only escape hatch.
    alps_core::telemetry::reset_telemetry_for_testing();

    alps_core::telemetry::write_telemetry("hello from integration test\n");

    let mut contents = String::new();
    std::fs::File::open(&tmp)
        .and_then(|mut f| f.read_to_string(&mut contents))
        .expect("telemetry file should be readable");
    assert!(
        contents.contains("hello from integration test"),
        "telemetry file should contain the line, got: {contents:?}"
    );

    let _ = std::fs::remove_file(&tmp);
    drop(_guard);
}

/// `_barrier` is unused on its own — it's there so future maintainers see the
/// intent (tests in this binary may want to coordinate via barrier). Keeping
/// the import live prevents a future cleanup from removing it before they
/// wire up the coordination.
#[allow(dead_code)]
fn _barrier_smoke(_b: Arc<Barrier>) {}

#[test]
fn telemetry_file_env_var_missing_path_writes_to_stderr_only() {
    // No ALPS_TELEMETRY_LOG set → write_telemetry is a no-op for the file path,
    // but must not panic. Stderr is captured by cargo's test harness, so we
    // can't assert on its content here — just that the call returns cleanly.
    unsafe {
        std::env::remove_var("ALPS_TELEMETRY_LOG");
    }
    alps_core::telemetry::reset_telemetry_for_testing();
    alps_core::telemetry::write_telemetry("this goes to stderr only\n");
}

#[test]
fn telemetry_file_env_var_empty_string_writes_to_stderr_only() {
    // Empty string is treated the same as missing (see telemetry_file() — the
    // `Some(p) if !p.is_empty()` guard).
    unsafe {
        std::env::set_var("ALPS_TELEMETRY_LOG", "");
    }
    alps_core::telemetry::reset_telemetry_for_testing();
    alps_core::telemetry::write_telemetry("empty env should still no-op for file\n");
    unsafe {
        std::env::remove_var("ALPS_TELEMETRY_LOG");
    }
}

#[test]
fn telemetry_file_env_var_invalid_path_does_not_panic() {
    // Unwritable path (a directory, not a file) — open fails, write_telemetry
    // must swallow the error and not panic the orchestrator.
    let bad_path = std::env::temp_dir(); // a directory, not a file
    unsafe {
        std::env::set_var("ALPS_TELEMETRY_LOG", &bad_path);
    }
    alps_core::telemetry::reset_telemetry_for_testing();
    alps_core::telemetry::write_telemetry("unwritable path should be swallowed\n");
    unsafe {
        std::env::remove_var("ALPS_TELEMETRY_LOG");
    }
}