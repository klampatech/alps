//! ALPS — Agentic Loop Programming System
//!
//! The four-step orchestrator: Plan → Implement → Review → Judge.
//! State machine is encoded in the type system using the type-state pattern.
//!
//! See `SPEC.md` for the full design.

pub mod domain;
pub mod error;
pub mod task;
pub mod agent;
pub mod plan;
pub mod implement;
pub mod review;
pub mod judge;
pub mod receipt;
pub mod persistence;

pub use error::AlpsError;
pub use task::{Task, Idle, Planned, Implemented, Reviewed, Done, Rejected, Failed, Attempt, FailureReason};
pub use domain::*;
pub use agent::Agent;
pub use plan::{PlanAgent, PlanError};
pub use implement::{ImplementAgent, ImplementError};
pub use review::{ReviewAgent, ReviewError};
pub use judge::{JudgeAgent, JudgeError};
pub use receipt::{Receipts, ImplementMetrics, ReviewSummary};
