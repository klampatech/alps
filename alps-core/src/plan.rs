use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent::{Agent, sealed};
use crate::domain::{
    DefinitionOfDone, Plan, PlanId, Prompt, StoryId, UserStory,
};
use crate::elog;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("claude code invocation failed: {0}")]
    ClaudeCode(String),

    #[error("failed to parse plan output: {0}")]
    Parse(String),

    #[error("schema validation failed: {0}")]
    Schema(String),
}

/// Config for the Plan agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConfig {
    /// Path to the `claude` binary.
    pub claude_path: String,
    /// Model to use (e.g. "claude-sonnet-4").
    pub model: String,
    /// System prompt that instructs Claude to emit JSON.
    pub system_prompt: String,
    /// Maximum number of total attempts when the LLM emits invalid JSON.
    /// `1` = no retry (just the original attempt), `3` = 1 original + 2
    /// retries (default). Only `PlanError::Parse` triggers a retry; spawn
    /// errors and schema validation errors are propagated immediately.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_max_retries() -> u32 {
    3
}

impl Default for PlanConfig {
    fn default() -> Self {
        PlanConfig {
            claude_path: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            system_prompt: PLAN_AGENT_SYSTEM_PROMPT.to_string(),
            max_retries: default_max_retries(),
        }
    }
}

/// Plan agent — invokes Claude Code with a structured system prompt,
/// parses the JSON output, and returns a typed `Plan`.
pub struct PlanAgent {
    pub config: PlanConfig,
    /// Test-only override: when set, `run()` calls this closure instead of
    /// spawning Claude Code. Used by `drive_*` integration tests in
    /// `loop_::tests` to deterministically exercise the orchestration.
    ///
    /// `run()` calls this closure PER ATTEMPT (i.e., once per retry). Use
    /// `Arc<AtomicUsize>` inside the closure if you need to return
    /// different responses across attempts (e.g., fail N-1 times then
    /// succeed on the Nth).
    #[cfg(test)]
    pub(crate) test_handler: Option<std::sync::Arc<dyn Fn(Prompt) -> Result<Plan, PlanError> + Send + Sync>>,
}

impl PlanAgent {
    pub fn new(model: impl Into<String>) -> Self {
        PlanAgent {
            config: PlanConfig {
                model: model.into(),
                ..Default::default()
            },
            #[cfg(test)]
            test_handler: None,
        }
    }

    pub fn with_config(config: PlanConfig) -> Self {
        PlanAgent {
            config,
            #[cfg(test)]
            test_handler: None,
        }
    }

    /// Test-only constructor that bypasses Claude Code. The closure receives
    /// the input prompt and returns a canned (or computed) `Plan`.
    #[cfg(test)]
    pub fn for_test<F>(f: F) -> Self
    where
        F: Fn(Prompt) -> Result<Plan, PlanError> + Send + Sync + 'static,
    {
        PlanAgent {
            config: PlanConfig::default(),
            test_handler: Some(std::sync::Arc::new(f)),
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Inherent methods (not on the Agent trait)
// ─────────────────────────────────────────────────────────────
//
// `run_once` is called by the trait's `run()` method (above) per retry.
// It MUST live in an `impl PlanAgent` block, not in the `impl Agent for
// PlanAgent` block, because Rust resolves `self.run_once(...)` to the
// trait first — and the `Agent` trait doesn't have a `run_once` method.
// Putting it here makes it an inherent method on `PlanAgent`, which is
// what we want.

impl PlanAgent {
    /// One attempt of the Plan agent. Called by `run()` per retry.
    /// Either invokes the test_handler (cfg(test) only) or spawns Claude Code.
    async fn run_once(&self, input: Prompt) -> Result<Plan, PlanError> {
        // Test-only fast path: if a test_handler is set, use it instead of
        // spawning Claude Code. This lets integration tests in `loop_::tests`
        // and `plan_retries_on_parse_failure` exercise the orchestration
        // deterministically. The closure is called PER ATTEMPT (i.e., once
        // per retry), so test fixtures can use interior mutability
        // (Arc<AtomicUsize>, Arc<Mutex<Vec<_>>>) to return different
        // responses across attempts.
        #[cfg(test)]
        if let Some(f) = &self.test_handler {
            return f(input);
        }

        // Build the full prompt: system prompt + user prompt.
        // If the prompt contains a "Previous attempt rejected" section (added
        // by Task<Rejected>::reset()), the system prompt instructs Claude to
        // treat it as feedback on the prior plan.
        let full_prompt = format!(
            "{}\n\n---\n\nUSER PROMPT:\n{}",
            self.config.system_prompt,
            input.as_str()
        );

        // Spawn Claude Code.
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
            .map_err(|e| PlanError::ClaudeCode(format!("spawn failed: {}", e)))?;

        // Write the prompt to stdin.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| PlanError::ClaudeCode("no stdin handle".to_string()))?;
        stdin
            .write_all(full_prompt.as_bytes())
            .await
            .map_err(|e| PlanError::ClaudeCode(format!("stdin write failed: {}", e)))?;
        drop(stdin);

        // Capture output.
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| PlanError::ClaudeCode(format!("wait failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PlanError::ClaudeCode(format!(
                "exit {:?}: {}",
                output.status.code(),
                stderr.chars().take(2000).collect::<String>()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let json_str = strip_markdown_fences(&stdout);

        let parsed: ParsedPlan = serde_json::from_str(json_str)
            .map_err(|e| PlanError::Parse(format!("{}: {}", e, json_str.chars().take(500).collect::<String>())))?;

        // Schema validation
        validate_plan(&parsed)?;

        Ok(parsed.into_plan())
    }
}

impl Default for PlanAgent {
    fn default() -> Self {
        PlanAgent::new("claude-sonnet-4")
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

    async fn run(&self, input: Prompt) -> Result<Self::Output, Self::Error> {
        // Retry loop. Claude Code occasionally emits invalid JSON (e.g.,
        // trailing comma from a "story titles" field — observed 2026-07-27
        // in herdr smokes). On `PlanError::Parse`, retry up to
        // `config.max_retries` total attempts. Spawn errors and schema
        // validation errors are propagated immediately — retrying won't fix
        // those (they're deterministic for the same input).
        //
        // See the `plan_retries_on_parse_failure` test in this file for
        // the exact contract: per-attempt calls of the test_handler with
        // monotonic attempt numbering, only Parse errors retried.
        let max_attempts = self.config.max_retries.max(1) as usize;
        let mut last_err: Option<PlanError> = None;
        for attempt in 1..=max_attempts {
            match self.run_once(input.clone()).await {
                Ok(plan) => return Ok(plan),
                Err(PlanError::Parse(msg)) => {
                    elog!(
                        "[plan] parse failed (attempt {}/{}): {}",
                        attempt, max_attempts, msg
                    );
                    last_err = Some(PlanError::Parse(msg));
                }
                Err(other) => return Err(other),
            }
        }
        Err(PlanError::Parse(format!(
            "failed after {} attempts: {}",
            max_attempts,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
}

// ─────────────────────────────────────────────────────────────
// Internal: parsed JSON schema
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ParsedPlan {
    goal: String,
    architecture: String,
    stories: Vec<ParsedStory>,
    dod: Vec<ParsedDoD>,
}

#[derive(Debug, Deserialize)]
struct ParsedStory {
    id: String,
    title: String,
    description: String,
    acceptance_criteria: Vec<String>,
    priority: u32,
}

#[derive(Debug, Deserialize)]
struct ParsedDoD {
    criterion: String,
    verifiable: bool,
}

impl ParsedPlan {
    fn into_plan(self) -> Plan {
        Plan {
            id: PlanId(Uuid::new_v4()),
            goal: self.goal,
            architecture: self.architecture,
            stories: self
                .stories
                .into_iter()
                .map(|s| UserStory {
                    id: StoryId(s.id),
                    title: s.title,
                    description: s.description,
                    acceptance_criteria: s.acceptance_criteria,
                    priority: s.priority,
                })
                .collect(),
            dod: self
                .dod
                .into_iter()
                .map(|d| DefinitionOfDone {
                    criterion: d.criterion,
                    verifiable: d.verifiable,
                })
                .collect(),
        }
    }
}

fn validate_plan(p: &ParsedPlan) -> Result<(), PlanError> {
    if p.goal.trim().is_empty() {
        return Err(PlanError::Schema("goal is empty".to_string()));
    }
    if p.architecture.trim().is_empty() {
        return Err(PlanError::Schema("architecture is empty".to_string()));
    }
    if p.stories.is_empty() {
        return Err(PlanError::Schema("no stories".to_string()));
    }
    if p.dod.is_empty() {
        return Err(PlanError::Schema("no DoD criteria".to_string()));
    }
    for (i, s) in p.stories.iter().enumerate() {
        if s.id.trim().is_empty() {
            return Err(PlanError::Schema(format!("story[{}] has empty id", i)));
        }
        if s.title.trim().is_empty() {
            return Err(PlanError::Schema(format!("story[{}] has empty title", i)));
        }
        if s.acceptance_criteria.is_empty() {
            return Err(PlanError::Schema(format!(
                "story[{}] ({}) has no acceptance criteria",
                i, s.id
            )));
        }
    }
    Ok(())
}

/// Strip markdown code fences if present.
/// Claude sometimes wraps JSON in ```json ... ``` or ``` ... ```.
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
// System prompt — the load-bearing piece
// ─────────────────────────────────────────────────────────────

const PLAN_AGENT_SYSTEM_PROMPT: &str = r#"You are the ALPS Plan agent. Given a user prompt describing work to be done, produce a structured implementation plan that will be executed by an autonomous coding loop (Ralph).

Output ONLY valid JSON matching this schema exactly:

{
  "goal": "string — concise statement of the goal",
  "architecture": "string — high-level architecture description (1-3 paragraphs)",
  "stories": [
    {
      "id": "US-001",
      "title": "string — short story title",
      "description": "string — detailed description of the story",
      "acceptance_criteria": ["string — verifiable criterion", ...],
      "priority": 1
    }
  ],
  "dod": [
    {
      "criterion": "string — what 'done' looks like at the end of the loop",
      "verifiable": true
    }
  ]
}

Guidelines:
- Stories should be atomically implementable in one Ralph iteration (focused, minimal blast radius)
- Acceptance criteria must be objectively verifiable (testable, measurable, observable)
- Order stories by priority (1 = highest first)
- DoD criteria: verifiable=true for things that can be checked by running code (tests, typecheck, lint, build); verifiable=false for soft things (code quality, design choices)
- 3-8 stories is typical. If the prompt is large, break it into more stories; if small, fewer.
- If the prompt contains a "Previous attempt rejected" section, treat it as feedback on the prior plan and produce a revised plan that addresses it.

Output ONLY the JSON. No commentary, no markdown fences, no explanation. The entire response must be a single JSON object."#;

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markdown_fences_json() {
        let s = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn strip_markdown_fences_bare() {
        let s = "```\n{\"a\": 1}\n```";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn strip_markdown_fences_no_fences() {
        let s = "{\"a\": 1}";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn strip_markdown_fences_with_whitespace() {
        let s = "  \n```json\n{\"a\": 1}\n```\n  ";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn parse_plan_json() {
        let json = r#"{
            "goal": "build a CLI",
            "architecture": "Rust binary with clap",
            "stories": [
                {
                    "id": "US-001",
                    "title": "set up project",
                    "description": "cargo init + add deps",
                    "acceptance_criteria": ["cargo build succeeds"],
                    "priority": 1
                }
            ],
            "dod": [
                {"criterion": "all tests pass", "verifiable": true}
            ]
        }"#;
        let parsed: ParsedPlan = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.goal, "build a CLI");
        assert_eq!(parsed.stories.len(), 1);
        assert_eq!(parsed.stories[0].id, "US-001");
        assert_eq!(parsed.dod.len(), 1);
        assert!(parsed.dod[0].verifiable);
    }

    #[test]
    fn validate_empty_goal() {
        let p = ParsedPlan {
            goal: "".to_string(),
            architecture: "test".to_string(),
            stories: vec![ParsedStory {
                id: "US-001".to_string(),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec!["test".to_string()],
                priority: 1,
            }],
            dod: vec![ParsedDoD {
                criterion: "test".to_string(),
                verifiable: true,
            }],
        };
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_no_stories() {
        let p = ParsedPlan {
            goal: "test".to_string(),
            architecture: "test".to_string(),
            stories: vec![],
            dod: vec![ParsedDoD {
                criterion: "test".to_string(),
                verifiable: true,
            }],
        };
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_no_dod() {
        let p = ParsedPlan {
            goal: "test".to_string(),
            architecture: "test".to_string(),
            stories: vec![ParsedStory {
                id: "US-001".to_string(),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec!["test".to_string()],
                priority: 1,
            }],
            dod: vec![],
        };
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_story_missing_criteria() {
        let p = ParsedPlan {
            goal: "test".to_string(),
            architecture: "test".to_string(),
            stories: vec![ParsedStory {
                id: "US-001".to_string(),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec![],
                priority: 1,
            }],
            dod: vec![ParsedDoD {
                criterion: "test".to_string(),
                verifiable: true,
            }],
        };
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn parsed_plan_to_plan() {
        let parsed = ParsedPlan {
            goal: "test".to_string(),
            architecture: "test".to_string(),
            stories: vec![ParsedStory {
                id: "US-001".to_string(),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec!["test".to_string()],
                priority: 1,
            }],
            dod: vec![ParsedDoD {
                criterion: "tests pass".to_string(),
                verifiable: true,
            }],
        };
        let plan = parsed.into_plan();
        assert_eq!(plan.goal, "test");
        assert_eq!(plan.stories.len(), 1);
        assert_eq!(plan.stories[0].id, StoryId("US-001".to_string()));
        assert_eq!(plan.dod.len(), 1);
        assert!(plan.dod[0].verifiable);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Retry-on-parse-failure tests
    // ─────────────────────────────────────────────────────────────────────
    //
    // Background: Claude Code (the Plan LLM) occasionally emits invalid JSON
    // (trailing comma, etc.) — observed 2026-07-27 in herdr smokes. Before the
    // retry, a single bad emission killed the run. The retry loop in
    // `PlanAgent::run` retries up to `config.max_retries` total attempts when
    // `run_once` returns `PlanError::Parse`. These tests verify the contract
    // deterministically by using `for_test` + interior-mutability to return
    // different responses across attempts.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Build a fresh Plan with a unique PlanId for testing.
    fn test_plan() -> Plan {
        Plan {
            id: PlanId(Uuid::new_v4()),
            goal: "test goal".to_string(),
            architecture: "test".to_string(),
            stories: vec![UserStory {
                id: StoryId("US-001".to_string()),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec!["test".to_string()],
                priority: 1,
            }],
            dod: vec![DefinitionOfDone {
                criterion: "tests pass".to_string(),
                verifiable: true,
            }],
        }
    }

    #[tokio::test]
    async fn plan_retries_on_parse_failure() {
        // First 2 calls return Parse errors; 3rd call returns Ok(plan).
        // With default max_retries=3, the loop should retry and succeed.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(PlanError::Parse(format!("simulated bad JSON on attempt {}", n + 1)))
            } else {
                Ok(test_plan())
            }
        });

        let result = plan.run(Prompt::new("test")).await;

        // The 3rd attempt should succeed.
        let plan_out = result.expect("plan should succeed on 3rd attempt");
        assert_eq!(plan_out.goal, "test goal");

        // The closure was called exactly 3 times (2 Parse errors + 1 Ok).
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn plan_gives_up_after_max_retries() {
        // All 3 calls return Parse errors. After max_retries attempts,
        // run() should return a final Parse error wrapping the last failure.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| {
            let n = calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(PlanError::Parse(format!("simulated bad JSON on attempt {}", n + 1)))
        });

        let result = plan.run(Prompt::new("test")).await;

        // No plan returned.
        let err = result.expect_err("plan should fail after max_retries");
        let msg = err.to_string();
        assert!(
            msg.contains("failed after 3 attempts"),
            "expected 'failed after 3 attempts' in error, got: {}",
            msg
        );

        // The closure was called exactly 3 times (max_retries).
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn plan_no_retry_on_first_success() {
        // The closure returns Ok on the first call. No retries.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Ok(test_plan())
        });

        let result = plan.run(Prompt::new("test")).await;

        let _plan = result.expect("plan should succeed");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retries on first success");
    }

    #[tokio::test]
    async fn plan_does_not_retry_on_spawn_error() {
        // Non-Parse errors (e.g. ClaudeCode) propagate immediately, no retry.
        // This is important: retrying a spawn error won't help (deterministic
        // for the same input).
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let plan = PlanAgent::for_test(move |_input: Prompt| {
            calls_for_closure.fetch_add(1, Ordering::SeqCst);
            Err(PlanError::ClaudeCode("spawn failed".to_string()))
        });

        let result = plan.run(Prompt::new("test")).await;

        let err = result.expect_err("spawn error should fail without retry");
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
    async fn plan_max_retries_1_means_no_retry() {
        // max_retries=1: only the original attempt. Parse error → fail.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let mut config = PlanConfig::default();
        config.max_retries = 1;
        let plan = PlanAgent::with_config(config);
        // We need for_test but with custom config — use the public field.
        // The test_handler field is pub(crate), but for tests in the same
        // crate, we can set it via the constructor. We'll re-construct.
        drop(plan);
        let calls_for_closure_2 = calls.clone();
        let plan = PlanAgent {
            config: PlanConfig {
                max_retries: 1,
                ..PlanConfig::default()
            },
            test_handler: Some(Arc::new(move |_input: Prompt| {
                calls_for_closure_2.fetch_add(1, Ordering::SeqCst);
                Err(PlanError::Parse("only attempt fails".to_string()))
            })),
        };

        let result = plan.run(Prompt::new("test")).await;
        let err = result.expect_err("max_retries=1 should fail on first Parse error");
        assert!(
            err.to_string().contains("failed after 1 attempts"),
            "expected 'failed after 1 attempts' in error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry with max_retries=1");
    }
}
