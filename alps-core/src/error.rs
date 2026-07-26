//! Typed errors for ALPS.

use crate::task::Attempt;
use crate::persistence::PersistenceError;

/// Top-level error for the ALPS orchestrator.
#[derive(Debug, thiserror::Error)]
pub enum AlpsError {
    #[error("plan agent failed: {0}")]
    PlanAgent(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("implement (Ralph) failed: {0}")]
    Implement(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("review agent failed: {0}")]
    ReviewAgent(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("judge failed: {0}")]
    Judge(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("persistence failed: {0}")]
    Persistence(#[source] PersistenceError),

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid task state: {0}")]
    InvalidState(String),

    #[error("max attempts ({max}) exceeded; task aborted")]
    MaxAttemptsExceeded { max: u32, history: Vec<Attempt> },
}
