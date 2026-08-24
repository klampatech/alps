//! ALPS — Agentic Loop Programming System.
//!
//! The four-step orchestrator: Plan → Implement → Review → Judge.
//! State machine is encoded in the type system using the type-state pattern.
//!
//! See `SPEC.md` for the full design.

// The `elog!` macro (telemetry line-flushing stderr writer, §12 item 10 fix #5)
// is declared in this crate-root position so `macro_rules!` with the local
// `macro_use` semantics makes it visible throughout every module without each
// one needing to `use crate::elog;` (which doesn't work for `macro_rules!`
// macros — Rust requires the full path `$crate::elog!` or `#[macro_export]`
// for that, but `#[macro_export]` lifts the macro to the consumer crate root
// which leaks it into alps-cli's public namespace).
//
// Implementation moved to `telemetry.rs`. This doc-comment block documents the
// design rationale.
//
// ## Why this macro exists
//
// Rust's `std::io::stderr()` returns a line-buffered handle when `isatty()` is
// true (TTY mode) and a fully-buffered handle when redirected to a file/pipe
// (non-TTY mode). When the alps orchestrator exits via `std::process::exit(0)`
// (or returns from `main()` without an explicit flush), the buffered stderr
// contents are dropped on the floor.
//
// This bit us hard during the Tier 4 smoke #4 run (2026-08-04 21:08): the
// orchestrator emitted `[plan] running`, `[implement] running`, `[implement]
// done: 10/10 stories`, `[review] running`, `[judge] running`, `[done]
// accepted` — but the 354 KB stderr log contained only Codex CLI's stderr
// (7000+ lines). Zero orchestrator stderr lines. The deliverable was real, the
// orchestrator exited successfully, but the diagnostic wrapper had nothing to
// show because every `eprintln!` was still in the dropped buffer.
//
// The `elog!` macro replaces `eprintln!` everywhere in the orchestrator. It
// uses `std::io::Write` directly (unbuffered writes via the stderr FD) and
// explicitly flushes after every line. Cost: one extra syscall per line
// (negligible — the orchestrator emits ~10 lines per smoke). Benefit: any
// operator wrapper can now grep for
// `[plan|implement|review|judge|done|rejected]` in the orchestrator's stderr
// and find the exact death point.

// Tokio-using modules are gated to native builds only. Tokio's
// process/IO modules don't compile to `wasm32-unknown-unknown` (the
// underlying syscalls are unix-only), so alps-core on wasm is a
// pure types crate — clients like alps-ui's browser build deserialize
// the serde-derived types without needing the orchestrator logic.
// The `pub use` re-exports below for those modules are also gated.
#[cfg(not(target_arch = "wasm32"))]
pub mod agent;
pub mod agents_md;
pub mod domain;
pub mod error;
pub mod git_ops;
#[cfg(not(target_arch = "wasm32"))]
pub mod implement;
#[cfg(not(target_arch = "wasm32"))]
pub mod judge;
#[cfg(not(target_arch = "wasm32"))]
pub mod loop_;
pub mod persistence;
#[cfg(not(target_arch = "wasm32"))]
pub mod plan;
pub mod summary;
#[cfg(not(target_arch = "wasm32"))]
pub mod ralph;
pub mod receipt;
#[cfg(not(target_arch = "wasm32"))]
pub mod review;
pub mod task;
pub mod telemetry;
pub mod workdir_guard;

#[cfg(not(target_arch = "wasm32"))]
pub use agent::{Agent, EmptyInput, sealed};
pub use domain::*;
pub use error::AlpsError;
#[cfg(not(target_arch = "wasm32"))]
pub use implement::{ImplementAgent, ImplementConfig, ImplementError};
#[cfg(not(target_arch = "wasm32"))]
pub use judge::{
    JudgeAgent, JudgeContext, JudgeError, LlmJudge, StructuredJudge,
    StructuredResult, AlwaysPassStructured, AlwaysPassLlm,
};
#[cfg(not(target_arch = "wasm32"))]
pub use loop_::drive;
pub use persistence::{TaskWorkspace, PersistenceError, Persistable, persist_task};
#[cfg(not(target_arch = "wasm32"))]
pub use plan::{PlanAgent, PlanError};
pub use receipt::{ImplementMetrics, Receipt, Receipts, ReviewSummary};
/// Re-export of the `uuid` crate so downstream consumers (alps-cli,
/// alps-gui) don't need to add their own direct dep just to mint a
/// PlanId for test fixtures. The alps-gui dashboard never mints UUIDs
/// itself, but alps-cli's unit tests do.
pub use uuid;
pub use summary::{TaskDetail, TaskList, TaskNotFound, TaskState, TaskSummary};
#[cfg(not(target_arch = "wasm32"))]
pub use review::{ReviewAgent, ReviewConfig, ReviewContext, ReviewError};
pub use task::{
    Attempt, Done, Failed, FailureReason, Idle, Implemented, Planned, Rejected, Reviewed, Task,
};
