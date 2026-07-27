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
}

impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig {
            claude_path: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            max_file_bytes: 50_000,
            max_total_bytes: 500_000,
        }
    }
}

/// Review agent — adversarial review via Claude Code.
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
    type Input = ReviewContext;
    type Output = Review;
    type Error = ReviewError;

    fn name(&self) -> &'static str {
        "review"
    }

    async fn run(&self, ctx: ReviewContext) -> Result<Self::Output, Self::Error> {
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
    let ralph_dir = impl_
        .prd_path
        .parent()
        .ok_or_else(|| ReviewError::Schema(format!("prd_path has no parent: {:?}", impl_.prd_path)))?;

    let mut files = Vec::new();
    let mut total = 0usize;

    for artifact in &impl_.artifacts {
        if total >= config.max_total_bytes {
            break;
        }
        let path = ralph_dir.join(&artifact.path);
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
    use uuid::Uuid;

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
}
