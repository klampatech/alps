//! Review agent — Claude Code adversarial review.
//!
//! Consumes a `ReviewContext { plan, implementation }`, produces a `Review`
//! (findings + assertions).
//!
//! The Review reads files from the ralph_dir (`prd_path.parent()`), builds an
//! adversarial prompt, invokes Claude, and parses the JSON output.
//!
//! See `SPEC.md` §10 (agent integrations).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent::{Agent, sealed};
use crate::domain::{
    ArtifactKind, Assertion, Finding, Implementation, Plan, Review, Severity,
};
use crate::elog;

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("claude code invocation failed: {0}")]
    ClaudeCode(String),

    #[error("failed to parse review output: {0}")]
    Parse(String),

    #[error("schema validation failed: {0}")]
    Schema(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Context for the Review agent — what it needs to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewContext {
    pub plan: Plan,
    pub implementation: Implementation,
    /// Codebase patterns from the implement step (via AGENTS.md propagation).
    /// Empty string if no patterns have been discovered yet.
    pub agents_md: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    pub claude_path: String,
    pub model: String,
    /// Skip files larger than this (in bytes).
    pub max_file_bytes: usize,
    /// Skip remaining files once we exceed this total.
    pub max_total_bytes: usize,
    /// Maximum number of total attempts when the LLM emits invalid JSON.
    /// `1` = no retry (just the original attempt), `3` = 1 original + 2
    /// retries (default). Only `ReviewError::Parse` triggers a retry; spawn
    /// errors and schema validation errors are propagated immediately.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_max_retries() -> u32 {
    3
}

impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig {
            claude_path: "claude".to_string(),
            // Claude Sonnet 4 was retired on 2026-06-15; the `claude` CLI now
            // refuses the alias with exit-1 ("Sonnet 4 was retired..."). Default
            // to the current Opus 4.5 alias, which matches the Judge default
            // at `alps-core/src/judge.rs:186` (`claude-opus-4`).
            model: "claude-opus-4-5".to_string(),
            max_file_bytes: 50_000,
            max_total_bytes: 500_000,
            max_retries: default_max_retries(),
        }
    }
}

/// Review agent — adversarial review via Claude Code.
pub struct ReviewAgent {
    pub config: ReviewConfig,
    /// Test-only override: when set, `run()` calls this closure instead of
    /// spawning Claude Code. Used by `drive_*` integration tests in
    /// `loop_::tests` to deterministically exercise the orchestration.
    #[cfg(test)]
    pub(crate) test_handler: Option<
        std::sync::Arc<dyn Fn(ReviewContext) -> Result<Review, ReviewError> + Send + Sync>,
    >,
}

impl ReviewAgent {
    pub fn new(config: ReviewConfig) -> Self {
        ReviewAgent {
            config,
            #[cfg(test)]
            test_handler: None,
        }
    }

    /// Test-only constructor that bypasses Claude Code. The closure receives
    /// the input context and returns a canned (or computed) `Review`.
    #[cfg(test)]
    pub fn for_test<F>(f: F) -> Self
    where
        F: Fn(ReviewContext) -> Result<Review, ReviewError> + Send + Sync + 'static,
    {
        ReviewAgent {
            config: ReviewConfig::default(),
            test_handler: Some(std::sync::Arc::new(f)),
        }
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
    type Input = ReviewContext;
    type Output = Review;
    type Error = ReviewError;

    fn name(&self) -> &'static str {
        "review"
    }

    async fn run(&self, ctx: ReviewContext) -> Result<Self::Output, Self::Error> {
        // Retry loop. Claude Code occasionally emits invalid JSON (e.g.,
        // trailing comma from a "finding description" field — observed
        // 2026-07-27 in herdr smoke wA3:p1, parse error at line 16 col 42).
        // On `ReviewError::Parse`, retry up to `config.max_retries` total
        // attempts. Spawn errors and schema validation errors propagate
        // immediately — retrying won't fix those (they're deterministic
        // for the same input).
        //
        // See the `review_retries_on_parse_failure` test in this file for
        // the exact contract: per-attempt calls of the test_handler with
        // monotonic attempt numbering, only Parse errors retried.
        let max_attempts = self.config.max_retries.max(1) as usize;
        let mut last_err: Option<ReviewError> = None;
        for attempt in 1..=max_attempts {
            match self.run_once(ctx.clone()).await {
                Ok(review) => return Ok(review),
                Err(ReviewError::Parse(msg)) => {
                    elog!(
                        "[review] parse failed (attempt {}/{}): {}",
                        attempt, max_attempts, msg
                    );
                    last_err = Some(ReviewError::Parse(msg));
                }
                Err(other) => return Err(other),
            }
        }
        Err(ReviewError::Parse(format!(
            "failed after {} attempts: {}",
            max_attempts,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
}

// ─────────────────────────────────────────────────────────────
// Inherent methods (not on the Agent trait)
//
// `run_once` is called by the trait's `run()` method (above) per retry.
// It MUST live in an `impl ReviewAgent` block, not in the
// `impl Agent for ReviewAgent` block, because Rust resolves
// `self.run_once(...)` to the trait first — and the `Agent` trait doesn't
// have a `run_once` method. Putting it here makes it an inherent method
// on `ReviewAgent`, which is what we want.

impl ReviewAgent {
    /// One attempt of the Review agent. Called by `run()` per retry.
    /// Either invokes the test_handler (cfg(test) only) or spawns Claude.
    async fn run_once(&self, ctx: ReviewContext) -> Result<Review, ReviewError> {
        // Test-only fast path: if a test_handler is set, use it instead of
        // spawning Claude. The closure is called PER ATTEMPT (i.e., once
        // per retry), so test fixtures can use interior mutability
        // (Arc<AtomicUsize>, Arc<Mutex<Vec<_>>>) to return different
        // responses across attempts.
        #[cfg(test)]
        if let Some(f) = &self.test_handler {
            return f(ctx);
        }

        // 1. Read source files from the ralph_dir so Claude can see the code
        let files = read_files(&ctx.implementation, &self.config)?;

        // 2. Build the adversarial prompt
        let prompt = build_review_prompt(&ctx, &files);

        // 3. Spawn Claude Code via stdin
        let mut child = Command::new(&self.config.claude_path)
            .args([
                "--dangerously-skip-permissions",
                "-p",
                "--model",
                &self.config.model,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ReviewError::ClaudeCode(format!("spawn failed: {}", e)))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ReviewError::ClaudeCode("no stdin handle".to_string()))?;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| ReviewError::ClaudeCode(format!("stdin write failed: {}", e)))?;
        drop(stdin);

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| ReviewError::ClaudeCode(format!("wait failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ReviewError::ClaudeCode(format!(
                "exit {:?}: {}",
                output.status.code(),
                stderr.chars().take(2000).collect::<String>()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let json_str = strip_markdown_fences(&stdout);

        let parsed: ParsedReview = serde_json::from_str(json_str)
            .map_err(|e| ReviewError::Parse(format!("{}: {}", e, json_str.chars().take(500).collect::<String>())))?;

        validate_review(&parsed)?;

        Ok(parsed.into_review())
    }
}

// ─────────────────────────────────────────────────────────────
// File reading
// ─────────────────────────────────────────────────────────────

fn read_files(
    impl_: &Implementation,
    config: &ReviewConfig,
) -> Result<Vec<(String, String)>, ReviewError> {
    // Walk the deliverable tree, not the ralph nested workspace. When the
    // CLI sets `--deliverable-path`, this points at the user's actual
    // target (e.g. `/tmp/foo/`); otherwise it falls back to ralph_dir
    // (the legacy behavior). See SPEC §12 item 2.
    let artifacts_root = if impl_.deliverable_path.as_os_str().is_empty() {
        impl_
            .prd_path
            .parent()
            .ok_or_else(|| ReviewError::Schema(format!("prd_path has no parent: {:?}", impl_.prd_path)))?
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
            Err(_) => continue, // skip unreadable files
        };
        if bytes.len() > config.max_file_bytes {
            continue; // skip large files
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

pub(crate) fn build_review_prompt(ctx: &ReviewContext, files: &[(String, String)]) -> String {
    let plan = &ctx.plan;
    let impl_ = &ctx.implementation;

    let mut commits = String::new();
    for c in &impl_.commits {
        commits.push_str(&format!("  - `{}` {}\n", &c.sha[..7.min(c.sha.len())], c.message));
    }

    let mut stories = String::new();
    for s in &plan.stories {
        stories.push_str(&format!(
            "  - **{}** (priority {}): {}\n    *Acceptance:* {}\n",
            s.id.0,
            s.priority,
            s.title,
            s.acceptance_criteria.join("; ")
        ));
    }

    let mut dod = String::new();
    for d in &plan.dod {
        let mark = if d.verifiable { "[verifiable]" } else { "[soft]" };
        dod.push_str(&format!("  - {} {}\n", mark, d.criterion));
    }

    let mut file_section = String::new();
    if files.is_empty() {
        file_section.push_str("(no files to review)\n");
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
        "{}\n\n---\n\n## Plan\n\n**Goal:** {}\n\n**Architecture:**\n{}\n\n**Stories:**\n{}\n**DoD criteria:**\n{}\n\n## Implementation\n\n**Branch:** `{}`
**Commits ({}):**\n{}

## Source files
{}
{}{}

---

USER REQUEST:
Review the implementation above adversarially. Verify each DoD criterion. Find bugs. Output JSON.",
        REVIEW_SYSTEM_PROMPT,
        plan.goal,
        plan.architecture,
        stories,
        dod,
        impl_.ralph_branch,
        impl_.commits.len(),
        commits,
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
struct ParsedReview {
    findings: Vec<ParsedFinding>,
    assertions: Vec<ParsedAssertion>,
}

#[derive(Debug, Deserialize)]
struct ParsedFinding {
    severity: String,
    description: String,
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct ParsedAssertion {
    criterion: String,
    passed: bool,
    evidence: String,
}

impl ParsedReview {
    fn into_review(self) -> Review {
        Review {
            findings: self
                .findings
                .into_iter()
                .map(|f| Finding {
                    severity: parse_severity(&f.severity),
                    description: f.description,
                    evidence: f.evidence,
                })
                .collect(),
            assertions: self
                .assertions
                .into_iter()
                .map(|a| Assertion {
                    criterion: a.criterion,
                    passed: a.passed,
                    evidence: a.evidence,
                })
                .collect(),
        }
    }
}

fn parse_severity(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "critical" => Severity::Critical,
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn validate_review(p: &ParsedReview) -> Result<(), ReviewError> {
    if p.assertions.is_empty() {
        return Err(ReviewError::Schema(
            "no assertions — must verify at least one criterion".to_string(),
        ));
    }
    for (i, a) in p.assertions.iter().enumerate() {
        if a.criterion.trim().is_empty() {
            return Err(ReviewError::Schema(format!("assertion[{}] has empty criterion", i)));
        }
        if a.evidence.trim().is_empty() {
            return Err(ReviewError::Schema(format!(
                "assertion[{}] ({}) has empty evidence",
                i, a.criterion
            )));
        }
    }
    for (i, f) in p.findings.iter().enumerate() {
        if f.description.trim().is_empty() {
            return Err(ReviewError::Schema(format!("finding[{}] has empty description", i)));
        }
        if f.evidence.trim().is_empty() {
            return Err(ReviewError::Schema(format!("finding[{}] has empty evidence", i)));
        }
    }
    Ok(())
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
// System prompt — adversarial, NOT a yes-bot
// ─────────────────────────────────────────────────────────────

const REVIEW_SYSTEM_PROMPT: &str = r#"You are the ALPS Review agent — an adversarial code reviewer. You are NOT a yes-bot. Your job is to find problems, not rubber-stamp.

CRITICAL OUTPUT RULES:
- Your response MUST be a single JSON object.
- Start your response with `{` and end with `}`.
- Do NOT write any prose, commentary, "I have all the data", markdown fences, or preamble.
- Do NOT call any tools (no Read, no Bash, no Grep). The file contents you need are already in the prompt.
- Do NOT use TodoWrite or any task-tracking tools.

Given a Plan and an Implementation, you will:

1. **Verify each DoD criterion** against the actual implementation. Read the source files provided. Run mental test cases. Check the commits.
2. **Find bugs and design issues** — edge cases, off-by-one errors, missing input validation, security concerns, performance traps.
3. **Be specific** — every finding and assertion must cite concrete evidence: a file:line reference, a commit SHA, a test name, an output snippet.

Output ONLY valid JSON matching this schema:

{
  "findings": [
    {
      "severity": "Info" | "Warning" | "Error" | "Critical",
      "description": "string — what the issue is, in 1-2 sentences",
      "evidence": "string — proof: commit SHA, file:line, test output, etc."
    }
  ],
  "assertions": [
    {
      "criterion": "string — DoD criterion text",
      "passed": true | false,
      "evidence": "string — verification proof"
    }
  ]
}

Severity guide:
- **Critical** — security flaw, data loss, crash, makes the system unusable
- **Error** — bug that breaks expected behavior; should block acceptance
- **Warning** — code smell, design issue, or robustness gap; should be fixed but not blocking
- **Info** — observation, style note, suggestion for improvement

Guidelines:
- Be adversarial. If you can't find issues, look harder — check edge cases, error paths, type mismatches.
- For every DoD criterion, emit exactly one assertion. If you can't verify it from the artifacts, mark passed=false with explanation.
- Order findings by severity: Critical first, then Error, Warning, Info.

REMINDER: First character of your response is `{`. Last character is `}`. No other characters outside the JSON object."#;

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Artifact, Commit, DefinitionOfDone, PlanId, StoryId, UserStory};
    use crate::domain::{Implementation, Plan};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use uuid::Uuid;

    /// Helper: a minimal valid `Review` for test fixtures. Constructed fresh
    /// each call (closures can't return the same `Review` across calls
    /// because `Review` is consumed by `Result<Review, _>`).
    fn test_review() -> Review {
        Review {
            findings: vec![],
            assertions: vec![Assertion {
                criterion: "tests pass".to_string(),
                passed: true,
                evidence: "1 passed".to_string(),
            }],
        }
    }

    fn dummy_ctx() -> ReviewContext {
        ReviewContext {
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
                    DefinitionOfDone { criterion: "code is clean".to_string(), verifiable: false },
                ],
            },
            implementation: Implementation {
                ralph_branch: "alps/test".to_string(),
                prd_path: PathBuf::from("/tmp/prd.json"),
                commits: vec![Commit {
                    sha: "abc1234".to_string(),
                    message: "feat: fib function".to_string(),
                }],
                artifacts: vec![Artifact {
                    path: PathBuf::from("fib.py"),
                    kind: ArtifactKind::Source,
                }],
                metrics: Default::default(),
                deliverable_path: PathBuf::from("/tmp/alps-review-fixture"),
            },
            agents_md: String::new(),
        }
    }

    #[test]
    fn strip_markdown_fences_json() {
        let s = "```json\n{}\n```";
        assert_eq!(strip_markdown_fences(s), "{}");
    }

    #[test]
    fn parse_severity_lowercase() {
        assert!(matches!(parse_severity("critical"), Severity::Critical));
        assert!(matches!(parse_severity("error"), Severity::Error));
        assert!(matches!(parse_severity("warning"), Severity::Warning));
        assert!(matches!(parse_severity("warn"), Severity::Warning));
        assert!(matches!(parse_severity("info"), Severity::Info));
        assert!(matches!(parse_severity(""), Severity::Info));
    }

    #[test]
    fn validate_no_assertions() {
        let p = ParsedReview {
            findings: vec![],
            assertions: vec![],
        };
        assert!(validate_review(&p).is_err());
    }

    #[test]
    fn validate_empty_criterion() {
        let p = ParsedReview {
            findings: vec![],
            assertions: vec![ParsedAssertion {
                criterion: "".to_string(),
                passed: true,
                evidence: "test".to_string(),
            }],
        };
        assert!(validate_review(&p).is_err());
    }

    #[test]
    fn validate_empty_evidence() {
        let p = ParsedReview {
            findings: vec![],
            assertions: vec![ParsedAssertion {
                criterion: "test".to_string(),
                passed: true,
                evidence: "".to_string(),
            }],
        };
        assert!(validate_review(&p).is_err());
    }

    #[test]
    fn validate_empty_finding_description() {
        let p = ParsedReview {
            findings: vec![ParsedFinding {
                severity: "Info".to_string(),
                description: "".to_string(),
                evidence: "test".to_string(),
            }],
            assertions: vec![ParsedAssertion {
                criterion: "test".to_string(),
                passed: true,
                evidence: "ok".to_string(),
            }],
        };
        assert!(validate_review(&p).is_err());
    }

    #[test]
    fn parsed_review_to_review() {
        let p = ParsedReview {
            findings: vec![ParsedFinding {
                severity: "warning".to_string(),
                description: "missing input validation".to_string(),
                evidence: "fib.py:5".to_string(),
            }],
            assertions: vec![ParsedAssertion {
                criterion: "tests pass".to_string(),
                passed: true,
                evidence: "1 passed".to_string(),
            }],
        };
        let r = p.into_review();
        assert_eq!(r.findings.len(), 1);
        assert!(matches!(r.findings[0].severity, Severity::Warning));
        assert_eq!(r.assertions.len(), 1);
        assert!(r.assertions[0].passed);
    }

    #[test]
    fn build_prompt_includes_plan_and_files() {
        let ctx = dummy_ctx();
        let files = vec![(
            "fib.py".to_string(),
            "def fib(n):\n    return [0,1]\n".to_string(),
        )];
        let prompt = build_review_prompt(&ctx, &files);
        assert!(prompt.contains("ALPS Review agent"));
        assert!(prompt.contains("build a fib function"));
        assert!(prompt.contains("US-001"));
        assert!(prompt.contains("tests pass"));
        assert!(prompt.contains("abc1234"));
        assert!(prompt.contains("fib.py"));
        assert!(prompt.contains("def fib(n)"));
    }

    #[test]
    fn build_prompt_includes_agents_md_when_nonempty() {
        // After implement, the loop extracts patterns from ralph's progress.txt
        // and writes them to AGENTS.md. The Review prompt MUST surface them so
        // Claude can avoid re-discovering patterns and align with the
        // implementer's notes.
        let mut ctx = dummy_ctx();
        ctx.agents_md = "## Codebase Patterns\n- use foo for bar\n- never baz\n".to_string();
        let prompt = build_review_prompt(&ctx, &[]);
        assert!(prompt.contains("Codebase Patterns"), "prompt should have a Codebase Patterns section, got:\n{}", prompt);
        assert!(prompt.contains("use foo for bar"));
        assert!(prompt.contains("never baz"));
    }

    #[test]
    fn build_prompt_omits_agents_md_section_when_empty() {
        // First iteration: no AGENTS.md yet (implement hasn't run). The prompt
        // should NOT include an empty "## Codebase Patterns" section.
        let ctx = dummy_ctx();
        assert!(ctx.agents_md.is_empty());
        let prompt = build_review_prompt(&ctx, &[]);
        assert!(!prompt.contains("## Codebase Patterns"),
            "empty agents_md should not appear as a section header, got:\n{}", prompt);
    }

    #[test]
    fn lang_for_known_extensions() {
        assert_eq!(lang_for("foo.py"), "python");
        assert_eq!(lang_for("foo.rs"), "rust");
        assert_eq!(lang_for("foo.ts"), "typescript");
        assert_eq!(lang_for("foo.md"), "markdown");
        assert_eq!(lang_for("foo.toml"), "toml");
        assert_eq!(lang_for("foo.txt"), "");
    }

    #[test]
    fn review_agent_name() {
        let agent = ReviewAgent::default();
        assert_eq!(agent.name(), "review");
    }

    // ─────────────────────────────────────────────────────────────
    // Retry-on-parse-failure tests (mirror plan.rs)
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn review_retries_on_parse_failure() {
        // First 2 calls return Parse errors; 3rd call returns Ok(review).
        // With default max_retries=3, the loop should retry and succeed.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent::for_test(move |_ctx: ReviewContext| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(ReviewError::Parse(format!(
                    "simulated bad JSON on attempt {}",
                    n + 1
                )))
            } else {
                Ok(test_review())
            }
        });

        let result = agent.run(dummy_ctx()).await;

        let review = result.expect("review should succeed on 3rd attempt");
        assert_eq!(review.assertions.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn review_gives_up_after_max_retries() {
        // All 3 calls return Parse errors. After max_retries attempts,
        // run() should return a final Parse error wrapping the last failure.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent::for_test(move |_ctx: ReviewContext| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(ReviewError::Parse(format!(
                "simulated bad JSON on attempt {}",
                n + 1
            )))
        });

        let result = agent.run(dummy_ctx()).await;

        let err = result.expect_err("review should fail after max_retries");
        let msg = err.to_string();
        assert!(
            msg.contains("failed after 3 attempts"),
            "expected 'failed after 3 attempts' in error, got: {}",
            msg
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn review_no_retry_on_first_success() {
        // The closure returns Ok on the first call. No retries.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent::for_test(move |_ctx: ReviewContext| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Ok(test_review())
        });

        let result = agent.run(dummy_ctx()).await;

        let _review = result.expect("review should succeed");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retries on first success");
    }

    #[tokio::test]
    async fn review_does_not_retry_on_claude_code_error() {
        // Non-Parse errors (e.g. ClaudeCode) propagate immediately, no retry.
        // Retrying a spawn error won't help (deterministic for the same input).
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent::for_test(move |_ctx: ReviewContext| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(ReviewError::ClaudeCode("spawn failed".to_string()))
        });

        let result = agent.run(dummy_ctx()).await;

        let err = result.expect_err("claude code error should fail without retry");
        assert!(
            err.to_string().contains("spawn failed"),
            "expected 'spawn failed' in error, got: {}",
            err
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no retry on ClaudeCode error"
        );
    }

    #[tokio::test]
    async fn review_does_not_retry_on_schema_error() {
        // Schema errors (e.g. empty criterion) are deterministic — the LLM
        // produced valid JSON but with wrong structure. Retrying won't help
        // (the schema is invariant to retry).
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent::for_test(move |_ctx: ReviewContext| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(ReviewError::Schema("no assertions".to_string()))
        });

        let result = agent.run(dummy_ctx()).await;

        let err = result.expect_err("schema error should fail without retry");
        assert!(
            err.to_string().contains("no assertions"),
            "expected 'no assertions' in error, got: {}",
            err
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no retry on Schema error"
        );
    }

    #[tokio::test]
    async fn review_max_retries_1_means_no_retry() {
        // max_retries=1: only the original attempt. Parse error → fail.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let agent = ReviewAgent {
            config: ReviewConfig {
                max_retries: 1,
                ..ReviewConfig::default()
            },
            test_handler: Some(Arc::new(move |_ctx: ReviewContext| {
                calls_for_closure.fetch_add(1, Ordering::SeqCst);
                Err(ReviewError::Parse("only attempt fails".to_string()))
            })),
        };

        let result = agent.run(dummy_ctx()).await;
        let err = result.expect_err("max_retries=1 should fail on first Parse error");
        assert!(
            err.to_string().contains("failed after 1 attempts"),
            "expected 'failed after 1 attempts' in error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry with max_retries=1");
    }
}
