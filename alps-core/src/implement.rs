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
use crate::receipt::ImplementMetrics;

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

/// Ralph tool backend. The implement agent wraps ralph.sh, which dispatches
/// to one of these backends per iteration. Default is `Codex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RalphTool {
    /// Claude Code via `claude --dangerously-skip-permissions`. Reads CLAUDE.md.
    Claude,
    /// OpenAI Codex via `codex exec --dangerously-bypass-approvals-and-sandbox`. Reads AGENTS.md.
    Codex,
    /// Sourcegraph Amp. Legacy/default in upstream ralph.sh.
    Amp,
}

impl RalphTool {
    /// CLI flag value passed to ralph.sh (`--tool <name>`).
    pub fn as_str(&self) -> &'static str {
        match self {
            RalphTool::Claude => "claude",
            RalphTool::Codex => "codex",
            RalphTool::Amp => "amp",
        }
    }

    /// Prompt filename vendored alongside ralph.sh.
    pub fn prompt_filename(&self) -> &'static str {
        match self {
            RalphTool::Claude => "CLAUDE.md",
            RalphTool::Codex => "AGENTS.md",
            RalphTool::Amp => "prompt.md",
        }
    }
}

impl std::fmt::Display for RalphTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for RalphTool {
    fn default() -> Self {
        RalphTool::Codex
    }
}

/// Config for the Implement agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementConfig {
    /// Path to the `ralph.sh` script (vendored or user-provided).
    pub ralph_path: PathBuf,
    /// Path to the `CLAUDE.md` (Ralph's prompt file for Claude Code).
    pub claude_prompt_path: PathBuf,
    /// Path to the `AGENTS.md` (Ralph's prompt file for Codex).
    #[serde(default = "default_agents_prompt_path")]
    pub agents_prompt_path: PathBuf,
    /// Max Ralph iterations before giving up.
    pub max_iterations: u32,
    /// Tool backend for Ralph. Default: `codex`.
    pub tool: RalphTool,
    /// Optional init command to run before Ralph (e.g. "cargo init --name foo").
    pub init_command: Option<String>,
}

fn default_agents_prompt_path() -> PathBuf {
    PathBuf::from("./scripts/AGENTS.md")
}

impl Default for ImplementConfig {
    fn default() -> Self {
        ImplementConfig {
            ralph_path: PathBuf::from("./scripts/ralph.sh"),
            claude_prompt_path: PathBuf::from("./scripts/CLAUDE.md"),
            agents_prompt_path: default_agents_prompt_path(),
            max_iterations: 20,
            tool: RalphTool::default(),
            init_command: None,
        }
    }
}

/// Implement agent — invokes Ralph.
pub struct ImplementAgent {
    pub config: ImplementConfig,
    /// The workspace root (`tasks/<id>/`). Used to compute the Ralph working dir.
    pub workspace_root: PathBuf,
    /// Test-only override: when set, `run()` calls this closure instead of
    /// spawning Ralph. Used by `drive_*` integration tests in `loop_::tests`
    /// to deterministically exercise the orchestration.
    #[cfg(test)]
    pub(crate) test_handler:
        Option<std::sync::Arc<dyn Fn(Plan) -> Result<Implementation, ImplementError> + Send + Sync>>,
}

impl ImplementAgent {
    pub fn new(workspace_root: PathBuf, config: ImplementConfig) -> Self {
        ImplementAgent {
            config,
            workspace_root,
            #[cfg(test)]
            test_handler: None,
        }
    }

    /// Test-only constructor that bypasses Ralph. The closure receives the
    /// input plan and returns a canned (or computed) `Implementation`.
    #[cfg(test)]
    pub fn for_test<F>(workspace_root: PathBuf, f: F) -> Self
    where
        F: Fn(Plan) -> Result<Implementation, ImplementError> + Send + Sync + 'static,
    {
        ImplementAgent {
            config: ImplementConfig::default(),
            workspace_root,
            test_handler: Some(std::sync::Arc::new(f)),
        }
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
        // Test-only fast path: if a test_handler is set, use it instead of
        // spawning Ralph. This lets integration tests in `loop_::tests`
        // exercise the orchestration deterministically.
        #[cfg(test)]
        if let Some(f) = &self.test_handler {
            return f(input);
        }

        let ralph_dir = self.ralph_dir();
        let task_id = self.task_id();

        // ── 1. Set up Ralph working directory ──
        std::fs::create_dir_all(&ralph_dir).map_err(|e| ImplementError::RalphSetup(format!(
            "create_dir_all: {}", e
        )))?;

        // ── 2. Initialize git repo (idempotent) ──
        // Re-using the ralph dir across outer loop iterations is intentional —
        // we want progress.txt and prior commits to survive. `git init` is a
        // no-op if the dir is already a repo, and we use `git checkout` (not
        // `-b`) for the branch step so retries don't blow up.
        run_git(&ralph_dir, &["init"])?;
        run_git(&ralph_dir, &["config", "user.email", "alps@local"])?;
        run_git(&ralph_dir, &["config", "user.name", "ALPS"])?;
        run_git(&ralph_dir, &["config", "commit.gpgsign", "false"])?;

        // ── 3. Copy ralph.sh + CLAUDE.md + AGENTS.md into the working dir ──
        copy_ralph_files(
            &self.config.ralph_path,
            &self.config.claude_prompt_path,
            &self.config.agents_prompt_path,
            self.config.tool,
            &ralph_dir,
        )?;

        // Write a sensible .gitignore so Ralph's auto-generated junk (pycache,
        // node_modules, target/, etc.) doesn't pollute commits.
        let gitignore = ralph_dir.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(
                &gitignore,
                "\
__pycache__/
*.pyc
*.pyo
node_modules/
target/
.DS_Store
.venv/
venv/
",
            )?;
        }

        // ── 4. Generate prd.json from Plan (always rewrite — plan may have been updated) ──
        let prd = plan_to_prd(&task_id, &input);
        let prd_json = serde_json::to_string_pretty(&prd)?;
        std::fs::write(ralph_dir.join("prd.json"), prd_json)?;

        // ── 5. Initialize progress.txt ONLY on first run (preserve across retries) ──
        let progress_path = ralph_dir.join("progress.txt");
        if !progress_path.exists() {
            std::fs::write(&progress_path, "## Codebase Patterns\n")?;
        }

        // ── 6. Optional init command ──
        if let Some(cmd) = &self.config.init_command {
            run_shell(&ralph_dir, cmd).await?;
        }

        // ── 7. Initial commit on main, then branch (idempotent) ──
        // First commit: only if there's no commit yet on main. If we re-enter,
        // the previous run's commits and branch are already there.
        let branch = prd.branch_name.clone();
        let branch_exists = run_git(&ralph_dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", branch)])
            .is_ok();

        if !branch_exists {
            // Check whether we have any commits at all
            let has_commits = run_git(&ralph_dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
            if !has_commits {
                run_git(&ralph_dir, &["add", "-A"])?;
                run_git(&ralph_dir, &["commit", "-m", "alps: initial setup"])?;
            }
            // Make sure we're on main before creating the branch
            run_git(&ralph_dir, &["checkout", "main"]).or_else(|_| run_git(&ralph_dir, &["checkout", "-b", "main"]))?;
            run_git(&ralph_dir, &["checkout", "-b", &branch])?;
        } else {
            // Branch exists from a prior iteration — just switch to it
            run_git(&ralph_dir, &["checkout", &branch])?;
        }

        // ── 8. Invoke Ralph ──
        eprintln!(
            "[implement] invoking Ralph: tool={}, max_iterations={}, stories={}",
            self.config.tool, self.config.max_iterations, prd.user_stories.len()
        );
        let ralph_status = Command::new(&self.config.ralph_path)
            .args([
                "--tool", self.config.tool.as_str(),
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

        // Read ralph.sh's own metrics (iterations, elapsed_secs) so receipts
        // show real numbers, not zeros.
        let ralph_result = read_ralph_result(&ralph_dir)?;

        eprintln!(
            "[implement] done: {}/{} stories passed, {} commits, {} artifacts, {} iterations, {}s elapsed",
            stories_passed, stories_total, commits.len(), artifacts.len(),
            ralph_result.iterations, ralph_result.elapsed_secs
        );

        Ok(Implementation {
            ralph_branch: prd_after.branch_name,
            prd_path: ralph_dir.join("prd.json"),
            commits,
            artifacts,
            metrics: ImplementMetrics {
                stories_passed,
                stories_total,
                iterations: ralph_result.iterations,
                elapsed_secs: ralph_result.elapsed_secs,
            },
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
    #[serde(default)]
    priority: u32,
    #[serde(default)]
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
    agents_src: &Path,
    tool: RalphTool,
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

    // Copy the prompt file that matches the chosen tool.
    // Always copy both AGENTS.md and CLAUDE.md when they exist — Ralph will
    // only read the one it needs, but the file presence keeps ralph.sh happy.
    for src in [claude_src, agents_src] {
        if !src.exists() {
            continue;
        }
        let fname = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ImplementError::RalphSetup(format!("bad prompt path: {:?}", src)))?;
        let dst = ralph_dir.join(fname);
        std::fs::copy(src, &dst).map_err(|e| {
            ImplementError::RalphSetup(format!("copy {} from {:?}: {}", fname, src, e))
        })?;
    }

    let _ = tool; // tool choice is communicated via --tool flag, not file content
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
    const SKIP: &[&str] = &[
        "ralph.sh",
        "CLAUDE.md",
        "AGENTS.md",
        "prd.json",
        "progress.txt",
        ".codex-last-message.txt",
        ".ralph-result.json",
        ".last-branch",
        ".git",
    ];

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
// Ralph .ralph-result.json (written by ralph.sh on every run)
// ─────────────────────────────────────────────────────────────

/// What ralph.sh writes to `.ralph-result.json` so alps can report real
/// iteration counts and elapsed time in receipts (instead of guessing).
/// All fields default — ralph.sh may write only some of them (e.g. if it
/// crashed mid-run, elapsed_secs may be missing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RalphResult {
    #[serde(default)]
    pub iterations: u32,
    #[serde(default)]
    pub elapsed_secs: u64,
    #[serde(default)]
    pub completed: bool,
}

/// Read `.ralph-result.json` from the ralph workspace. If the file is
/// missing (older ralph.sh, crash before write), return `Default::default()`
/// — the receipts will honestly show 0 rather than fabricate numbers.
pub fn read_ralph_result(ralph_dir: &Path) -> Result<RalphResult, ImplementError> {
    let path = ralph_dir.join(".ralph-result.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            ImplementError::PrdParse(format!(".ralph-result.json invalid: {}: {}", e, text))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RalphResult::default()),
        Err(e) => Err(ImplementError::RalphSetup(format!(
            "read .ralph-result.json: {}",
            e
        ))),
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

    /// Tiny test helper — create a uniquely-named temp dir under `/tmp` without
    /// pulling in the `tempfile` crate just for this. Returns the dir path.
    fn tempdir_via_tmp(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = PathBuf::from(format!("/tmp/alps-test-{}-{}{}", label, pid, nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
    fn ralph_result_parses_full_json() {
        // Happy path: ralph.sh writes a complete .ralph-result.json on success.
        let dir = tempdir_via_tmp("alps-ralph-result-ok");
        std::fs::write(
            dir.join(".ralph-result.json"),
            r#"{"iterations": 3, "elapsed_secs": 184, "completed": true}"#,
        )
        .unwrap();
        let r = read_ralph_result(&dir).unwrap();
        assert_eq!(r.iterations, 3);
        assert_eq!(r.elapsed_secs, 184);
        assert!(r.completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ralph_result_missing_file_returns_zeros() {
        // Backward-compat: if ralph.sh didn't write the file (older version,
        // crashed before writing, etc.), we should NOT error — we should return
        // a default. The receipts will show 0, which is honest "we don't know".
        let dir = tempdir_via_tmp("alps-ralph-result-missing");
        let r = read_ralph_result(&dir).unwrap();
        assert_eq!(r.iterations, 0);
        assert_eq!(r.elapsed_secs, 0);
        assert!(!r.completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ralph_result_partial_json_uses_defaults() {
        // ralph.sh might write only some fields. Missing fields default to
        // 0/false rather than erroring.
        let dir = tempdir_via_tmp("alps-ralph-result-partial");
        std::fs::write(dir.join(".ralph-result.json"), r#"{"iterations": 5}"#).unwrap();
        let r = read_ralph_result(&dir).unwrap();
        assert_eq!(r.iterations, 5);
        assert_eq!(r.elapsed_secs, 0);
        assert!(!r.completed);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn ralph_tool_default_is_codex() {
        // ALPS defaults to codex so the implement loop uses OpenAI Codex.
        assert_eq!(RalphTool::default(), RalphTool::Codex);
        assert_eq!(ImplementConfig::default().tool, RalphTool::Codex);
    }

    #[test]
    fn ralph_tool_as_str() {
        assert_eq!(RalphTool::Claude.as_str(), "claude");
        assert_eq!(RalphTool::Codex.as_str(), "codex");
        assert_eq!(RalphTool::Amp.as_str(), "amp");
    }

    #[test]
    fn ralph_tool_prompt_filename() {
        assert_eq!(RalphTool::Claude.prompt_filename(), "CLAUDE.md");
        assert_eq!(RalphTool::Codex.prompt_filename(), "AGENTS.md");
        assert_eq!(RalphTool::Amp.prompt_filename(), "prompt.md");
    }

    #[test]
    fn ralph_tool_serializes_lowercase() {
        // The on-disk config files use lowercase tool names. Verify roundtrip.
        for tool in [RalphTool::Claude, RalphTool::Codex, RalphTool::Amp] {
            let json = serde_json::to_string(&tool).unwrap();
            let back: RalphTool = serde_json::from_str(&json).unwrap();
            assert_eq!(tool, back);
        }
        assert_eq!(serde_json::to_string(&RalphTool::Codex).unwrap(), "\"codex\"");
    }

    #[test]
    fn ralph_tool_display_matches_as_str() {
        // Display is what eprintln uses in the implement log line.
        for tool in [RalphTool::Claude, RalphTool::Codex, RalphTool::Amp] {
            assert_eq!(format!("{}", tool), tool.as_str());
        }
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
