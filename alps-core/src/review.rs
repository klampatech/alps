//! Review agent — Claude Code (adversarial review).
//!
//! Consumes an `Implementation`, produces a `Review` (findings + assertions).
//!
//! MVP: stub that returns a positive Review (the loop needs to advance for the
//! smoke test). Real impl will spawn `claude -p` with an adversarial prompt that
//! asks Claude to find bugs and verify the implementation against the DoD.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent::{Agent, sealed};
use crate::domain::{Assertion, Implementation, Review};

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

/// Review agent — currently a stub that returns a positive Review.
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

    async fn run(&self, input: Implementation) -> Result<Self::Output, Self::Error> {
        // MVP stub: emit a positive Review so the loop can reach the Judge.
        // Real impl will spawn Claude Code with an adversarial prompt:
        //   - "Examine the implementation. Find bugs. Verify each criterion."
        //   - Parse JSON output → Review { findings, assertions }
        let assertion = Assertion {
            criterion: "build_succeeds".to_string(),
            passed: input.commits.len() > 0,
            evidence: format!("{} commits in implementation", input.commits.len()),
        };
        let artifacts_present = input.artifacts.len() > 0;
        let artifacts_assertion = Assertion {
            criterion: "artifacts_present".to_string(),
            passed: artifacts_present,
            evidence: format!("{} artifacts", input.artifacts.len()),
        };

        Ok(Review {
            findings: vec![],
            assertions: vec![assertion, artifacts_assertion],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Artifact, ArtifactKind, Commit};

    #[tokio::test]
    async fn review_stub_returns_positive_for_implementation() {
        let agent = ReviewAgent::default();
        let impl_ = Implementation {
            ralph_branch: "alps/test".to_string(),
            prd_path: PathBuf::from("/tmp/prd.json"),
            commits: vec![Commit {
                sha: "abc123".to_string(),
                message: "feat: test".to_string(),
            }],
            artifacts: vec![Artifact {
                path: PathBuf::from("main.rs"),
                kind: ArtifactKind::Source,
            }],
        };
        let review = agent.run(impl_).await.unwrap();
        assert_eq!(review.assertions.len(), 2);
        assert!(review.assertions.iter().all(|a| a.passed));
    }

    #[test]
    fn review_agent_name() {
        let agent = ReviewAgent::default();
        assert_eq!(agent.name(), "review");
    }

    use std::path::PathBuf;
}
