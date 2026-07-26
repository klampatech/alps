//! Implement agent — Ralph (Codex loop).
//!
//! Consumes a `Plan`, produces an `Implementation`. ALPS treats Ralph as a
//! black-box subprocess: write `prd.json` → invoke `ralph.sh` → read back
//! `prd.json` and `progress.txt`.
//!
//! See `SPEC.md` §2.1 for the compose boundary.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::path::PathBuf;

use crate::agent::{Agent, sealed};
use crate::domain::{Implementation, Plan};

#[derive(Debug, Error)]
pub enum ImplementError {
    #[error("ralph invocation failed: {0}")]
    Ralph(String),

    #[error("ralph exited with error code {code}: {message}")]
    RalphExit { code: i32, message: String },

    #[error("failed to parse prd.json: {0}")]
    PrdParse(String),

    #[error("failed to read progress.txt: {0}")]
    ProgressRead(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementConfig {
    pub ralph_path: PathBuf,
    pub max_iterations: u32,
    pub target_repo: PathBuf,
}

impl Default for ImplementConfig {
    fn default() -> Self {
        ImplementConfig {
            ralph_path: PathBuf::from("./ralph.sh"),
            max_iterations: 20,
            target_repo: PathBuf::from("."),
        }
    }
}

/// Default Implement agent — invokes Ralph as a subprocess.
pub struct ImplementAgent {
    pub config: ImplementConfig,
}

impl ImplementAgent {
    pub fn new(config: ImplementConfig) -> Self {
        ImplementAgent { config }
    }
}

impl Default for ImplementAgent {
    fn default() -> Self {
        ImplementAgent::new(ImplementConfig::default())
    }
}

impl sealed::Sealed for ImplementAgent {}

#[async_trait]
impl Agent for ImplementAgent {
    type Input = Plan;
    type Output = Implementation;
    type Error = ImplementError;

    fn name(&self) -> &'static str {
        "implement"
    }

    async fn run(&self, _input: Self::Input) -> Result<Self::Output, Self::Error> {
        // MVP: stub. Real impl:
        //   1. Serialize plan → prd.json in target_repo
        //   2. Write progress.txt with header
        //   3. Invoke `ralph.sh --tool claude --max-iterations N`
        //   4. Read back prd.json (now with passes:true) and progress.txt
        //   5. Parse git log for commits
        //   6. Return Implementation
        Err(ImplementError::Ralph(format!(
            "ImplementAgent.run not yet implemented; ralph_path={:?}",
            self.config.ralph_path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implement_agent_name() {
        let agent = ImplementAgent::default();
        assert_eq!(agent.name(), "implement");
    }
}
