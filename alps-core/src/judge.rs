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
//! MVP stubs: `AlwaysPassStructured` (always passes), `AlwaysPassLlm` (always
//! returns minimal Receipts). Real impls: see `HermesLlmJudge` (real LLM call).

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent::{Agent, sealed};
use crate::domain::{
    Assertion, Feedback, Finding, Implementation, Judgment, Plan, Review, Severity, TaskId,
};
use crate::elog;
use crate::receipt::{ImplementMetrics, Receipts, ReviewSummary};

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("structured check failed: {0}")]
    Structured(String),

    #[error("llm check failed: {0}")]
    Llm(String),

    /// LLM emitted invalid JSON. Retried up to `max_retries` in
    /// `HermesLlmJudge` — only this variant triggers a retry.
    #[error("failed to parse llm output: {0}")]
    Parse(String),

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Context for the judge — what it needs to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeContext {
    pub task_id: TaskId,
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    /// Codebase patterns from the implement step (via AGENTS.md propagation).
    /// Empty string if no patterns have been discovered yet.
    pub agents_md: String,
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
impl crate::agent::Agent for JudgeAgent {
    type Input = JudgeContext;
    type Output = Judgment;
    type Error = JudgeError;

    fn name(&self) -> &'static str {
        "judge"
    }

    async fn run(&self, ctx: JudgeContext) -> Result<Self::Output, Self::Error> {
        // Stage 1: structured pass
        let s = self
            .structured
            .check(&ctx)
            .await
            .map_err(|e| JudgeError::Structured(e.to_string()))?;
        if !s.all_pass {
            return Ok(Judgment::Reject(Feedback {
                reason: "verifiable DoD criteria failed".to_string(),
                failed_assertions: s.failed,
                retry_hints: vec!["fix the failing verifiable checks".to_string()],
            }));
        }

        // Stage 2: LLM pass
        self.llm
            .judge(&ctx)
            .await
            .map_err(|e| JudgeError::Llm(e.to_string()))
    }
}

// =================== LLM Judge (real) ===================

/// Config for the LLM judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmJudgeConfig {
    /// Path to the LLM CLI (default: `claude`). Configurable for swapping
    /// in alternative backends (e.g. a `hermes` CLI if/when one exists).
    pub cli_path: String,
    /// Model identifier. Default is the Claude Code `opus` alias (which on
    /// this host routes to MiniMax-M3 — Kyle's higher-quality model
    /// dedicated for the judgment slot). Sub-agents (Plan + Review) stay on
    /// `claude-sonnet-4` (MiniMax-M2.7) for cheaper/longer-prompt work.
    pub model: String,
    /// Skip files larger than this (bytes).
    pub max_file_bytes: usize,
    /// Skip remaining files once we exceed this total.
    pub max_total_bytes: usize,
    /// Maximum number of total attempts when the LLM emits invalid JSON.
    /// `1` = no retry (just the original attempt), `3` = 1 original + 2
    /// retries (default). Only `JudgeError::Parse` triggers a retry;
    /// spawn errors, schema validation errors, and unknown-verdict errors
    /// propagate immediately.
    #[serde(default = "default_llm_max_retries")]
    pub max_retries: u32,
    /// Per-attempt wall-clock timeout for the spawned Judge subprocess
    /// (`claude --dangerously-skip-permissions -p --model …`). If the
    /// subprocess doesn't exit within this many seconds, the Judge call
    /// returns `JudgeError::Llm` with a timeout message and the
    /// orchestrator's outer retry loop catches it.
    ///
    /// **Why this matters (SPEC §12 item 8):** before this field existed,
    /// the Judge subprocess had no timeout. On a heavy Judge prompt
    /// (large `files` section, long Review findings), Claude Code Opus
    /// could exceed any reasonable wall-clock expectation, leaving
    /// `judge.run(ctx).await` awaiting forever. The orchestrator would
    /// then be killed by upstream SIGPIPE (from the wrapping agent's
    /// tee/pipe), causing receipts.json + .alps-last-done to never land
    /// even though the deliverable was real. The 600s default is
    /// generous enough for Opus on realistic Judge prompts (~50KB) and
    /// tight enough that a hung subprocess doesn't block the smoke
    /// indefinitely.
    #[serde(default = "default_judge_timeout_secs")]
    pub judge_timeout_secs: u64,
}

fn default_llm_max_retries() -> u32 {
    3
}

fn default_judge_timeout_secs() -> u64 {
    600
}

impl Default for LlmJudgeConfig {
    fn default() -> Self {
        LlmJudgeConfig {
            cli_path: "claude".to_string(),
            model: "claude-opus-4".to_string(),
            max_file_bytes: 50_000,
            max_total_bytes: 500_000,
            max_retries: default_llm_max_retries(),
            judge_timeout_secs: default_judge_timeout_secs(),
        }
    }
}

/// Real LLM Judge — invokes Claude Code with a judge-specific system prompt.
/// The system prompt is decisive: pass if DoD is met, reject with specific
/// feedback if not.
pub struct HermesLlmJudge {
    pub config: LlmJudgeConfig,
    /// Test-only override: when set, `judge()` calls this closure instead
    /// of spawning Claude Code. The closure is called PER ATTEMPT (i.e.,
    /// once per retry). Use `Arc<AtomicUsize>` inside the closure if you
    /// need to return different responses across attempts.
    #[cfg(test)]
    pub(crate) test_handler: Option<
        std::sync::Arc<
            dyn Fn(&JudgeContext) -> Result<Judgment, JudgeError> + Send + Sync,
        >,
    >,
}

impl HermesLlmJudge {
    pub fn new(config: LlmJudgeConfig) -> Self {
        HermesLlmJudge {
            config,
            #[cfg(test)]
            test_handler: None,
        }
    }

    pub fn with_model(model: impl Into<String>) -> Self {
        HermesLlmJudge {
            config: LlmJudgeConfig {
                model: model.into(),
                ..Default::default()
            },
            #[cfg(test)]
            test_handler: None,
        }
    }

    /// Test-only constructor that bypasses Claude Code. The closure
    /// receives the judge context and returns a canned (or computed)
    /// `Judgment`. Called PER ATTEMPT — use `Arc<AtomicUsize>` to return
    /// different responses across attempts.
    #[cfg(test)]
    pub fn for_test<F>(f: F) -> Self
    where
        F: Fn(&JudgeContext) -> Result<Judgment, JudgeError> + Send + Sync + 'static,
    {
        HermesLlmJudge {
            config: LlmJudgeConfig::default(),
            test_handler: Some(std::sync::Arc::new(f)),
        }
    }
}

impl Default for HermesLlmJudge {
    fn default() -> Self {
        HermesLlmJudge::new(LlmJudgeConfig::default())
    }
}

#[async_trait]
impl LlmJudge for HermesLlmJudge {
    async fn judge(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
        // Retry loop. Claude Code occasionally emits invalid JSON (parse
        // error). On `JudgeError::Parse`, retry up to `config.max_retries`
        // total attempts. Spawn errors, schema validation errors, and
        // unknown-verdict errors propagate immediately — retrying won't fix
        // those (they're either deterministic for the same input, or the
        // JSON parsed fine and the issue is semantic).
        //
        // See `judge_retries_on_parse_failure` in this file for the exact
        // contract: per-attempt calls of the test_handler with monotonic
        // attempt numbering, only `Parse` errors retried.
        let max_attempts = self.config.max_retries.max(1) as usize;
        let mut last_err: Option<JudgeError> = None;
        for attempt in 1..=max_attempts {
            match self.judge_once(ctx).await {
                Ok(j) => return Ok(j),
                Err(JudgeError::Parse(msg)) => {
                    elog!(
                        "[judge] parse failed (attempt {}/{}): {}",
                        attempt, max_attempts, msg
                    );
                    last_err = Some(JudgeError::Parse(msg));
                }
                Err(other) => return Err(other),
            }
        }
        Err(JudgeError::Parse(format!(
            "failed after {} attempts: {}",
            max_attempts,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
}

// ─────────────────────────────────────────────────────────────
// Inherent methods (not on the LlmJudge trait)
//
// `judge_once` is called by the trait's `judge()` method (above) per
// retry. It MUST live in an `impl HermesLlmJudge` block, not in the
// `impl LlmJudge for HermesLlmJudge` block, because Rust resolves
// `self.judge_once(...)` to the trait first — and the `LlmJudge` trait
// doesn't have a `judge_once` method. Putting it here makes it an
// inherent method on `HermesLlmJudge`, which is what we want.

impl HermesLlmJudge {
    /// One attempt of the LLM judge. Called by `judge()` per retry.
    /// Either invokes the test_handler (cfg(test) only) or spawns Claude.
    async fn judge_once(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
        // Test-only fast path: if a test_handler is set, use it instead of
        // spawning Claude. The closure is called PER ATTEMPT (i.e., once
        // per retry).
        #[cfg(test)]
        if let Some(f) = &self.test_handler {
            return f(ctx);
        }

        // 1. Read files from ralph_dir for verification context
        let files = read_files(&ctx.implementation, &self.config)?;

        // 2. Build the judge prompt
        let prompt = build_judge_prompt(ctx, &files);

        // 3. Spawn Claude Code via stdin
        let mut child = Command::new(&self.config.cli_path)
            .args([
                "--dangerously-skip-permissions",
                "-p",
                "--model",
                &self.config.model,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| JudgeError::Llm(format!("spawn failed: {}", e)))?;

        // Capture the child PID up-front so we can kill it if the timeout
        // fires (otherwise `child.wait_with_output()` consumes `child` and we
        // have no handle left to kill).
        let child_pid = child.id();

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| JudgeError::Llm("no stdin handle".to_string()))?;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| JudgeError::Llm(format!("stdin write failed: {}", e)))?;
        drop(stdin);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.judge_timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            // Timeout fired. Best-effort kill by PID so the subprocess doesn't
            // linger holding stdout/stderr handles. If the PID is None (already
            // reaped) or the kill itself fails, we still return the timeout
            // error because that's what the caller asked for.
            if let Some(pid) = child_pid {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
            JudgeError::Llm(format!(
                "judge subprocess timed out after {}s (model={})",
                self.config.judge_timeout_secs, self.config.model
            ))
        })?
        .map_err(|e| JudgeError::Llm(format!("wait failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(JudgeError::Llm(format!(
                "exit {:?}: {}",
                output.status.code(),
                stderr.chars().take(2000).collect::<String>()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let json_str = strip_markdown_fences(&stdout);

        // JSON parse errors → JudgeError::Parse (triggers retry)
        let parsed: ParsedVerdict = serde_json::from_str(json_str).map_err(|e| {
            JudgeError::Parse(format!(
                "{}: {}",
                e,
                json_str.chars().take(500).collect::<String>()
            ))
        })?;

        // Schema/semantic errors → JudgeError::Llm (no retry — JSON parsed
        // fine, the issue is in the contents).
        validate_verdict(&parsed)?;

        // 4. Build the Judgment
        match parsed.verdict.to_lowercase().as_str() {
            "pass" => Ok(Judgment::Pass(build_receipts(ctx, &self.config.model))),
            "reject" => Ok(Judgment::Reject(Feedback {
                reason: parsed.reason,
                failed_assertions: parsed
                    .failed_assertions
                    .into_iter()
                    .map(|a| Assertion {
                        criterion: a.criterion,
                        passed: false,
                        evidence: a.evidence,
                    })
                    .collect(),
                retry_hints: parsed.retry_hints,
            })),
            other => Err(JudgeError::Llm(format!(
                "unknown verdict: '{}' (expected 'pass' or 'reject')",
                other
            ))),
        }
    }
}

// ─────────────────────────────────────────────────────────────
// File reading
// ─────────────────────────────────────────────────────────────

fn read_files(
    impl_: &Implementation,
    config: &LlmJudgeConfig,
) -> Result<Vec<(String, String)>, JudgeError> {
    // Walk the deliverable tree, not the ralph nested workspace. When the
    // CLI sets `--deliverable-path`, this points at the user's actual
    // target (e.g. `/tmp/foo/`); otherwise it falls back to ralph_dir
    // (the legacy behavior). See SPEC §12 item 2.
    let artifacts_root = if impl_.deliverable_path.as_os_str().is_empty() {
        impl_.prd_path.parent().ok_or_else(|| {
            JudgeError::Llm(format!("prd_path has no parent: {:?}", impl_.prd_path))
        })?
    } else {
        impl_.deliverable_path.as_path()
    };

    let mut files = Vec::new();
    let mut total = 0usize;

    for artifact in &impl_.artifacts {
        if total >= config.max_total_bytes {
            break;
        }
        let path = artifacts_root.join(&artifact.path);
        if !path.is_file() {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.len() > config.max_file_bytes {
            continue;
        }
        total += bytes.len();
        let content = String::from_utf8_lossy(&bytes).to_string();
        files.push((artifact.path.display().to_string(), content));
    }

    Ok(files)
}

// ─────────────────────────────────────────────────────────────
// Prompt building
// ─────────────────────────────────────────────────────────────

pub(crate) fn build_judge_prompt(ctx: &JudgeContext, files: &[(String, String)]) -> String {
    let plan = &ctx.plan;
    let impl_ = &ctx.implementation;
    let review = &ctx.review;

    let mut dod = String::new();
    for d in &plan.dod {
        let mark = if d.verifiable { "[verifiable]" } else { "[soft]" };
        dod.push_str(&format!("  - {} {}\n", mark, d.criterion));
    }

    let mut findings = String::new();
    for f in &review.findings {
        findings.push_str(&format!(
            "  - **{:?}**: {} (evidence: {})\n",
            f.severity, f.description, f.evidence
        ));
    }
    if findings.is_empty() {
        findings.push_str("  (no findings)\n");
    }

    let mut assertions = String::new();
    let (passed, total) = review
        .assertions
        .iter()
        .fold((0, 0), |(p, t), a| (p + a.passed as u32, t + 1));
    for a in &review.assertions {
        assertions.push_str(&format!(
            "  - [{}] {} (evidence: {})\n",
            if a.passed { "x" } else { " " },
            a.criterion,
            a.evidence
        ));
    }

    let mut file_section = String::new();
    if files.is_empty() {
        file_section.push_str("(no files to inspect)\n");
    } else {
        for (path, content) in files {
            file_section.push_str(&format!("\n### `{}`\n\n```{}\n{}\n```\n", path, lang_for(path), content));
        }
    }

    let mut agents_md_section = String::new();
    if !ctx.agents_md.trim().is_empty() {
        agents_md_section.push_str(&format!(
            "\n## Codebase Patterns (from prior implement)\n\n{}\n",
            ctx.agents_md
        ));
    }

    let trailing = if agents_md_section.is_empty() { "" } else { "\n" };

    format!(
        "{}\n\n---\n\n## Plan\n\n**Goal:** {}\n\n**DoD criteria:**\n{}\n\n## Implementation\n\n**Branch:** `{}`
**Commits ({}):**\n{}

## Review\n\n**Findings ({}):**\n{}
**Assertions ({}/{} passed):**\n{}

## Source files
{}
{}{}

---

Decide. Be decisive. Output JSON.",
        JUDGE_SYSTEM_PROMPT,
        plan.goal,
        dod,
        impl_.ralph_branch,
        impl_.commits.len(),
        impl_.commits.iter().map(|c| format!("  - `{}` {}", &c.sha[..7.min(c.sha.len())], c.message)).collect::<Vec<_>>().join("\n"),
        review.findings.len(),
        findings,
        passed, total,
        assertions,
        file_section,
        agents_md_section,
        trailing,
    )
}

fn lang_for(path: &str) -> &'static str {
    if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".js") || path.ends_with(".ts") {
        "typescript"
    } else if path.ends_with(".go") {
        "go"
    } else if path.ends_with(".md") {
        "markdown"
    } else if path.ends_with(".toml") {
        "toml"
    } else if path.ends_with(".json") {
        "json"
    } else {
        ""
    }
}

// ─────────────────────────────────────────────────────────────
// JSON parsing
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ParsedVerdict {
    verdict: String,
    reason: String,
    #[serde(default)]
    failed_assertions: Vec<ParsedFailedAssertion>,
    #[serde(default)]
    retry_hints: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ParsedFailedAssertion {
    criterion: String,
    evidence: String,
}

impl ParsedVerdict {
    fn into_judgment(self, ctx: &JudgeContext, judge_model: &str) -> Judgment {
        match self.verdict.to_lowercase().as_str() {
            "pass" => Judgment::Pass(build_receipts(ctx, judge_model)),
            _ => Judgment::Reject(Feedback {
                reason: self.reason,
                failed_assertions: self
                    .failed_assertions
                    .into_iter()
                    .map(|a| Assertion {
                        criterion: a.criterion,
                        passed: false,
                        evidence: a.evidence,
                    })
                    .collect(),
                retry_hints: self.retry_hints,
            }),
        }
    }
}

fn validate_verdict(p: &ParsedVerdict) -> Result<(), JudgeError> {
    let v = p.verdict.to_lowercase();
    if v != "pass" && v != "reject" {
        return Err(JudgeError::Llm(format!(
            "verdict must be 'pass' or 'reject', got '{}'",
            p.verdict
        )));
    }
    if p.reason.trim().is_empty() {
        return Err(JudgeError::Llm("reason is empty".to_string()));
    }
    if v == "reject" && p.failed_assertions.is_empty() {
        return Err(JudgeError::Llm(
            "reject requires at least one failed_assertion".to_string(),
        ));
    }
    Ok(())
}

fn build_receipts(ctx: &JudgeContext, judge_model: &str) -> Receipts {
    // The implementation already carried the real metrics through (Ralph
    // run → .ralph-result.json → implement.rs → Implementation.metrics).
    // Don't recompute from the plan — that loses Ralph's iteration count
    // and elapsed time.
    let metrics = ctx.implementation.metrics.clone();
    let summary = ReviewSummary::from_findings(&ctx.review.findings, &ctx.review.assertions);

    Receipts {
        task_id: ctx.task_id.clone(),
        plan_id: ctx.plan.id.clone(),
        plan_summary: ctx.plan.goal.clone(),
        implement_metrics: metrics,
        review_summary: summary,
        judged_at: Utc::now(),
        judge_model: judge_model.to_string(),
    }
}

fn strip_markdown_fences(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        if let Some(after) = rest.find('\n') {
            let body = &rest[after + 1..];
            if let Some(stripped) = body.trim_end().strip_suffix("```") {
                return stripped.trim();
            }
        }
    }
    if let Some(rest) = s.strip_prefix("```") {
        if let Some(after) = rest.find('\n') {
            let body = &rest[after + 1..];
            if let Some(stripped) = body.trim_end().strip_suffix("```") {
                return stripped.trim();
            }
        }
    }
    s
}

// ─────────────────────────────────────────────────────────────
// System prompt — decisive, not a yes-bot
// ─────────────────────────────────────────────────────────────

const JUDGE_SYSTEM_PROMPT: &str = r#"You are the ALPS Judge — the final arbiter. Your job is to accept or reject the implementation based on whether the Definition of Done is actually met.

You are NOT a yes-bot. You are decisive. Don't waffle.

CRITICAL OUTPUT RULES:
- Your response MUST be a single JSON object.
- Start your response with `{` and end with `}`.
- Do NOT write any prose, commentary, "I have all the data", markdown fences, or preamble.
- Do NOT call any tools (no Read, no Bash, no Grep). All needed data is in the prompt.
- Do NOT use TodoWrite or any task-tracking tools.

Given:
- The Plan (goal, DoD criteria)
- The Implementation (commits, source files)
- The Review (findings, assertions)

Decide PASS if ALL of these are true:
- Every `[verifiable]` DoD criterion is satisfied (or has been verified by the Review)
- No `Critical` findings indicate a real bug, security flaw, or data loss
- The implementation is complete — not abandoned, not partial
- If tests exist, they pass
- The code is functional for the stated goal

Decide REJECT if ANY of these are true:
- A `Critical` finding indicates a real bug or security issue
- A `[verifiable]` DoD criterion clearly fails
- The implementation is incomplete or abandoned
- Tests fail
- The code does not satisfy the stated goal

On REJECT, you MUST provide:
- `reason`: short explanation (1-2 sentences)
- `failed_assertions`: specific failures with evidence
- `retry_hints`: actionable suggestions for the next iteration

Output ONLY valid JSON:
{
  "verdict": "pass" | "reject",
  "reason": "string — short explanation of decision",
  "failed_assertions": [
    {
      "criterion": "string — what failed",
      "evidence": "string — proof of failure"
    }
  ],
  "retry_hints": ["string — actionable suggestion", ...]
}

REMINDER: First character of your response is `{`. Last character is `}`. No other characters outside the JSON object."#;

// =================== DoD Runner (real) ===================

/// Config for the DoD runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoDRunnerConfig {
    /// Auto-detect project type from files in the ralph dir.
    pub auto_detect: bool,
    /// Per-command timeout in seconds.
    pub timeout_secs: u64,
    /// Skip all verification (useful for testing).
    pub skip_verification: bool,
}

impl Default for DoDRunnerConfig {
    fn default() -> Self {
        DoDRunnerConfig {
            auto_detect: true,
            timeout_secs: 120,
            skip_verification: false,
        }
    }
}

/// Detected project type — drives the test command selection.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ProjectType {
    Rust,
    Python,
    Node,
    Go,
    Unknown,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Rust => write!(f, "rust"),
            ProjectType::Python => write!(f, "python"),
            ProjectType::Node => write!(f, "node"),
            ProjectType::Go => write!(f, "go"),
            ProjectType::Unknown => write!(f, "unknown"),
        }
    }
}

/// Real StructuredJudge — runs verifiable DoD checks by spawning the
/// project's test command (cargo test / pytest / npm test / go test).
pub struct DoDRunner {
    pub config: DoDRunnerConfig,
}

impl DoDRunner {
    pub fn new() -> Self {
        DoDRunner { config: DoDRunnerConfig::default() }
    }

    pub fn with_config(config: DoDRunnerConfig) -> Self {
        DoDRunner { config }
    }
}

impl Default for DoDRunner {
    fn default() -> Self {
        DoDRunner::new()
    }
}

#[async_trait]
impl StructuredJudge for DoDRunner {
    async fn check(&self, ctx: &JudgeContext) -> Result<StructuredResult, JudgeError> {
        if self.config.skip_verification {
            return Ok(StructuredResult { all_pass: true, failed: vec![] });
        }

        let ralph_dir = ctx.implementation.prd_path.parent().ok_or(
            JudgeError::Structured(format!(
                "prd_path has no parent: {:?}",
                ctx.implementation.prd_path
            )),
        )?;

        if !self.config.auto_detect {
            return Ok(StructuredResult { all_pass: true, failed: vec![] });
        }

        // Walk the deliverable tree, not the ralph nested workspace. When
        // the CLI sets `--deliverable-path`, this points at the user's
        // actual target (e.g. `/tmp/foo/`); otherwise it falls back to
        // `ralph_dir` (the legacy behavior). See SPEC §12 item 7 — closes
        // the gap surfaced by the 2026-08-01 Node smoke (Runtime Pitfall
        // #18 in the alps skill).
        let detect_root = if ctx.implementation.deliverable_path.as_os_str().is_empty() {
            ralph_dir
        } else {
            ctx.implementation.deliverable_path.as_path()
        };

        let (project_type, test_root) = detect_project_type(detect_root);
        elog!(
            "[judge:structured] detected project type: {} (test_root: {})",
            project_type,
            test_root.display()
        );

        if matches!(project_type, ProjectType::Unknown) {
            elog!("[judge:structured] no project type detected, skipping DoD checks");
            return Ok(StructuredResult { all_pass: true, failed: vec![] });
        }

        let (cmd, args) = test_command_for(&project_type);
        elog!("[judge:structured] running: {} {}", cmd, args.join(" "));

        // Run from `test_root` (the dir where the project's marker file
        // lives), NOT from `detect_root` (the deliverable root). For
        // monorepo layouts (Tier-4: `backend/pyproject.toml` +
        // `frontend/package.json`), `test_root = backend/` — pytest runs
        // from there, picks up the venv at `backend/.venv/`, and reports
        // 10 passed instead of failing with `ModuleNotFoundError:
        // sqlalchemy`. Surfaced by Tier-4 smoke 2026-08-10
        // (smoke #22-tier4): the previous code ran pytest from the
        // deliverable root and got exit Some(2) on every Tier-4 monorepo
        // smoke, regardless of whether the tests actually passed.
        let result = run_cmd_with_timeout(&test_root, cmd, &args, self.config.timeout_secs).await?;

        if result.success {
            elog!("[judge:structured] PASS");
            Ok(StructuredResult { all_pass: true, failed: vec![] })
        } else {
            elog!(
                "[judge:structured] FAIL (exit {:?})",
                result.exit_code
            );
            Ok(StructuredResult {
                all_pass: false,
                failed: vec![Assertion {
                    criterion: format!("DoD check: {} {}", cmd, args.join(" ")),
                    passed: false,
                    evidence: format!(
                        "exit {:?}
--- stderr ---
{}
--- end ---",
                        result.exit_code,
                        result.stderr.chars().take(1000).collect::<String>()
                    ),
                }],
            })
        }
    }
}

/// Subdirs that should never be walked for project detection — they
/// contain vendored dependencies, build artifacts, or version-control
/// metadata that would produce false-positive classifications or burn
/// cycles on large trees.
const SKIP_DETECT_SUBDIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
    ".cache",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

/// True if the dir's root-level entries contain Rust / Python / Node / Go
/// markers — used by `detect_project_type` to short-circuit before
/// recursing into monorepo subdirs. Kept in a helper so the recursion
/// helper below can call it for both the root and each child.
fn has_project_marker(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
        || dir.join("Cargo.lock").exists()
        || dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("pytest.ini").exists()
        || dir.join("package.json").exists()
        || dir.join("go.mod").exists()
        || has_py_tests(dir)
}

/// Classify the project rooted at `dir` by walking the dir itself plus
/// Classify the project at `dir` AND return the directory the marker was
/// found in. The two-tuple return is load-bearing for monorepo layouts
/// (Tier-4: `backend/{pyproject.toml,...}` + `frontend/{package.json,...}`):
/// the `ProjectType` tells us which test command to run, and the `PathBuf`
/// tells us **where** to run it from. Running `python3 -m pytest -q` from
/// the deliverable root in a monorepo case fails — pytest recurses into
/// `backend/tests/`, hits `from sqlalchemy...`, and errors with
/// `ModuleNotFoundError` because there's no venv at the deliverable root.
/// Surfaced by Tier-4 smoke 2026-08-10 (smoke #22-tier4): Judge's
/// structured stage returned exit Some(2) while codex's runtime pytest
/// (from `backend/` with the venv) reported 10 passed. This function's
/// second return value lets the Judge run the test command from the
/// directory where the project's actual config lives.
///
/// **Priority order** (matters for ALPS running against itself):
/// 1. Root-level markers win over nested markers — if `Cargo.toml` sits
///    at the deliverable root, the project is Rust even if there's a
///    `frontend/package.json` somewhere underneath.
/// 2. Among monorepo subdirs, the **first** subdir (alphabetical, by
///    `read_dir` order on Linux ext4) to match wins. This is
///    deterministic on a given filesystem; Tier-4 smokes that care
///    about a specific sub-project should set `--deliverable-path` to
///    point at that subdir directly.
fn detect_project_type(dir: &Path) -> (ProjectType, PathBuf) {
    if dir.join("Cargo.toml").exists() || dir.join("Cargo.lock").exists() {
        return (ProjectType::Rust, dir.to_path_buf());
    }
    if dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("pytest.ini").exists()
        || dir.join("pyproject.toml").exists()
        || has_py_tests(dir)
    {
        return (ProjectType::Python, dir.to_path_buf());
    }
    if dir.join("package.json").exists() {
        return (ProjectType::Node, dir.to_path_buf());
    }
    if dir.join("go.mod").exists() {
        return (ProjectType::Go, dir.to_path_buf());
    }

    // Root had no marker file. For monorepo layouts (Tier-4 deliverable
    // is `backend/{pyproject.toml,...}` + `frontend/{package.json,...}`),
    // walk one level of immediate subdirs and try to classify. Skip
    // vendor / build dirs to keep the walk bounded and deterministic.
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (ProjectType::Unknown, dir.to_path_buf()),
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if SKIP_DETECT_SUBDIRS.contains(&name) {
            continue;
        }
        children.push(path);
    }
    // Sort for deterministic ordering across filesystems — `read_dir`
    // doesn't guarantee order on every platform, and the test suite
    // needs stable results.
    children.sort();
    for child in &children {
        if !has_project_marker(child) {
            continue;
        }
        // Recurse into the matching child and return whatever it found.
        // The recursive call returns the matched child path itself, so
        // the Judge's test-command cwd is the right place.
        return detect_project_type(child);
    }
    (ProjectType::Unknown, dir.to_path_buf())
}

fn has_py_tests(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .any(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("test_") && name.ends_with(".py")
                })
        })
        .unwrap_or(false)
}

fn test_command_for(project_type: &ProjectType) -> (&'static str, Vec<&'static str>) {
    match project_type {
        ProjectType::Rust => ("cargo", vec!["test", "--quiet"]),
        ProjectType::Python => ("python3", vec!["-m", "pytest", "-q"]),
        ProjectType::Node => ("npm", vec!["test", "--silent"]),
        ProjectType::Go => ("go", vec!["test", "./..."]),
        ProjectType::Unknown => ("", vec![]),
    }
}

struct CmdResult {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

async fn run_cmd_with_timeout(
    dir: &Path,
    cmd: &str,
    args: &[&str],
    timeout_secs: u64,
) -> Result<CmdResult, JudgeError> {
    if cmd.is_empty() {
        return Ok(CmdResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        });
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new(cmd).args(args).current_dir(dir).output(),
    )
    .await
    .map_err(|_| {
        JudgeError::Structured(format!(
            "timeout after {}s running '{} {}'",
            timeout_secs,
            cmd,
            args.join(" ")
        ))
    })?
    .map_err(|e| JudgeError::Structured(format!("spawn failed: {}", e)))?;

    Ok(CmdResult {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

// =================== Stubs (for MVP / testing) ===================

/// Stub that always passes. Kept for tests + as a fallback option.
pub struct AlwaysPassStructured;

#[async_trait]
impl StructuredJudge for AlwaysPassStructured {
    async fn check(&self, _ctx: &JudgeContext) -> Result<StructuredResult, JudgeError> {
        Ok(StructuredResult {
            all_pass: true,
            failed: vec![],
        })
    }
}

/// MVP stub: LLM check always passes. Real impl is `HermesLlmJudge`.
pub struct AlwaysPassLlm;

#[async_trait]
impl LlmJudge for AlwaysPassLlm {
    async fn judge(&self, ctx: &JudgeContext) -> Result<Judgment, JudgeError> {
        let receipts = Receipts {
            task_id: ctx.task_id.clone(),
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

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DefinitionOfDone, Finding, PlanId, StoryId, UserStory};
    use crate::domain::{Artifact, ArtifactKind, Commit};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use uuid::Uuid;

    fn dummy_ctx() -> JudgeContext {
        JudgeContext {
            task_id: TaskId::new(),
            plan: Plan {
                id: PlanId(Uuid::new_v4()),
                goal: "build a fib function".to_string(),
                architecture: "Python stdlib".to_string(),
                stories: vec![UserStory {
                    id: StoryId("US-001".to_string()),
                    title: "fib".to_string(),
                    description: "fib function".to_string(),
                    acceptance_criteria: vec!["fib(10) = [0,1,1,2,3,5,8,13,21,34]".to_string()],
                    priority: 1,
                }],
                dod: vec![
                    DefinitionOfDone { criterion: "tests pass".to_string(), verifiable: true },
                ],
            },
            implementation: Implementation {
                ralph_branch: "alps/test".to_string(),
                prd_path: PathBuf::from("/tmp/prd.json"),
                commits: vec![Commit {
                    sha: "abc1234".to_string(),
                    message: "feat: fib".to_string(),
                }],
                artifacts: vec![Artifact {
                    path: PathBuf::from("fib.py"),
                    kind: ArtifactKind::Source,
                }],
                metrics: Default::default(),
                deliverable_path: PathBuf::from("/tmp/alps-judge-fixture"),
            },
            review: Review {
                findings: vec![],
                assertions: vec![Assertion {
                    criterion: "tests pass".to_string(),
                    passed: true,
                    evidence: "1 passed".to_string(),
                }],
            },
            agents_md: String::new(),
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

    #[test]
    fn strip_markdown_fences_json() {
        let s = "```json\n{}\n```";
        assert_eq!(strip_markdown_fences(s), "{}");
    }

    #[test]
    fn validate_verdict_pass() {
        let p = ParsedVerdict {
            verdict: "pass".to_string(),
            reason: "all good".to_string(),
            failed_assertions: vec![],
            retry_hints: vec![],
        };
        assert!(validate_verdict(&p).is_ok());
    }

    #[test]
    fn validate_verdict_reject_requires_failed_assertions() {
        let p = ParsedVerdict {
            verdict: "reject".to_string(),
            reason: "tests fail".to_string(),
            failed_assertions: vec![],
            retry_hints: vec![],
        };
        assert!(validate_verdict(&p).is_err());
    }

    #[test]
    fn validate_verdict_empty_reason() {
        let p = ParsedVerdict {
            verdict: "pass".to_string(),
            reason: "".to_string(),
            failed_assertions: vec![],
            retry_hints: vec![],
        };
        assert!(validate_verdict(&p).is_err());
    }

    #[test]
    fn validate_verdict_unknown() {
        let p = ParsedVerdict {
            verdict: "maybe".to_string(),
            reason: "test".to_string(),
            failed_assertions: vec![],
            retry_hints: vec![],
        };
        assert!(validate_verdict(&p).is_err());
    }

    #[test]
    fn parsed_verdict_into_judgment_pass() {
        let p = ParsedVerdict {
            verdict: "pass".to_string(),
            reason: "all good".to_string(),
            failed_assertions: vec![],
            retry_hints: vec![],
        };
        let j = p.into_judgment(&dummy_ctx(), "test-model");
        assert!(matches!(j, Judgment::Pass(_)));
    }

    #[test]
    fn parsed_verdict_into_judgment_reject() {
        let p = ParsedVerdict {
            verdict: "reject".to_string(),
            reason: "tests fail".to_string(),
            failed_assertions: vec![ParsedFailedAssertion {
                criterion: "test_x".to_string(),
                evidence: "expected 1 got 2".to_string(),
            }],
            retry_hints: vec!["fix the test".to_string()],
        };
        let j = p.into_judgment(&dummy_ctx(), "test-model");
        assert!(matches!(j, Judgment::Reject(_)));
    }

    #[test]
    fn build_receipts_includes_task_id() {
        let ctx = dummy_ctx();
        let expected_id = ctx.task_id.clone();
        let r = build_receipts(&ctx, "test-model");
        assert_eq!(r.task_id, expected_id);
        assert_eq!(r.judge_model, "test-model");
        assert_eq!(r.plan_id, ctx.plan.id);
    }

    #[test]
    fn build_receipts_uses_implementation_metrics_not_zeros() {
        // Regression: build_receipts used to hardcode iterations=0, elapsed_secs=0.
        // It must read from ctx.implementation.metrics so the receipts reflect
        // the actual Ralph run.
        let mut ctx = dummy_ctx();
        ctx.implementation.metrics = ImplementMetrics {
            stories_passed: 2,
            stories_total: 3,
            iterations: 7,
            elapsed_secs: 1234,
        };
        let r = build_receipts(&ctx, "test-model");
        assert_eq!(
            r.implement_metrics,
            ImplementMetrics {
                stories_passed: 2,
                stories_total: 3,
                iterations: 7,
                elapsed_secs: 1234,
            }
        );
    }

    #[test]
    fn build_judge_prompt_includes_all_sections() {
        let ctx = dummy_ctx();
        let files = vec![("fib.py".to_string(), "def fib(): pass".to_string())];
        let prompt = build_judge_prompt(&ctx, &files);
        assert!(prompt.contains("ALPS Judge"));
        assert!(prompt.contains("build a fib function"));
        assert!(prompt.contains("tests pass"));
        assert!(prompt.contains("abc1234"));
        assert!(prompt.contains("def fib()"));
    }

    #[test]
    fn build_judge_prompt_includes_agents_md_when_nonempty() {
        // After implement, the loop extracts patterns from ralph's progress.txt
        // and writes them to AGENTS.md. The Judge prompt MUST surface them so
        // the LLM can judge against the patterns the implementer discovered.
        let mut ctx = dummy_ctx();
        ctx.agents_md =
            "## Codebase Patterns\n- always run cargo fmt before commit\n- use thiserror for error enums\n".to_string();
        let prompt = build_judge_prompt(&ctx, &[]);
        assert!(prompt.contains("Codebase Patterns"),
            "judge prompt should have Codebase Patterns section, got:\n{}", prompt);
        assert!(prompt.contains("always run cargo fmt before commit"));
        assert!(prompt.contains("use thiserror for error enums"));
    }

    #[test]
    fn build_judge_prompt_omits_agents_md_section_when_empty() {
        // First iteration: no AGENTS.md yet. Should NOT include an empty
        // "## Codebase Patterns" section.
        let ctx = dummy_ctx();
        assert!(ctx.agents_md.is_empty());
        let prompt = build_judge_prompt(&ctx, &[]);
        assert!(!prompt.contains("## Codebase Patterns"),
            "empty agents_md should not appear as a section header, got:\n{}", prompt);
    }

    #[test]
    fn lang_for_known_extensions() {
        assert_eq!(lang_for("foo.py"), "python");
        assert_eq!(lang_for("foo.rs"), "rust");
        assert_eq!(lang_for("foo.ts"), "typescript");
        assert_eq!(lang_for("foo.md"), "markdown");
    }

    #[test]
    fn always_pass_llm_uses_ctx_task_id() {
        let ctx = dummy_ctx();
        let expected_id = ctx.task_id.clone();
        // Run via tokio runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(AlwaysPassLlm.judge(&ctx)).unwrap();
        match result {
            Judgment::Pass(receipts) => assert_eq!(receipts.task_id, expected_id),
            _ => panic!("expected Pass"),
        }
    }

    // ── DoD Runner tests ──

    fn make_tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alps-dod-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detect_rust_project() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Rust, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_project_via_pyproject() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("pyproject.toml"), "[project]").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Python, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_project_via_pytest_ini() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("pytest.ini"), "[pytest]").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Python, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_project_via_test_files() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("test_foo.py"), "def test_x(): pass").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Python, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_node_project() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Node, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_go_project() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("go.mod"), "module foo").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Go, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_unknown_project() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("README.md"), "# readme").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Unknown, dir.clone())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_project_in_nested_subdir_monorepo() {
        // Mirrors smoke #21 (Tier-4 full-stack notes app): the deliverable
        // root contains backend/{pyproject.toml,app/} and frontend/{package.json,src/}
        // but nothing matching the detector's marker set AT the root. The
        // detector must walk immediate subdirs (depth 1-2) to find the
        // Python backend; otherwise every Tier-4 monorepo falls into the
        // "Unknown → skip" short-circuit and the LLM Judge is the
        // load-bearing verifier alone.
        //
        // The second tuple element pins the load-bearing contract surfaced
        // by Tier-4 smoke 2026-08-10 (smoke #22-tier4): the matched subdir
        // path must be returned so the Judge runs the test command from
        // there (not from the deliverable root).
        let dir = make_tmp_dir();
        let backend = dir.join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        std::fs::write(backend.join("pyproject.toml"), "[project]").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Python, backend)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_node_project_in_nested_subdir_monorepo() {
        // Symmetric case: package.json lives one level down (frontend/).
        // The second tuple element pins that the Judge's test command
        // will run from `frontend/` (not from the deliverable root) so
        // `npm test` picks up the right package.json.
        let dir = make_tmp_dir();
        let frontend = dir.join("frontend");
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(frontend.join("package.json"), "{}").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Node, frontend)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_does_not_walk_into_heavy_subdirs() {
        // Don't burn cycles / false positives walking node_modules, .git,
        // target, dist, __pycache__, .venv, venv, etc. If the recursion
        // hits one of those and finds a marker file, it could falsely
        // classify the repo. Pin: skip these subdirs during the walk.
        let dir = make_tmp_dir();
        let node_modules = dir.join("node_modules").join("foo");
        std::fs::create_dir_all(&node_modules).unwrap();
        std::fs::write(node_modules.join("package.json"), "{}").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Unknown, dir.clone()),
            "node_modules/* must not be walked for project detection"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_tier4_fullstack_monorepo_layout() {
        // Exact mirror of smoke #21 deliverable layout: a monorepo with
        // `backend/` (Python, pyproject.toml at depth 1) and `frontend/`
        // (Node, package.json at depth 1) and nothing matching the
        // detector's marker set AT the root. Pre-fix this returned
        // Unknown and the structured DoD short-circuited, leaving the
        // LLM Judge as the load-bearing verifier. Post-fix the detector
        // returns Python (root-priority Rust check fails; first matching
        // subdir in sorted order is `backend/` which wins over
        // `frontend/` alphabetically).
        //
        // The second tuple element pins that the Judge runs pytest from
        // `backend/` (where pyproject.toml + .venv live) and NOT from the
        // deliverable root (which has no venv). Surfaced by Tier-4 smoke
        // 2026-08-10 (smoke #22-tier4): the previous code ran pytest
        // from the root and got exit Some(2) due to ModuleNotFoundError.
        let dir = make_tmp_dir();
        let backend = dir.join("backend");
        let frontend = dir.join("frontend");
        std::fs::create_dir_all(&backend).unwrap();
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(backend.join("pyproject.toml"), "[project]").unwrap();
        std::fs::write(frontend.join("package.json"), "{}").unwrap();
        // `backend` sorts before `frontend` alphabetically, so Python
        // wins under the deterministic sorted-walk contract. This is
        // the expected behavior — Tier-4's structured DoD will fire
        // `pytest -q` against `backend/`.
        assert_eq!(
            detect_project_type(&dir),
            (
                ProjectType::Python,
                dir.join("backend")
            ),
            "Tier-4 monorepo (backend pyproject + frontend package.json) must classify as Python with test_root=backend/"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_root_marker_wins_over_nested_marker() {
        // Priority contract: if a marker file exists at the dir root,
        // that's the project type, even if a different-language marker
        // exists in a subdir. Matters for ALPS running against itself
        // (deliverable IS the alps repo, which has a `frontend/` or
        // docs-site subdir with package.json but should still be Rust).
        //
        // The second tuple element pins that root-level markers return
        // the input dir (not a subdir) as `test_root`, so the Judge's
        // test command runs from where the operator expects it to.
        let dir = make_tmp_dir();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let frontend = dir.join("frontend");
        std::fs::create_dir_all(&frontend).unwrap();
        std::fs::write(frontend.join("package.json"), "{}").unwrap();
        assert_eq!(
            detect_project_type(&dir),
            (ProjectType::Rust, dir.clone()),
            "root-level Cargo.toml must win over nested package.json (test_root = input dir)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn has_py_tests_finds_test_py() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("test_foo.py"), "").unwrap();
        std::fs::write(dir.join("bar.py"), "").unwrap();
        assert!(has_py_tests(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn has_py_tests_no_test_py() {
        let dir = make_tmp_dir();
        std::fs::write(dir.join("foo.py"), "").unwrap();
        std::fs::write(dir.join("bar.py"), "").unwrap();
        assert!(!has_py_tests(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_command_for_each_type() {
        assert_eq!(test_command_for(&ProjectType::Rust), ("cargo", vec!["test", "--quiet"]));
        assert_eq!(test_command_for(&ProjectType::Python), ("python3", vec!["-m", "pytest", "-q"]));
        assert_eq!(test_command_for(&ProjectType::Node), ("npm", vec!["test", "--silent"]));
        assert_eq!(test_command_for(&ProjectType::Go), ("go", vec!["test", "./..."]));
        assert_eq!(test_command_for(&ProjectType::Unknown), ("", vec![]));
    }

    #[test]
    fn project_type_display() {
        assert_eq!(format!("{}", ProjectType::Rust), "rust");
        assert_eq!(format!("{}", ProjectType::Python), "python");
        assert_eq!(format!("{}", ProjectType::Node), "node");
        assert_eq!(format!("{}", ProjectType::Go), "go");
        assert_eq!(format!("{}", ProjectType::Unknown), "unknown");
    }

    #[tokio::test]
    async fn dod_runner_skip_verification_passes() {
        let runner = DoDRunner::with_config(DoDRunnerConfig {
            skip_verification: true,
            ..Default::default()
        });
        let result = runner.check(&dummy_ctx()).await.unwrap();
        assert!(result.all_pass);
        assert!(result.failed.is_empty());
    }

    #[tokio::test]
    async fn dod_runner_unknown_project_skips() {
        // ralph_dir has no project markers (only README.md), and
        // deliverable_path is empty → runner should fall back to ralph_dir
        // and skip DoD checks.
        let dir = make_tmp_dir();
        std::fs::write(dir.join("README.md"), "# readme").unwrap();
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: dir.join("prd.json"),
                deliverable_path: PathBuf::new(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();
        assert!(result.all_pass);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn dod_runner_detect_uses_rust_at_deliverable_path() {
        // ralph_dir is empty (no Cargo.toml there), but the deliverable
        // path has one. Runner should detect Rust at the deliverable
        // path, not at ralph_dir. Smoke4-style (Python + Rust libs from
        // the same alps run) depended on this — the previous
        // `detect_project_type(ralph_dir)` would miss Rust when the
        // Rust crate lives in the deliverable tree.
        let ralph_dir = make_tmp_dir();
        let deliverable = make_tmp_dir();
        std::fs::write(deliverable.join("Cargo.toml"), "[package]").unwrap();
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: ralph_dir.join("prd.json"),
                deliverable_path: deliverable.clone(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();
        // We don't assert PASS — cargo test will fail in this tmp dir
        // because there's no src/lib.rs. We only assert that the
        // detection walked the deliverable: the result should NOT be
        // `all_pass` from the "Unknown → skip" short-circuit, so it
        // should be `all_pass: false` from a real cargo invocation.
        assert!(!result.all_pass, "should have run cargo test, not skipped");
        std::fs::remove_dir_all(&ralph_dir).ok();
        std::fs::remove_dir_all(&deliverable).ok();
    }

    #[tokio::test]
    async fn dod_runner_detect_uses_node_at_deliverable_path() {
        // ralph_dir is empty, deliverable has a package.json. The
        // previous behavior (detect on ralph_dir) would return Unknown
        // and short-circuit — the LLM Judge was the only verifier. After
        // this fix, the structured DoD should attempt to run npm test
        // (and fail because the tmp package.json has no test script,
        // proving the structured path actually fired).
        let ralph_dir = make_tmp_dir();
        let deliverable = make_tmp_dir();
        std::fs::write(deliverable.join("package.json"), r#"{"name":"x","version":"0.0.0"}"#).unwrap();
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: ralph_dir.join("prd.json"),
                deliverable_path: deliverable.clone(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();
        // npm test will exit non-zero on the bare package.json
        // (Missing script: "test"), so all_pass should be false. This
        // proves the structured path actually invoked npm — pre-fix,
        // this would have been all_pass: true from the Unknown
        // short-circuit.
        assert!(!result.all_pass, "structured npm path should have fired");
        std::fs::remove_dir_all(&ralph_dir).ok();
        std::fs::remove_dir_all(&deliverable).ok();
    }

    #[tokio::test]
    async fn dod_runner_detect_uses_go_at_deliverable_path() {
        // ralph_dir is empty, deliverable has a go.mod. Pre-fix this
        // would be Unknown → skip. Post-fix the structured runner
        // should attempt `go test ./...` (and fail because there's no
        // Go source, proving the path fired).
        let ralph_dir = make_tmp_dir();
        let deliverable = make_tmp_dir();
        std::fs::write(deliverable.join("go.mod"), "module x\n\ngo 1.21\n").unwrap();
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: ralph_dir.join("prd.json"),
                deliverable_path: deliverable.clone(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();
        assert!(!result.all_pass, "structured go path should have fired");
        std::fs::remove_dir_all(&ralph_dir).ok();
        std::fs::remove_dir_all(&deliverable).ok();
    }

    #[tokio::test]
    async fn dod_runner_falls_back_to_ralph_dir_when_deliverable_empty() {
        // Empty deliverable_path → runner should probe ralph_dir (legacy
        // behavior preserved). Set a package.json AT ralph_dir to prove
        // the fallback works. Pre-fix and post-fix this is the same
        // behavior; this test pins the contract.
        let ralph_dir = make_tmp_dir();
        std::fs::write(ralph_dir.join("package.json"), r#"{"name":"x","version":"0.0.0"}"#).unwrap();
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: ralph_dir.join("prd.json"),
                deliverable_path: PathBuf::new(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();
        // npm test will fail (no test script) → all_pass: false,
        // proving we hit the structured path via the ralph_dir
        // fallback.
        assert!(!result.all_pass, "structured npm path via ralph_dir should have fired");
        std::fs::remove_dir_all(&ralph_dir).ok();
    }

    /// Tier-4 monorepo regression: the Judge's structured-DoD stage must
    /// run `pytest -q` from `backend/` (where `pyproject.toml` + `.venv`
    /// live), NOT from the deliverable root (which has no venv).
    ///
    /// **Surfaced by smoke #22-tier4 (2026-08-10):** the previous code
    /// ran pytest from the deliverable root and got exit Some(2) with
    /// `ModuleNotFoundError: No module named 'sqlalchemy'`, rejecting
    /// the loop even though codex's runtime pytest (from `backend/` with
    /// the venv activated) reported 10 passed.
    ///
    /// This test builds a synthetic Tier-4-like monorepo in a tmp dir:
    /// the root has nothing matching the detector, but `backend/`
    /// contains a real pyproject.toml + a tiny `test_x.py`. The Judge's
    /// structured stage should:
    /// 1. Detect Python (via monorepo walk into backend/)
    /// 2. Run pytest from backend/, NOT from the deliverable root
    /// 3. Discover + execute test_x.py → all_pass: true
    ///
    /// Pre-fix this test would FAIL (pytest would error out on the
    /// missing module or with "no tests collected" — depending on what
    /// pytest's collection error looks like in this synthetic setup).
    /// Post-fix it passes.
    #[tokio::test]
    async fn dod_runner_runs_pytest_from_monorepo_subdir_not_root() {
        let deliverable = make_tmp_dir();
        let backend = deliverable.join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        // Minimal pyproject.toml so detect_project_type picks backend/.
        std::fs::write(
            backend.join("pyproject.toml"),
            "[project]\nname = \"synthetic\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        // A trivial test that will pass. No imports → no ModuleNotFoundError,
        // no venv needed (we use the system python3). The point of this
        // test is cwd resolution + test discovery, not venv resolution.
        std::fs::create_dir_all(backend.join("tests")).unwrap();
        std::fs::write(
            backend.join("tests").join("test_synthetic.py"),
            "def test_truth():\n    assert True\n",
        )
        .unwrap();

        // Verify the detector returns the right tuple before we drive the runner.
        let (ptype, test_root) = detect_project_type(&deliverable);
        assert_eq!(ptype, ProjectType::Python, "monorepo must classify as Python");
        assert_eq!(
            test_root, backend,
            "detect_project_type must return the matched subdir, not the deliverable root"
        );

        // Drive the Judge's structured stage against the synthetic
        // monorepo. The runner should run pytest from `test_root`
        // (= backend/) and find test_synthetic.py → all_pass.
        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: deliverable.join("prd.json"),
                deliverable_path: deliverable.clone(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();

        assert!(
            result.all_pass,
            "structured-DoD must PASS on the synthetic monorepo (test_root=backend/). \
             Pre-fix this fails because pytest ran from the deliverable root and \
             either failed to collect tests or hit a ModuleNotFoundError. \
             Got: failed.len()={}, first_failed={:?}",
            result.failed.len(),
            result.failed.first().map(|a| a.evidence.clone()),
        );
        assert!(
            result.failed.is_empty(),
            "no failed assertions expected, got: {:?}",
            result.failed
        );

        std::fs::remove_dir_all(&deliverable).ok();
    }

    /// Tier-4 monorepo regression — negative case: the structured-DoD
    /// stage must NOT silently pass when the monorepo's backend pytest
    /// actually fails. Pins that the fix doesn't accidentally turn
    /// "wrong cwd" into "always PASS". The synthetic backend has a
    /// failing test; the runner must return all_pass: false with a
    /// failed assertion citing the actual test failure.
    #[tokio::test]
    async fn dod_runner_monorepo_pytest_failure_is_not_swallowed() {
        let deliverable = make_tmp_dir();
        let backend = deliverable.join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        std::fs::write(
            backend.join("pyproject.toml"),
            "[project]\nname = \"synthetic\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(backend.join("tests")).unwrap();
        std::fs::write(
            backend.join("tests").join("test_failing.py"),
            "def test_fails():\n    assert False, 'synthetic failure'\n",
        )
        .unwrap();

        let ctx = JudgeContext {
            implementation: crate::domain::Implementation {
                prd_path: deliverable.join("prd.json"),
                deliverable_path: deliverable.clone(),
                ..impl_dummy()
            },
            ..dummy_ctx()
        };
        let runner = DoDRunner::new();
        let result = runner.check(&ctx).await.unwrap();

        assert!(
            !result.all_pass,
            "structured-DoD must FAIL when the monorepo's pytest actually fails"
        );
        assert_eq!(
            result.failed.len(),
            1,
            "exactly one failed assertion expected (the pytest -q run), got: {:?}",
            result.failed
        );
        assert!(
            result.failed[0].criterion.contains("pytest"),
            "failed criterion must reference the pytest command, got: {:?}",
            result.failed[0].criterion
        );

        std::fs::remove_dir_all(&deliverable).ok();
    }

    fn impl_dummy() -> crate::domain::Implementation {
        crate::domain::Implementation {
            ralph_branch: "alps/test".to_string(),
            prd_path: PathBuf::from("/tmp/prd.json"),
            commits: vec![],
            artifacts: vec![],
            metrics: Default::default(),
            deliverable_path: PathBuf::from("/tmp/prd"),
        }
    }

    /// Helper: a minimal valid `Judgment::Pass` for test fixtures. Built
    /// fresh each call (closures can't return the same `Receipts` across
    /// calls because `Receipts` contains the consumed `Review`).
    fn pass_judgment(ctx: &JudgeContext) -> Judgment {
        // Receipts requires a real Review context. We reuse the test
        // fixture's review/plan. This is the success path; we just need
        // a Pass judgment with reasonable fields.
        Judgment::Pass(crate::receipt::Receipts {
            task_id: ctx.task_id.clone(),
            plan_id: ctx.plan.id.clone(),
            plan_summary: ctx.plan.goal.clone(),
            implement_metrics: Default::default(),
            review_summary: crate::receipt::ReviewSummary::from_findings(
                &ctx.review.findings,
                &ctx.review.assertions,
            ),
            judged_at: chrono::Utc::now(),
            judge_model: "test".to_string(),
        })
    }

    // ─────────────────────────────────────────────────────────────
    // Retry-on-parse-failure tests (mirror plan.rs / review.rs)
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn judge_retries_on_parse_failure() {
        // First 2 calls return Parse errors; 3rd call returns Ok(pass).
        // With default max_retries=3, the loop should retry and succeed.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let judge = HermesLlmJudge::for_test(move |ctx: &JudgeContext| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(JudgeError::Parse(format!(
                    "simulated bad JSON on attempt {}",
                    n + 1
                )))
            } else {
                Ok(pass_judgment(ctx))
            }
        });

        let result = judge.judge(&dummy_ctx()).await;

        let j = result.expect("judge should succeed on 3rd attempt");
        assert!(matches!(j, Judgment::Pass(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn judge_gives_up_after_max_retries() {
        // All 3 calls return Parse errors. After max_retries attempts,
        // judge() should return a final Parse error wrapping the last failure.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let judge = HermesLlmJudge::for_test(move |_ctx: &JudgeContext| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(JudgeError::Parse(format!(
                "simulated bad JSON on attempt {}",
                n + 1
            )))
        });

        let result = judge.judge(&dummy_ctx()).await;

        let err = result.expect_err("judge should fail after max_retries");
        let msg = err.to_string();
        assert!(
            msg.contains("failed after 3 attempts"),
            "expected 'failed after 3 attempts' in error, got: {}",
            msg
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn judge_no_retry_on_first_success() {
        // The closure returns Ok on the first call. No retries.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let judge = HermesLlmJudge::for_test(move |ctx: &JudgeContext| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Ok(pass_judgment(ctx))
        });

        let result = judge.judge(&dummy_ctx()).await;
        let _j = result.expect("judge should succeed");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retries on first success");
    }

    #[tokio::test]
    async fn judge_does_not_retry_on_llm_error() {
        // Non-Parse errors (e.g. spawn failure, unknown verdict) propagate
        // immediately, no retry. Retrying a deterministic error won't help.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let judge = HermesLlmJudge::for_test(move |_ctx: &JudgeContext| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(JudgeError::Llm("spawn failed".to_string()))
        });

        let result = judge.judge(&dummy_ctx()).await;

        let err = result.expect_err("llm error should fail without retry");
        assert!(
            err.to_string().contains("spawn failed"),
            "expected 'spawn failed' in error, got: {}",
            err
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no retry on Llm error"
        );
    }

    #[tokio::test]
    async fn judge_max_retries_1_means_no_retry() {
        // max_retries=1: only the original attempt. Parse error → fail.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let judge = HermesLlmJudge {
            config: LlmJudgeConfig {
                max_retries: 1,
                ..LlmJudgeConfig::default()
            },
            test_handler: Some(Arc::new(move |_ctx: &JudgeContext| {
                calls_for_closure.fetch_add(1, Ordering::SeqCst);
                Err(JudgeError::Parse("only attempt fails".to_string()))
            })),
        };

        let result = judge.judge(&dummy_ctx()).await;
        let err = result.expect_err("max_retries=1 should fail on first Parse error");
        assert!(
            err.to_string().contains("failed after 1 attempts"),
            "expected 'failed after 1 attempts' in error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry with max_retries=1");
    }

    // ─────────────────────────────────────────────────────────────
        // Judge subprocess timeout tests (SPEC §12 item 8)
        // ─────────────────────────────────────────────────────────────
        //
        // Before this field existed (commit `6c391d8` and earlier), the
        // Judge subprocess had no wall-clock timeout. On a heavy Judge
        // prompt (large `files` section, long Review findings), Claude Code
        // Opus could exceed any reasonable wall-clock expectation, leaving
        // `judge.run(ctx).await` awaiting forever. The orchestrator would
        // then be killed by upstream SIGPIPE (from the wrapping agent's
        // tee/pipe), causing receipts.json + .alps-last-done to never land
        // even though the deliverable was real. This block pins the fix.
        //
        // We bypass the `for_test` handler to actually spawn a subprocess.
        // `cli_path` points to a tiny bash script that ignores all flags
        // and runs `sleep 86400` (hangs forever). Without the fix the
        // Judge call would block for 86400s; with the fix it returns in
        // `judge_timeout_secs`.

        /// Write a hang-helper script to a tempdir and return its path.
        /// The script ignores all CLI flags (so it survives the Judge's
        /// `--dangerously-skip-permissions -p --model <value>` invocation)
        /// and exec's `sleep 86400` to hang forever.
        fn write_hang_helper() -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "alps-judge-timeout-test-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("tempdir should be creatable");
            let script = dir.join("hang.sh");
            std::fs::write(
                &script,
                "#!/bin/bash\n\
                 # Test helper: ignore all flags, hang forever.\n\
                 while [[ $# -gt 0 ]]; do shift; done\n\
                 exec sleep 86400\n",
            )
            .expect("write hang script");
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .expect("chmod hang script");
            script
        }

        fn hang_subprocess_config(timeout_secs: u64) -> (LlmJudgeConfig, std::path::PathBuf) {
            let script = write_hang_helper();
            let config = LlmJudgeConfig {
                cli_path: script.to_string_lossy().to_string(),
                // model is passed as `--model {model}` — anything works
                // since the helper script ignores it.
                model: "unused".to_string(),
                judge_timeout_secs: timeout_secs,
                ..LlmJudgeConfig::default()
            };
            (config, script)
        }

        #[tokio::test]
        async fn judge_subprocess_timeout_fires_when_hung() {
            let (config, script) = hang_subprocess_config(1);

            let judge = HermesLlmJudge {
                config,
                test_handler: None, // <-- bypass for_test, actually spawn subprocess
            };

            let start = std::time::Instant::now();
            let result = judge.judge(&dummy_ctx()).await;
            let elapsed = start.elapsed();

            let err = result.expect_err("hung subprocess should time out, not succeed");
            let msg = err.to_string();
            assert!(
                msg.contains("timed out after 1s"),
                "expected 'timed out after 1s' in error, got: {}",
                msg
            );
            assert!(
                elapsed < std::time::Duration::from_secs(5),
                "timeout should fire in ~1s, took {:?}",
                elapsed
            );

            // Clean up the helper script's parent dir.
            let _ = std::fs::remove_dir_all(script.parent().unwrap());
        }

        #[tokio::test]
        async fn judge_subprocess_timeout_default_is_sensible() {
            // Default timeout should be large enough to accommodate realistic
            // Opus Judge calls on 50KB+ prompts, but small enough that a hung
            // subprocess doesn't block a smoke indefinitely. 600s (10 min) is
            // the documented default (SPEC §12 item 8). If anyone changes
            // this, this test makes the change intentional.
            assert_eq!(
                LlmJudgeConfig::default().judge_timeout_secs,
                600,
                "default judge_timeout_secs must stay at 600s unless SPEC §12 item 8 is revisited"
            );
        }

        #[tokio::test]
        async fn judge_subprocess_killed_after_timeout() {
            // After the timeout fires, the subprocess should be killed
            // (not left holding stdout/stderr handles). We snapshot the
            // baseline PID count of hang-helper processes before the test,
            // then assert no new ones are left running after the test.
            let (config, script) = hang_subprocess_config(1);

            let baseline = std::process::Command::new("pgrep")
                .args(["-f", &script.to_string_lossy()])
                .output()
                .expect("pgrep should run")
                .stdout;
            let baseline_pids: std::collections::HashSet<u32> = String::from_utf8_lossy(&baseline)
                .lines()
                .filter_map(|s| s.trim().parse().ok())
                .collect();

            let judge = HermesLlmJudge {
                config,
                test_handler: None,
            };
            let _ = judge.judge(&dummy_ctx()).await;

            // Give the kill -9 a moment to take effect.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let after = std::process::Command::new("pgrep")
                .args(["-f", &script.to_string_lossy()])
                .output()
                .expect("pgrep should run")
                .stdout;
            let after_pids: std::collections::HashSet<u32> = String::from_utf8_lossy(&after)
                .lines()
                .filter_map(|s| s.trim().parse().ok())
                .collect();

            // Any new hang-helper processes that appeared during our test
            // and are still running means we leaked a subprocess.
            let leaked: Vec<_> = after_pids.difference(&baseline_pids).collect();
            assert!(
                leaked.is_empty(),
                "leaked {} hang-helper subprocess(es) after timeout: {:?}",
                leaked.len(),
                leaked
            );

            // Clean up.
            let _ = std::fs::remove_dir_all(script.parent().unwrap());
        }
    }

use std::sync::Arc;
