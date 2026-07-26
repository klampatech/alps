//! Plan agent — Claude Code.
//!
//! Consumes a `Prompt`, produces a `Plan` (granular implementation plan with
//! user stories and definition-of-done criteria).

use async_trait::async_trait;
use thiserror::Error;

use crate::agent::{Agent, sealed};
use crate::domain::{Plan, Prompt};

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("claude code invocation failed: {0}")]
    ClaudeCode(String),

    #[error("failed to parse plan output: {0}")]
    Parse(String),

    #[error("schema validation failed: {0}")]
    Schema(String),
}

/// Default Plan agent — invokes Claude Code via stdin and parses JSON output.
///
/// MVP shell: this is a stub that emits a hard-coded plan. The real
/// implementation will spawn `claude --dangerously-skip-permissions -p`.
pub struct PlanAgent {
    pub model: String,
}

impl PlanAgent {
    pub fn new(model: impl Into<String>) -> Self {
        PlanAgent { model: model.into() }
    }
}

impl sealed::Sealed for PlanAgent {}

#[async_trait]
impl Agent for PlanAgent {
    type Input = Prompt;
    type Output = Plan;
    type Error = PlanError;

    fn name(&self) -> &'static str {
        "plan"
    }

    async fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error> {
        // MVP: stub. Real impl spawns `claude -p` and parses JSON.
        Err(PlanError::ClaudeCode(format!(
            "PlanAgent.run not yet implemented; model={}, prompt={}",
            self.model,
            input.as_str()
        )))
    }
}

// =================== Tests ===================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_agent_name() {
        let agent = PlanAgent::new("claude-sonnet-4");
        assert_eq!(agent.name(), "plan");
    }
}
