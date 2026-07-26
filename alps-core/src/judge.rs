//! Hybrid Judge — structured DoD + LLM (Hermes).
//!
//! Resolves 2026-07-26: the Judge runs in two stages.
//!
//! 1. **Structured pass**: run all `DefinitionOfDone { verifiable: true }` criteria
//!    deterministically (tests, typecheck, lint, etc.). If any verifiable check
//!    fails, the verdict is **REJECT** with the failed criteria as feedback.
//! 2. **LLM pass**: if all verifiable checks pass, call Hermes (LLM) with the
//!    review findings and the implementation. Hermes can hold the verdict (PASS)
//!    or reject (REJECT) with soft reasoning (code quality, design, etc.).
//!
//! Both stages must clear for PASS. Hermes can reject a structured PASS.
//!
//! For MVP, both stages are stubs. See `AlwaysPassStructured` and `AlwaysPassLlm`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use crate::agent::{Agent, sealed};
use crate::domain::{Assertion, Feedback, Implementation, Judgment, Plan, Review};
use crate::domain::TaskId;
use crate::receipt::{ImplementMetrics, Receipts, ReviewSummary};
use chrono::Utc;

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("structured check failed: {0}")]
    Structured(String),

    #[error("llm check failed: {0}")]
    Llm(String),

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Context for the judge — what it needs to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeContext {
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
}

/// Result of the structured (deterministic) part of the judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredResult {
    pub all_pass: bool,
    pub failed: Vec<Assertion>,
}

/// Stage 1: deterministic checks for verifiable DoD criteria.
#[async_trait]
pub trait StructuredJudge: Send + Sync {
    async fn check(&self, ctx: &JudgeContext) -> Result<StructuredResult, JudgeError>;
}

/// Stage 2: LLM-driven soft judgment.
#[async_trait]
pub trait LlmJudge: Send + Sync {
    async fn judge(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError>;
}

/// The hybrid Judge agent — composes `StructuredJudge` + `LlmJudge`.
pub struct JudgeAgent {
    pub structured: Arc<dyn StructuredJudge>,
    pub llm: Arc<dyn LlmJudge>,
}

impl JudgeAgent {
    pub fn new(structured: Arc<dyn StructuredJudge>, llm: Arc<dyn LlmJudge>) -> Self {
        JudgeAgent { structured, llm }
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

    async fn run(&self, ctx: Self::Input) -> Result<Self::Output, Self::Error> {
        // Stage 1: structured pass
        let s = self.structured.check(&ctx).await
            .map_err(|e| JudgeError::Structured(e.to_string()))?;
        if !s.all_pass {
            return Ok(Judgment::Reject(Feedback {
                reason: "verifiable DoD criteria failed".to_string(),
                failed_assertions: s.failed,
                retry_hints: vec!["fix the failing verifiable checks".to_string()],
            }));
        }

        // Stage 2: LLM pass
        self.llm.judge(&ctx).await
            .map_err(|e| JudgeError::Llm(e.to_string()))
    }
}

// =================== Stubs (for MVP) ===================

/// MVP stub: structured check always passes.
/// Real impl: spawn subprocesses for each verifiable DoD criterion
/// (cargo test, cargo check, cargo clippy, etc.).
pub struct AlwaysPassStructured;

#[async_trait]
impl StructuredJudge for AlwaysPassStructured {
    async fn check(&self, _ctx: &JudgeContext) -> Result<StructuredResult, JudgeError> {
        Ok(StructuredResult { all_pass: true, failed: vec![] })
    }
}

/// MVP stub: LLM check always passes with a minimal receipt.
/// Real impl: invoke Hermes (via subprocess or HTTP) with the review findings.
pub struct AlwaysPassLlm;

#[async_trait]
impl LlmJudge for AlwaysPassLlm {
    async fn judge(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
        let receipts = Receipts {
            task_id: TaskId("stub".to_string()),
            plan_id: ctx.plan.id.clone(),
            plan_summary: ctx.plan.goal.clone(),
            implement_metrics: ImplementMetrics {
                stories_passed: 0,
                stories_total: 0,
                iterations: 0,
                elapsed_secs: 0,
            },
            review_summary: ReviewSummary::from_findings(
                &ctx.review.findings,
                &ctx.review.assertions,
            ),
            judged_at: Utc::now(),
            judge_model: "stub:AlwaysPassLlm".to_string(),
        };
        Ok(Judgment::Pass(receipts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::domain::{
        Assertion, DefinitionOfDone, Finding, Plan, PlanId, Review, Severity, StoryId, UserStory,
    };
    use uuid::Uuid;

    fn dummy_ctx() -> JudgeContext {
        JudgeContext {
            plan: Plan {
                id: PlanId(Uuid::new_v4()),
                goal: "test".to_string(),
                architecture: "test".to_string(),
                stories: vec![UserStory {
                    id: StoryId("US-001".to_string()),
                    title: "test".to_string(),
                    description: "test".to_string(),
                    acceptance_criteria: vec!["test".to_string()],
                    priority: 1,
                }],
                dod: vec![DefinitionOfDone {
                    criterion: "test".to_string(),
                    verifiable: true,
                }],
            },
            implementation: Implementation {
                ralph_branch: "alps/test".to_string(),
                prd_path: PathBuf::from("/tmp/prd.json"),
                commits: vec![],
                artifacts: vec![],
            },
            review: Review {
                findings: vec![Finding {
                    severity: Severity::Info,
                    description: "test".to_string(),
                    evidence: "test".to_string(),
                }],
                assertions: vec![Assertion {
                    criterion: "test".to_string(),
                    passed: true,
                    evidence: "test".to_string(),
                }],
            },
        }
    }

    #[tokio::test]
    async fn hybrid_judge_passes_when_both_stages_pass() {
        let agent = JudgeAgent::new(
            Arc::new(AlwaysPassStructured),
            Arc::new(AlwaysPassLlm),
        );
        let result = agent.run(dummy_ctx()).await.unwrap();
        assert!(matches!(result, Judgment::Pass(_)));
    }

    #[tokio::test]
    async fn hybrid_judge_rejects_when_structured_fails() {
        struct FailStructured;
        #[async_trait]
        impl StructuredJudge for FailStructured {
            async fn check(&self, _ctx: &JudgeContext) -> Result<StructuredResult, JudgeError> {
                Ok(StructuredResult {
                    all_pass: false,
                    failed: vec![Assertion {
                        criterion: "tests_pass".to_string(),
                        passed: false,
                        evidence: "test foo failed".to_string(),
                    }],
                })
            }
        }
        let agent = JudgeAgent::new(
            Arc::new(FailStructured),
            Arc::new(AlwaysPassLlm),
        );
        let result = agent.run(dummy_ctx()).await.unwrap();
        assert!(matches!(result, Judgment::Reject(_)));
    }

    use std::path::PathBuf;
}
