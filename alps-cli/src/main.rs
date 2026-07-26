//! ALPS CLI — `alps run "prompt"` to start a task.
//!
//! MVP: stub CLI. The real impl will:
//!   1. Create `tasks/<task-id>/` workspace
//!   2. Initialize `Task<Idle>` with the prompt
//!   3. Drive the outer loop (Plan → Implement → Review → Judge)
//!   4. Persist + commit at each state
//!   5. On Done, print receipts and exit

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

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
            info!(target: "alps.cli", "run: prompt={}, workdir={}", prompt, workdir);
            // MVP: stub. Real impl creates Task<Idle> and drives the loop.
            eprintln!("alps: not yet implemented");
            eprintln!("spec: see /home/kyle/Development/alps/SPEC.md");
            std::process::exit(1);
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
}
