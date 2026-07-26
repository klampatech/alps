//! Sealed agent trait.
//!
//! Every agent in the ALPS loop implements `Agent<Input, Output, Error>`.
//! The trait is sealed: only `alps-core` can implement new agents.
//!
//! See `SPEC.md` §5.3 for the full design.

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::marker::PhantomData;

/// Sealed marker — only `alps-core` types can implement `Agent`.
pub mod sealed {
    pub trait Sealed {}
}

/// Input marker — `EmptyInput` is a valid agent input for steps that don't
/// need additional data beyond the task state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct EmptyInput {
    _marker: PhantomData<()>,
}

impl EmptyInput {
    pub fn new() -> Self {
        EmptyInput { _marker: PhantomData }
    }
}

impl Default for EmptyInput {
    fn default() -> Self {
        Self::new()
    }
}

/// The agent trait.
///
/// All four steps in ALPS implement this trait. The associated types
/// `Input`, `Output`, and `Error` give each step strong typing.
#[async_trait]
pub trait Agent: Send + Sync + sealed::Sealed {
    type Input: Serialize + DeserializeOwned + Send + Sync;
    type Output: Serialize + DeserializeOwned + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Stable name for logging and persistence.
    fn name(&self) -> &'static str;

    /// Run the agent with the given input.
    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}
