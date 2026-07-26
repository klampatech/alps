//! Implement agent — Ralph (Codex loop).
//!
//! Consumes a `Plan`, produces an `Implementation`. ALPS treats Ralph as a
//! black-box subprocess:
//!
//! 1. Set up `tasks/<id>/implementation/ralph/` as Ralph's working directory
//! 2. Initialize git, copy ralph.sh + CLAUDE.md, write prd.json + progress.txt
//! 3. Run `ralph.sh --tool claude --max-iters N` (inherits stdout/stderr)
//! 4. Read back prd.json (with passes:true), progress.txt, git log
//! 5. Return typed `Implementation`
//!
//! See `SPEC.md` §2.1 for the compose boundary.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

use crate::agent::{Agent, sealed};
use crate::domain::{Artifact, ArtifactKind, Commit, Implementation, Plan};

#[derive(Debug, Error)]
pub enum ImplementError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to setup ralph dir: {0}")]
    RalphSetup(String),

    #[error("failed to {op}: {msg}")]
    Ralph { op: String, msg: String },

    #[error("git operation failed: {0}")]
    Git(String),

    #[error("failed to parse prd.json: {0}")]
    PrdParse(String),

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Config for the Implement agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementConfig {
    /// Path to the `ralph.sh` script (vendored or user-provided).
    pub ralph_path: PathBuf,
    /// Path to the `CLAUDE.md` (Ralph's prompt file for Claude Code).
    pub claude_prompt_path: PathBuf,
    /// Max Ralph iterations before giving up.
    pub max_iterations: u32,
    /// Tool to use (default: "claude").
    pub tool: String,
    /// Optional init command to run before Ralph (e.g. "cargo init --name foo").
    pub init_command: Option<String>,
}

impl Default for ImplementConfig {
    fn default() -> Self {
        ImplementConfig {
            ralph_path: PathBuf::from("./scripts/ralph.sh"),
            claude_prompt_path: PathBuf::from("./scripts/CLAUDE.md"),
            max_iterations: 20,
            tool: "claude".to_string(),
            init_command: None,
        }
    }
}

/// Implement agent — invokes Ralph.
pub struct ImplementAgent {
    pub config: ImplementConfig,
    /// The workspace root (`tasks/<id>/`). Used to compute the Ralph working dir.
    pub workspace_root: PathBuf,
}

impl ImplementAgent {
    pub fn new(workspace_root: PathBuf, config: ImplementConfig) -> Self {
        ImplementAgent { config, workspace_root }
    }

    pub fn ralph_dir(&self) -> PathBuf {
        self.workspace_root.join("implementation").join("ralph")
    }

    /// Derive the task id from the workspace root (the basename of `tasks/<id>`).
    pub fn task_id(&self) -> String {
        self.workspace_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("alps")
            .to_string()
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

    async fn run(&self, input: Plan) -> Result<Self::Output, Self::Error> {
        let ralph_dir = self.ralph_dir();
        let task_id = self.task_id();

        // ── 1. Set up Ralph working directory ──
        std::fs::create_dir_all(&ralph_dir).map_err(|e| ImplementError::RalphSetup(format!(
            "create_dir_all: {}", e
        )))?;

        // ── 2. Initialize git repo ──
        run_git(&ralph_dir, &["init"])?;
        run_git(&ralph_dir, &["config", "user.email", "alps@local"])?;
        run_git(&ralph_dir, &["config", "user.name", "ALPS"])?;
        run_git(&ralph_dir, &["config", "commit.gpgsign", "false"])?;

        // ── 3. Copy ralph.sh + CLAUDE.md into the working dir ──
        copy_ralph_files(&self.config.ralph_path, &self.config.claude_prompt_path, &ralph_dir)?;

        // ── 4. Generate prd.json from Plan ──
        let prd = plan_to_prd(&task_id, &input);
        let prd_json = serde_json::to_string_pretty(&prd)?;
        std::fs::write(ralph_dir.join("prd.json"), prd_json)?;

        // ── 5. Initialize progress.txt ──
        std::fs::write(ralph_dir.join("progress.txt"), "## Codebase Patterns\n")?;

        // ── 6. Optional init command ──
        if let Some(cmd) = &self.config.init_command {
            run_shell(&ralph_dir, cmd).await?;
        }

        // ── 7. Initial commit on main, then branch ──
        run_git(&ralph_dir, &["add", "-A"])?;
        run_git(&ralph_dir, &["commit", "-m", "alps: initial setup"])?;
        let branch = prd.branch_name.clone();
        run_git(&ralph_dir, &["checkout", "-b", &branch])?;

        // ── 8. Invoke Ralph ──
        eprintln!(
            "[implement] invoking Ralph: tool={}, max_iterations={}, stories={}",
            self.config.tool, self.config.max_iterations, prd.user_stories.len()
        );
        let ralph_status = Command::new(&self.config.ralph_path)
            .args([
                "--tool", &self.config.tool,
                &self.config.max_iterations.to_string(),
            ])
            .current_dir(&ralph_dir)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .map_err(|e| ImplementError::Ralph {
                op: "spawn ralph".to_string(),
                msg: e.to_string(),
            })?;

        if !ralph_status.success() {
            return Err(ImplementError::Ralph {
                op: "ralph exited".to_string(),
                msg: format!("code: {:?}", ralph_status.code()),
            });
        }

        // ── 9. Read back results ──
        let prd_text = std::fs::read_to_string(ralph_dir.join("prd.json"))?;
        let prd_after: RalphPrd = serde_json::from_str(&prd_text)
            .map_err(|e| ImplementError::PrdParse(format!("{}: {}", e, prd_text.chars().take(500).collect::<String>())))?;

        let commits = read_commits(&ralph_dir)?;
        let artifacts = read_artifacts(&ralph_dir)?;

        // Count stories that Ralph marked as passed
        let stories_passed = prd_after.user_stories.iter().filter(|s| s.passes).count() as u32;
        let stories_total = prd_after.user_stories.len() as u32;

        eprintln!(
            "[implement] done: {}/{} stories passed, {} commits, {} artifacts",
            stories_passed, stories_total, commits.len(), artifacts.len()
        );

        // Implement metrics live in the Implementation struct's prd_path, but we
        // also need to surface stories/iterations to the receipts. Receipts come
        // from the Judge, which can read these from the Implementation or the
        // persisted prd.json. For now, we expose stories via the artifacts list
        // (prd.json is included).
        let _ = (stories_passed, stories_total); // used for logging only

        Ok(Implementation {
            ralph_branch: prd_after.branch_name,
            prd_path: ralph_dir.join("prd.json"),
            commits,
            artifacts,
        })
    }
}

// ─────────────────────────────────────────────────────────────
// Ralph prd.json schema (1:1 with snarktank/ralph)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RalphPrd {
    project: String,
    #[serde(rename = "branchName")]
    branch_name: String,
    description: String,
    #[serde(rename = "userStories")]
    user_stories: Vec<RalphStory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RalphStory {
    id: String,
    title: String,
    description: String,
    #[serde(rename = "acceptanceCriteria")]
    acceptance_criteria: Vec<String>,
    priority: u32,
    passes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

fn plan_to_prd(task_id: &str, plan: &Plan) -> RalphPrd {
    let user_stories = plan
        .stories
        .iter()
        .map(|s| RalphStory {
            id: s.id.0.clone(),
            title: s.title.clone(),
            description: s.description.clone(),
            acceptance_criteria: s.acceptance_criteria.clone(),
            priority: s.priority,
            passes: false,
            notes: None,
        })
        .collect();

    RalphPrd {
        project: format!("alps-{}", task_id),
        branch_name: format!("alps/{}", task_id),
        description: plan.goal.clone(),
        user_stories,
    }
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

fn run_git(dir: &Path, args: &[&str]) -> Result<(), ImplementError> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| ImplementError::Git(format!("git {} spawn: {}", args.join(" "), e)))?;
    if !status.success() {
        return Err(ImplementError::Git(format!(
            "git {} failed: {:?}",
            args.join(" "),
            status.code()
        )));
    }
    Ok(())
}

async fn run_shell(dir: &Path, cmd: &str) -> Result<(), ImplementError> {
    let status = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .map_err(|e| ImplementError::RalphSetup(format!("shell spawn: {}", e)))?;
    if !status.success() {
        return Err(ImplementError::RalphSetup(format!(
            "shell command failed: {:?}",
            status.code()
        )));
    }
    Ok(())
}

fn copy_ralph_files(
    ralph_src: &Path,
    claude_src: &Path,
    ralph_dir: &Path,
) -> Result<(), ImplementError> {
    use std::os::unix::fs::PermissionsExt;

    // Copy ralph.sh
    let ralph_dst = ralph_dir.join("ralph.sh");
    std::fs::copy(ralph_src, &ralph_dst).map_err(|e| {
        ImplementError::RalphSetup(format!("copy ralph.sh from {:?}: {}", ralph_src, e))
    })?;
    std::fs::set_permissions(&ralph_dst, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| ImplementError::RalphSetup(format!("chmod ralph.sh: {}", e)))?;

    // Copy CLAUDE.md (if it exists)
    if claude_src.exists() {
        let claude_dst = ralph_dir.join("CLAUDE.md");
        std::fs::copy(claude_src, &claude_dst).map_err(|e| {
            ImplementError::RalphSetup(format!("copy CLAUDE.md from {:?}: {}", claude_src, e))
        })?;
    }

    Ok(())
}

fn read_commits(ralph_dir: &Path) -> Result<Vec<Commit>, ImplementError> {
    let output = std::process::Command::new("git")
        .args(["log", "--pretty=format:%H|%s"])
        .current_dir(ralph_dir)
        .output()
        .map_err(|e| ImplementError::Git(format!("git log: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commits: Vec<Commit> = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() == 2 {
                Some(Commit {
                    sha: parts[0].to_string(),
                    message: parts[1].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(commits)
}

fn read_artifacts(ralph_dir: &Path) -> Result<Vec<Artifact>, ImplementError> {
    let mut artifacts = Vec::new();
    let entries = std::fs::read_dir(ralph_dir)
        .map_err(|e| ImplementError::RalphSetup(format!("read_dir: {}", e)))?;

    // Skip files we added ourselves
    const SKIP: &[&str] = &["ralph.sh", "CLAUDE.md", "prd.json", "progress.txt", ".git"];

    for entry in entries {
        let entry = entry.map_err(|e| ImplementError::RalphSetup(format!("entry: {}", e)))?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if SKIP.contains(&name) || name.starts_with('.') {
            continue;
        }

        let kind = if name.ends_with(".rs") || name.ends_with(".py") || name.ends_with(".js")
            || name.ends_with(".ts") || name.ends_with(".go") {
            ArtifactKind::Source
        } else if name.ends_with("_test.rs") || name.ends_with(".test.") || name.ends_with("test_") {
            ArtifactKind::Test
        } else if name.ends_with(".md") {
            ArtifactKind::Doc
        } else if name.ends_with(".toml") || name.ends_with(".json") || name.ends_with(".yaml")
            || name.ends_with(".yml") || name == "Cargo.lock" || name == "package.json" {
            ArtifactKind::Config
        } else {
            ArtifactKind::Other(name.to_string())
        };

        let rel = path
            .strip_prefix(ralph_dir)
            .unwrap_or(&path)
            .to_path_buf();

        artifacts.push(Artifact { path: rel, kind });
    }

    Ok(artifacts)
}

// Helper for the error chain
trait ToRalph {
    fn to_ralph(self) -> ImplementError;
}

impl ToRalph for ImplementError {
    fn to_ralph(self) -> ImplementError {
        self
    }
}

fn with_op(op: &str) -> ImplementError {
    ImplementError::Ralph {
        op: op.to_string(),
        msg: String::new(),
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PlanId, StoryId, UserStory, DefinitionOfDone};
    use uuid::Uuid;

    fn dummy_plan() -> Plan {
        Plan {
            id: PlanId(Uuid::new_v4()),
            goal: "build a CLI".to_string(),
            architecture: "rust binary".to_string(),
            stories: vec![
                UserStory {
                    id: StoryId("US-001".to_string()),
                    title: "set up project".to_string(),
                    description: "cargo init".to_string(),
                    acceptance_criteria: vec!["cargo build succeeds".to_string()],
                    priority: 1,
                },
                UserStory {
                    id: StoryId("US-002".to_string()),
                    title: "implement core".to_string(),
                    description: "main logic".to_string(),
                    acceptance_criteria: vec!["tests pass".to_string()],
                    priority: 2,
                },
            ],
            dod: vec![DefinitionOfDone {
                criterion: "all tests pass".to_string(),
                verifiable: true,
            }],
        }
    }

    #[test]
    fn plan_to_prd_matches_ralph_schema() {
        let plan = dummy_plan();
        let prd = plan_to_prd("2026-07-26T120000-abc", &plan);

        // Match the exact serde shape Ralph expects
        assert_eq!(prd.project, "alps-2026-07-26T120000-abc");
        assert_eq!(prd.branch_name, "alps/2026-07-26T120000-abc");
        assert_eq!(prd.description, "build a CLI");
        assert_eq!(prd.user_stories.len(), 2);

        // Roundtrip through serde to verify the JSON keys match Ralph's schema
        let json = serde_json::to_string(&prd).unwrap();
        assert!(json.contains("\"branchName\""));
        assert!(json.contains("\"userStories\""));
        assert!(json.contains("\"acceptanceCriteria\""));
        assert!(json.contains("\"passes\":false"));
        assert!(json.contains("\"priority\":1"));
        assert!(json.contains("\"priority\":2"));
    }

    #[test]
    fn task_id_from_workspace_root() {
        let agent = ImplementAgent::new(
            PathBuf::from("/home/kyle/Development/alps/tasks/2026-07-26T120000-abc"),
            ImplementConfig::default(),
        );
        assert_eq!(agent.task_id(), "2026-07-26T120000-abc");
        assert_eq!(
            agent.ralph_dir(),
            PathBuf::from("/home/kyle/Development/alps/tasks/2026-07-26T120000-abc/implementation/ralph")
        );
    }

    #[test]
    fn classification_of_artifacts() {
        // Just exercise the classify logic via a real ralph dir
        let tmp = std::env::temp_dir().join("alps-test-artifacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(tmp.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(tmp.join("main_test.rs"), "#[test] fn t() {}").unwrap();
        std::fs::write(tmp.join("README.md"), "# readme").unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(tmp.join("data.txt"), "data").unwrap();
        std::fs::write(tmp.join(".gitignore"), "target").unwrap();

        let artifacts = read_artifacts(&tmp).unwrap();

        assert!(artifacts.iter().any(|a| a.path == PathBuf::from("main.rs")));
        assert!(artifacts.iter().any(|a| a.path == PathBuf::from("main_test.rs")));
        assert!(artifacts.iter().any(|a| a.path == PathBuf::from("README.md")));
        assert!(artifacts.iter().any(|a| a.path == PathBuf::from("Cargo.toml")));
        assert!(artifacts.iter().any(|a| a.path == PathBuf::from("data.txt")));
        assert!(!artifacts.iter().any(|a| a.path.to_string_lossy().contains(".gitignore")));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
