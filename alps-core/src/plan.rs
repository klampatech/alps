use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent::{Agent, sealed};
use crate::domain::{
    DefinitionOfDone, Plan, PlanId, Prompt, StoryId, UserStory,
};
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
}

impl Default for PlanConfig {
    fn default() -> Self {
        PlanConfig {
            claude_path: "claude".to_string(),
            model: "claude-sonnet-4".to_string(),
            system_prompt: PLAN_AGENT_SYSTEM_PROMPT.to_string(),
        }
    }
}

/// Plan agent — invokes Claude Code with a structured system prompt,
/// parses the JSON output, and returns a typed `Plan`.
pub struct PlanAgent {
    pub config: PlanConfig,
}

impl PlanAgent {
    pub fn new(model: impl Into<String>) -> Self {
        PlanAgent {
            config: PlanConfig {
                model: model.into(),
                ..Default::default()
            },
        }
    }

    pub fn with_config(config: PlanConfig) -> Self {
        PlanAgent { config }
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
            .map_err(|e| PlanError::Parse(format!("{}: {}", e, json_str.chars().take(500).collect::<String>())))
            .map_err(|e| PlanError::Parse(e.to_string()))?;

        // Schema validation
        validate_plan(&parsed)?;

        Ok(parsed.into_plan())
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
}
