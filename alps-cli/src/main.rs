//! ALPS CLI — `alps run "prompt"`.
//!
//! MVP behavior:
//!   1. Create `tasks/<task-id>/` workspace under the given workdir
//!   2. Initialize `Task<Idle>` with the prompt
//!   3. Drive the outer loop (Plan → Implement → Review → Judge)
//!   4. Persist + git commit at each state
//!   5. On Done, print markdown summary to stdout and write `receipts.json`
//!   6. Exit 0 on Done, 1 on AlpsError, 2 on Failed
//!
//! Resolves 2026-07-26:
//!   - Hybrid judge (structured DoD + LLM) — wired MVP stubs
//!   - Unbounded loop — no max attempts
//!   - stdout notifications — no Discord/polling
//!   - Markdown + JSON receipts — terminal summary + receipts.json

use std::path::PathBuf;
use std::sync::Arc;

use alps_core::domain::{Prompt, TaskId};
use alps_core::git_ops::{commit_smart, CommitOutcome, GitOpsError};
use alps_core::implement::ImplementAgent;
use alps_core::judge::{DoDRunner, HermesLlmJudge, JudgeAgent};
use alps_core::loop_::drive;
use alps_core::persistence::TaskWorkspace;
use alps_core::plan::PlanAgent;
use alps_core::receipt::Receipts;
use alps_core::review::ReviewAgent;
use alps_core::task::{Done, Task};
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(name = "alps", version, about = "Agentic Loop Programming System")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a new task with the given prompt.
    Run {
        /// The prompt describing the work to do.
        prompt: String,

        /// Working directory (defaults to current directory).
        #[arg(long, default_value = ".")]
        workdir: String,
    },

    /// List tasks in the current workspace.
    List,

    /// Show receipts for a given task.
    Show {
        /// Task ID.
        task_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    match args.command {
        Command::Run { prompt, workdir } => {
            run_task(prompt, workdir).await?;
        }
        Command::List => {
            eprintln!("alps list: not yet implemented");
            std::process::exit(1);
        }
        Command::Show { task_id } => {
            eprintln!("alps show {}: not yet implemented", task_id);
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn run_task(prompt: String, workdir: String) -> Result<()> {
    let task_id = TaskId::new();
    let workdir = PathBuf::from(workdir);
    let workspace_root = workdir.join("tasks").join(task_id.as_str());
    let workspace = TaskWorkspace::new(&workspace_root);

    info!(target: "alps.cli", task_id = %task_id.as_str(), "starting task");

    let task = Task::<alps_core::task::Idle>::new(
        task_id.clone(),
        workspace.root.clone(),
        Prompt::new(prompt),
    );

    // Resolve the ALPS repo root (where scripts/ and the vendored ralph.sh live).
    // We use the binary's own path: target/{debug,release}/alps → ../../..
    let alps_root = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let ralph_path = alps_root.join("scripts/ralph.sh");
    let claude_prompt_path = alps_root.join("scripts/CLAUDE.md");

    let plan = PlanAgent::new("claude-sonnet-4");
    let implement = ImplementAgent::new(
        workspace.root.clone(),
        alps_core::implement::ImplementConfig {
            ralph_path,
            claude_prompt_path,
            ..Default::default()
        },
    );
    let review = ReviewAgent::default();
    let judge = JudgeAgent::new(
        Arc::new(DoDRunner::new()),
        Arc::new(HermesLlmJudge::default()),
    );

    match drive(task, &plan, &implement, &review, &judge, &workspace).await {
        Ok(done) => {
            // Write receipts.json (the durable artifact)
            alps_core::persistence::persist_task(&done, &workspace)
                .map_err(|e| anyhow::anyhow!("persistence failed: {}", e))?;

            // Auto-commit only if there are changes. Most ALPS runs produce
            // work in tasks/<id>/ which is gitignored, so this is a no-op
            // in practice. The smart check prevents a noisy warning.
            match commit_smart(&workdir, &format!("done: {}", task_id.as_str())) {
                Ok(CommitOutcome::NothingToCommit) => {} // silent — expected
                Ok(CommitOutcome::Committed) => {
                    eprintln!("[done] auto-committed final state");
                }
                Ok(CommitOutcome::CommitFailed(msg)) => {
                    eprintln!("warning: git commit failed: {}", msg);
                }
                Err(GitOpsError::Git { op, msg }) => {
                    eprintln!("warning: git {} error: {}", op, msg);
                }
                Err(e) => {
                    eprintln!("warning: auto-commit skipped: {}", e);
                }
            }

            // Print markdown summary to stdout
            print_markdown(&done);

            Ok(())
        }
        Err(e) => {
            error!(target: "alps.cli", error = %e, "task failed");
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_markdown(task: &Task<Done>) {
    let r: &Receipts = task.receipts();
    println!();
    println!("# ALPS — Done");
    println!();
    println!("**Task ID:** `{}`", task.id.as_str());
    println!("**Attempts:** {}", task.attempts());
    println!();
    println!("## Receipts");
    println!();
    println!("- **Plan summary:** {}", r.plan_summary);
    println!("- **Stories:** {}/{} passed", r.implement_metrics.stories_passed, r.implement_metrics.stories_total);
    println!("- **Implement iterations:** {}", r.implement_metrics.iterations);
    println!("- **Implement elapsed:** {}s", r.implement_metrics.elapsed_secs);
    println!("- **Review findings:** {} ({} critical)",
        r.review_summary.findings_count, r.review_summary.critical_findings);
    println!("- **Review assertions:** {}/{} passed",
        r.review_summary.assertions_passed, r.review_summary.assertions_total);
    println!("- **Judged at:** {}", r.judged_at.to_rfc3339());
    println!("- **Judge model:** {}", r.judge_model);
    println!();
    println!("Receipts written to `tasks/{}/receipts.json`", task.id.as_str());
}
