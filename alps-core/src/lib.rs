//! ALPS — Agentic Loop Programming System
//!
//! The four-step orchestrator: Plan → Implement → Review → Judge.
//! State machine is encoded in the type system using the type-state pattern.
//!
//! See `SPEC.md` for the full design.

pub mod agent;
pub mod agents_md;
pub mod domain;
pub mod error;
pub mod git_ops;
pub mod implement;
pub mod judge;
pub mod loop_;
pub mod persistence;
pub mod plan;
pub mod receipt;
pub mod review;
pub mod task;
pub mod workdir_guard;

pub use agent::{Agent, EmptyInput, sealed};
pub use domain::*;
pub use error::AlpsError;
pub use implement::{ImplementAgent, ImplementConfig, ImplementError};
pub use judge::{
    JudgeAgent, JudgeContext, JudgeError, LlmJudge, StructuredJudge,
    StructuredResult, AlwaysPassStructured, AlwaysPassLlm,
};
pub use loop_::drive;
pub use persistence::{TaskWorkspace, PersistenceError, Persistable, persist_task};
pub use plan::{PlanAgent, PlanError};
pub use receipt::{ImplementMetrics, Receipt, Receipts, ReviewSummary};
pub use review::{ReviewAgent, ReviewConfig, ReviewContext, ReviewError};
pub use task::{
    Attempt, Done, Failed, FailureReason, Idle, Implemented, Planned, Rejected, Reviewed, Task,
};
