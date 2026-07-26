//! Receipts — the final output of a Done task. What Kyle sees.
//!
//! `Receipts` lives here (not in `domain.rs`) because it's the assembled
//! final artifact, not domain data carried through the loop.

use serde::{Deserialize, Serialize};

use crate::domain::{PlanId, Severity, TaskId};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImplementMetrics {
    pub stories_passed: u32,
    pub stories_total: u32,
    pub iterations: u32,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub findings_count: u32,
    pub critical_findings: u32,
    pub assertions_passed: u32,
    pub assertions_total: u32,
}

impl ReviewSummary {
    pub fn from_findings(
        findings: &[crate::domain::Finding],
        assertions: &[crate::domain::Assertion],
    ) -> Self {
        ReviewSummary {
            findings_count: findings.len() as u32,
            critical_findings: findings
                .iter()
                .filter(|f| matches!(f.severity, Severity::Critical))
                .count() as u32,
            assertions_passed: assertions.iter().filter(|a| a.passed).count() as u32,
            assertions_total: assertions.len() as u32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipts {
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub plan_summary: String,
    pub implement_metrics: ImplementMetrics,
    pub review_summary: ReviewSummary,
    pub judged_at: DateTime<Utc>,
    pub judge_model: String,
}

/// Compact summary receipt — what gets printed to Kyle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub task_id: TaskId,
    pub plan_id: PlanId,
    pub issued_at: DateTime<Utc>,
    pub summary: String,
}

impl Receipt {
    pub fn from_full(receipts: &Receipts, summary: impl Into<String>) -> Self {
        Receipt {
            task_id: receipts.task_id.clone(),
            plan_id: receipts.plan_id.clone(),
            issued_at: receipts.judged_at,
            summary: summary.into(),
        }
    }
}
