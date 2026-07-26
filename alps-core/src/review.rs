//! Review agent — Claude Code adversarial review.
//!
//! Consumes an `Implementation`, produces a `Review` (findings + assertions).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent::{Agent, sealed};
use crate::domain::{Implementation, Review};

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("claude code invocation failed: {0}")]
    ClaudeCode(String),

    #[error("failed to parse review output: {0}")]
    Parse(String),

    #[error("schema validation failed: {0}")]
    Schema(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    pub model: String,
    pub adversarial: bool,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig {
            model: "claude-sonnet-4".to_string(),
            adversarial: true,
        }
    }
}

/// Default Review agent — invokes Claude Code via stdin and parses JSON output.
pub struct ReviewAgent {
    pub config: ReviewConfig,
}

impl ReviewAgent {
    pub fn new(config: ReviewConfig) -> Self {
        ReviewAgent { config }
    }
}

impl Default for ReviewAgent {
    fn default() -> Self {
        ReviewAgent::new(ReviewConfig::default())
    }
}

impl sealed::Sealed for ReviewAgent {}

#[async_trait]
impl Agent for ReviewAgent {
    type Input = Implementation;
    type Output = Review;
    type Error = ReviewError;

    fn name(&self) -> &'static str {
        "review"
    }

    async fn run(&self, _input: Self::Input) -> Result<Self::Output, Self::Error> {
        // MVP: stub. Real impl:
        //   1. Build review prompt: impl artifacts + plan + DoD
        //   2. Spawn `claude --dangerously-skip-permissions -p`
        //   3. Parse JSON output → Review { findings, assertions }
        Err(ReviewError::ClaudeCode(format!(
            "ReviewAgent.run not yet implemented; model={}",
            self.config.model
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_agent_name() {
        let agent = ReviewAgent::default();
        assert_eq!(agent.name(), "review");
    }
}
