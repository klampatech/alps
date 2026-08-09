//! Integration test for `alps_core::ralph::run` state-file semantics.
//!
//! 1:1 port of `scripts/test-state-file-location.sh` (which was the bash
//! version's guard against the state-files-in-SCRIPT_DIR bug). Now that
//! Ralph is a Rust library function, we test it directly: provide a fake
//! `codex` binary in PATH, write a 1-story prd.json, run the loop, and
//! assert that `.ralph-result.json`, `.codex-last-message.txt`, and the
//! other state files all live in `ralph_dir` (not in `script_dir`).
//!
//! Also includes the false-positive guard from smoke #8: codex emits the
//! literal `<promise>COMPLETE</promise>` string in a denial, but the prd
//! shows failing stories. Ralph must NOT treat this as completion.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alps_core::implement::{RalphResult, RalphTool};
use alps_core::ralph::{self, RalphConfig};

/// PATH is process-global. Every test in this integration-test binary routes
/// Ralph to a different fake `codex`, so serialize the env mutation + run.
/// Without this lock, `cargo test --all-targets` can launch the real codex or
/// another test's fake binary (the old "unique fake_bin makes it safe" comment
/// was wrong).
static PATH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/// Create a temp directory under /tmp with the given prefix.
fn mktemp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).expect("mktemp dir");
    dir
}

/// Write a 1-story prd.json where the story passes.
fn write_one_story_passing_prd(dir: &Path) {
    let json = r#"{
        "project": "alps-ralph-test",
        "branchName": "alps/test",
        "description": "test fixture",
        "userStories": [
            {"id": "US-001", "title": "x", "description": "x", "acceptanceCriteria": [], "priority": 1, "passes": true}
        ]
    }"#;
    fs::write(dir.join("prd.json"), json).expect("write prd.json");
}

/// Write a 2-story prd.json where one story is still failing.
fn write_one_story_failing_prd(dir: &Path) {
    let json = r#"{
        "project": "alps-ralph-test",
        "branchName": "alps/test",
        "description": "test fixture",
        "userStories": [
            {"id": "US-001", "title": "x", "description": "x", "acceptanceCriteria": [], "priority": 1, "passes": true},
            {"id": "US-002", "title": "y", "description": "y", "acceptanceCriteria": [], "priority": 2, "passes": false}
        ]
    }"#;
    fs::write(dir.join("prd.json"), json).expect("write prd.json");
}

/// Build a fake `codex` binary that:
/// - reads stdin (the AGENTS.md prompt)
/// - emits `<promise>COMPLETE</promise>` on stdout (Ralph greps this)
/// - if `-o <file>` is given (real codex's last-message file), writes the
///   signal to that file too
/// - exits 0
fn write_fake_codex(bin_dir: &Path) {
    let codex = bin_dir.join("codex");
    let script = r#"#!/bin/bash
# Fake codex — mimics real codex just enough for Ralph to think it ran.
last_message_file=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) last_message_file="$2"; shift 2 ;;
        *) shift ;;
    esac
done
# Drain stdin so the parent process doesn't block on the pipe.
cat > /dev/null
echo "<promise>COMPLETE</promise>"
if [[ -n "$last_message_file" ]]; then
    echo "<promise>COMPLETE</promise>" > "$last_message_file"
fi
exit 0
"#;
    fs::write(&codex, script).expect("write fake codex");
    fs::set_permissions(&codex, PermissionsExt::from_mode(0o755)).expect("chmod fake codex");
}

/// Build a fake `codex` that emits the literal COMPLETE string in a
/// *denial* (smoke #8 false-positive regression test).
fn write_fake_codex_with_denial(bin_dir: &Path) {
    let codex = bin_dir.join("codex");
    let script = r#"#!/bin/bash
last_message_file=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) last_message_file="$2"; shift 2 ;;
        *) shift ;;
    esac
done
cat > /dev/null
# The denial: codex MENTIONS the COMPLETE string but doesn't emit it.
echo "US-002 is still failing, so no <promise>COMPLETE</promise> is emitted this iteration."
if [[ -n "$last_message_file" ]]; then
    echo "US-002 is still failing, so no <promise>COMPLETE</promise> is emitted this iteration." > "$last_message_file"
fi
exit 0
"#;
    fs::write(&codex, script).expect("write fake codex with denial");
    fs::set_permissions(&codex, PermissionsExt::from_mode(0o755)).expect("chmod fake codex");
}

/// Build a fake codex that records its working directory before completing.
fn write_fake_codex_recording_cwd(bin_dir: &Path, cwd_log: &Path) {
    let codex = bin_dir.join("codex");
    let script = format!(
        r#"#!/bin/bash
last_message_file=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) last_message_file="$2"; shift 2 ;;
        *) shift ;;
    esac
done
pwd > "{}"
cat > /dev/null
echo "<promise>COMPLETE</promise>"
if [[ -n "$last_message_file" ]]; then
    echo "<promise>COMPLETE</promise>" > "$last_message_file"
fi
exit 0
"#,
        cwd_log.display()
    );
    fs::write(&codex, script).expect("write cwd-recording fake codex");
    fs::set_permissions(&codex, PermissionsExt::from_mode(0o755))
        .expect("chmod cwd-recording fake codex");
}

/// Run `ralph::run` with the given ralph_dir, script_dir, and a modified
/// PATH that puts `fake_bin` first. Returns the result.
async fn run_ralph_with_fake_codex(
    ralph_dir: PathBuf,
    script_dir: PathBuf,
    fake_bin: PathBuf,
    max_iterations: u32,
) -> RalphResult {
    let _path_guard = PATH_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    // Save and prepend PATH. The process-global mutation is serialized by
    // PATH_LOCK until Ralph finishes and PATH is restored.
    let prev_path = std::env::var("PATH").ok();
    let new_path = match &prev_path {
        Some(p) => format!("{}:{}", fake_bin.display(), p),
        None => fake_bin.display().to_string(),
    };
    // SAFETY: PATH mutation is process-global, so PATH_LOCK serializes all
    // fake-codex Ralph runs in this integration-test binary.
    unsafe {
        std::env::set_var("PATH", &new_path);
    }

    let cfg = RalphConfig::new(ralph_dir, script_dir, RalphTool::Codex, max_iterations);
    let result = ralph::run(cfg).await.expect("ralph::run");

    // Restore PATH.
    // SAFETY: see above.
    unsafe {
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }
    result
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

/// Port of `scripts/test-state-file-location.sh` (the bash guard against
/// the state-files-in-SCRIPT_DIR bug). State files MUST live in `ralph_dir`,
/// NOT in `script_dir`.
#[tokio::test]
async fn state_files_live_in_ralph_dir_not_script_dir() {
    let ralph_dir = mktemp_dir("alps-ralph-test-state-location");
    let script_dir = mktemp_dir("alps-ralph-test-script-dir");
    let fake_bin = mktemp_dir("alps-ralph-test-fake-bin");

    write_one_story_passing_prd(&ralph_dir);
    // Drop an AGENTS.md into ralph_dir. Ralph prefers the workspace's
    // AGENTS.md (matches the bash fallback at line 202-205 of ralph.sh),
    // and the orchestrator's step 3 normally copies it in for production
    // runs. The fake-codex test doesn't read it for content — it just
    // needs the file to exist so Ralph can pipe its bytes to codex's
    // stdin.
    fs::write(
        ralph_dir.join("AGENTS.md"),
        "# Ralph agent instructions (fake, for tests)\n",
    )
    .expect("write AGENTS.md to ralph_dir");
    write_fake_codex(&fake_bin);

    let result =
        run_ralph_with_fake_codex(ralph_dir.clone(), script_dir.clone(), fake_bin.clone(), 5).await;

    // Ralph must claim completion (1-story prd, all passing, fake codex emits COMPLETE).
    assert!(
        result.completed,
        "Ralph should complete for 1-story passing prd (got iterations={})",
        result.iterations
    );

    // Assertions from the bash test:
    let state_files = [
        ".ralph-result.json",
        ".codex-last-message.txt",
        "prd.json",
        "progress.txt",
    ];
    for f in &state_files {
        assert!(
            ralph_dir.join(f).exists(),
            "{} should exist in ralph_dir ({})",
            f,
            ralph_dir.display()
        );
        if *f != "ralph.sh" {
            // No state file should leak into script_dir (the analog of
            // SCRIPT_DIR in the bash version).
            assert!(
                !script_dir.join(f).exists(),
                "{} leaked into script_dir ({}) — state files must live in ralph_dir only",
                f,
                script_dir.display()
            );
        }
    }

    // Validate .ralph-result.json shape.
    let result_text =
        fs::read_to_string(ralph_dir.join(".ralph-result.json")).expect("read .ralph-result.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&result_text).expect("parse .ralph-result.json");
    assert_eq!(parsed["completed"], serde_json::json!(true));
    assert!(parsed["iterations"].as_u64().unwrap() >= 1);
    assert!(parsed["elapsed_secs"].as_u64().is_some());

    // Cleanup
    let _ = fs::remove_dir_all(&ralph_dir);
    let _ = fs::remove_dir_all(&script_dir);
    let _ = fs::remove_dir_all(&fake_bin);
}

/// Smoke #8 regression: codex emits the literal `<promise>COMPLETE</promise>`
/// string in a denial prose, but prd shows 1/2 stories passing. Ralph
/// MUST NOT treat this as completion — the cross-check guard rejects it.
#[tokio::test]
async fn phantom_complete_in_prose_is_not_real_completion() {
    let ralph_dir = mktemp_dir("alps-ralph-test-phantom-complete");
    let script_dir = mktemp_dir("alps-ralph-test-phantom-script");
    let fake_bin = mktemp_dir("alps-ralph-test-phantom-fake-bin");

    write_one_story_failing_prd(&ralph_dir);
    fs::write(
        ralph_dir.join("AGENTS.md"),
        "# Ralph agent instructions (fake, for tests)\n",
    )
    .expect("write AGENTS.md");
    write_fake_codex_with_denial(&fake_bin);

    // Max-iter 3 so the test runs fast. After 3 iterations with no real
    // completion signal, Ralph writes .ralph-result.json with
    // completed=false.
    let result =
        run_ralph_with_fake_codex(ralph_dir.clone(), script_dir.clone(), fake_bin.clone(), 3).await;

    // The grep WOULD have matched the literal string (it's in the
    // denial text), but the cross-check guard MUST reject the claim
    // because US-002 is still failing.
    assert!(
        !result.completed,
        "phantom COMPLETE in prose must NOT be treated as completion (smoke #8 regression)"
    );

    // Verify .ralph-result.json reflects the rejection.
    let result_text =
        fs::read_to_string(ralph_dir.join(".ralph-result.json")).expect("read .ralph-result.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&result_text).expect("parse .ralph-result.json");
    assert_eq!(
        parsed["completed"],
        serde_json::json!(false),
        ".ralph-result.json must show completed=false (cross-check guard fired)"
    );

    let _ = fs::remove_dir_all(&ralph_dir);
    let _ = fs::remove_dir_all(&script_dir);
    let _ = fs::remove_dir_all(&fake_bin);
}

/// The `--tool` validation in `alps ralph` matches bash (lines 41-45):
/// invalid tools are rejected with exit 1.
#[tokio::test]
async fn state_files_persist_across_iterations_via_progress_txt() {
    // Sanity check: after Ralph runs (even just 1 iteration), progress.txt
    // MUST exist in ralph_dir (not script_dir). Mirrors the bash init at
    // lines 157-162.
    let ralph_dir = mktemp_dir("alps-ralph-test-progress-txt");
    let script_dir = mktemp_dir("alps-ralph-test-progress-script");
    let fake_bin = mktemp_dir("alps-ralph-test-progress-fake-bin");

    write_one_story_passing_prd(&ralph_dir);
    // Drop an AGENTS.md into ralph_dir. Ralph prefers the workspace's
    // AGENTS.md (matches the bash fallback at line 202-205 of ralph.sh),
    // and the orchestrator's step 3 normally copies it in for production
    // runs. The fake-codex test doesn't read it for content — it just
    // needs the file to exist so Ralph can pipe its bytes to codex's
    // stdin.
    fs::write(
        ralph_dir.join("AGENTS.md"),
        "# Ralph agent instructions (fake, for tests)\n",
    )
    .expect("write AGENTS.md to ralph_dir");
    write_fake_codex(&fake_bin);

    let _result =
        run_ralph_with_fake_codex(ralph_dir.clone(), script_dir.clone(), fake_bin.clone(), 5).await;

    let progress = fs::read_to_string(ralph_dir.join("progress.txt"))
        .expect("progress.txt exists in ralph_dir");
    assert!(
        progress.contains("Ralph Progress Log"),
        "progress.txt should be initialized with the standard header"
    );

    let _ = fs::remove_dir_all(&ralph_dir);
    let _ = fs::remove_dir_all(&script_dir);
    let _ = fs::remove_dir_all(&fake_bin);
}

/// Rust Ralph must launch its tool backend from `ralph_dir`, matching the
/// shell path's `.current_dir(&ralph_dir)` contract and the vendored AGENTS.md
/// instructions that say prd.json/progress.txt/.git are in CWD.
#[tokio::test]
async fn tool_backend_runs_with_ralph_dir_as_cwd() {
    let ralph_dir = mktemp_dir("alps-ralph-test-tool-cwd");
    let script_dir = mktemp_dir("alps-ralph-test-tool-cwd-script");
    let fake_bin = mktemp_dir("alps-ralph-test-tool-cwd-bin");
    let cwd_log = fake_bin.join("observed-cwd.txt");

    write_one_story_passing_prd(&ralph_dir);
    fs::write(
        ralph_dir.join("AGENTS.md"),
        "# Ralph agent instructions (fake, for tests)\n",
    )
    .expect("write AGENTS.md");
    write_fake_codex_recording_cwd(&fake_bin, &cwd_log);

    let result =
        run_ralph_with_fake_codex(ralph_dir.clone(), script_dir.clone(), fake_bin.clone(), 1).await;

    assert!(result.completed, "fixture should complete");
    let observed = fs::read_to_string(&cwd_log).expect("fake codex recorded cwd");
    assert_eq!(
        PathBuf::from(observed.trim()),
        ralph_dir,
        "tool backend must run from ralph_dir so relative prd.json/progress.txt/.git access is stable"
    );

    let _ = fs::remove_dir_all(&ralph_dir);
    let _ = fs::remove_dir_all(&script_dir);
    let _ = fs::remove_dir_all(&fake_bin);
}
