//! Judge agent — Hermes.
//!
//! Consumes a `Review` (plus the `Plan` and `Implementation` for verification),
//! produces a `Judgment` (Pass + Receipts, or Reject + Feedback).
//!
//! For MVP the judge is a stub. The real implementation will:
//!   1. Verify each `Assertion` against the actual implementation artifacts
//!   2. Cross-check `DoD` criteria from the plan
//!   3. Emit Receipts (plan summary, implement metrics, review summary)
//!   4. On failure, emit Feedback with retry hints

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent::{Agent, sealed};
use crate::domain::{Judgment, Review};

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("hermes invocation failed: {0}")]
    Hermes(String),

    #[error("artifact verification failed: {0}")]
    Verification(String),

    #[error("failed to parse judgment: {0}")]
    Parse(String),
}

/// Context for the judge — what it needs to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeContext {
    pub review: Review,
    pub plan: crate::domain::Plan,
    pub implementation: crate::domain::Implementation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeConfig {
    pub model: String,
    pub hermes_endpoint: String,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        JudgeConfig {
            model: "hermes".to_string(),
            hermes_endpoint: "http://localhost:8080".to_string(),
        }
    }
}

/// Default Judge agent — invokes Hermes (or local CLI).
pub struct JudgeAgent {
    pub config: JudgeConfig,
}

impl JudgeAgent {
    pub fn new(config: JudgeConfig) -> Self {
        JudgeAgent { config }
    }
}

impl Default for JudgeAgent {
    fn default() -> Self {
        JudgeAgent::new(JudgeConfig::default())
    }
}

impl sealed::Sealed for JudgeAgent {}

#[async_trait]
impl Agent for JudgeAgent {
    type Input = JudgeContext;
    type Output = Judgment;
    type Error = JudgeError;

    fn name(&self) -> &'static str {
        "judge"
    }

    async fn run(&self, _input: Self::Input) -> Result<Self::Output, Self::Error> {
        // MVP: stub. Real impl:
        //   1. Verify each `Assertion::passed` against actual artifacts
        //   2. Cross-check `DoD` criteria from `Plan::dod`
        //   3. If all pass → emit `Judgment::Pass(Receipts)`
        //   4. If any fail → emit `Judgment::Reject(Feedback)`
        Err(JudgeError::Hermes(format!(
            "JudgeAgent.run not yet implemented; model={}, endpoint={}",
            self.config.model, self.config.hermes_endpoint
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_agent_name() {
        let agent = JudgeAgent::default();
        assert_eq!(agent.name(), "judge");
    }
}
