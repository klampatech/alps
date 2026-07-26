//! Task type-state pattern.
//!
//! Each state is a distinct struct. Transitions consume `self` and return
//! the next state. The Rust compiler enforces the order — invalid transitions
//! are compile errors.
//!
//! The `prompt` lives on `Task<S>` itself, not in each state, because it
//! doesn't change between states (except when `Rejected::reset()` appends
//! feedback to it).
//!
//! See `SPEC.md` §5.4 for the full design.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::path::PathBuf;

use crate::domain::{Feedback, Implementation, Plan, Prompt, Review};
use crate::receipt::Receipts;

/// Generic task parameterized by its current state.
/// Each `State` is a distinct struct (e.g. `Idle`, `Planned`, etc.).
pub struct Task<State> {
    pub id: super::TaskId,
    pub prompt: Prompt,
    pub workdir: PathBuf,
    pub state: State,
    _phantom: PhantomData<State>,
}

/// Initial state. Only the prompt exists.
pub struct Idle;

/// Plan has been emitted.
pub struct Planned {
    pub plan: Plan,
    pub attempt: u32,
}

/// Implementation has been emitted by Ralph.
pub struct Implemented {
    pub plan: Plan,
    pub implementation: Implementation,
    pub attempt: u32,
}

/// Review has been emitted.
pub struct Reviewed {
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    pub attempt: u32,
}

/// Judge has accepted. Terminal.
pub struct Done {
    pub receipts: Receipts,
    pub attempts: u32,
}

/// Judge has rejected. Must reset to Idle.
pub struct Rejected {
    pub feedback: Feedback,
    pub attempts: u32,
    pub history: Vec<Attempt>,
}

/// Catastrophic failure. Terminal.
pub struct Failed {
    pub reason: FailureReason,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub plan: Plan,
    pub implementation: Implementation,
    pub review: Review,
    pub feedback: Option<Feedback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FailureReason {
    PlanAgentError(String),
    ImplementError(String),
    ReviewAgentError(String),
    JudgeError(String),
    PersistenceError(String),
    MaxAttemptsExceeded { max: u32 },
}

// =================== Transitions ===================

impl Task<Idle> {
    pub fn new(id: super::TaskId, workdir: PathBuf, prompt: Prompt) -> Self {
        Task {
            id,
            prompt,
            workdir,
            state: Idle,
            _phantom: PhantomData,
        }
    }

    pub fn plan(self, plan: Plan) -> Task<Planned> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Planned { plan, attempt: 1 },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed { reason, attempts: 0 },
            _phantom: PhantomData,
        }
    }
}

impl Task<Planned> {
    pub fn implement(self, implementation: Implementation) -> Task<Implemented> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Implemented {
                plan: self.state.plan,
                implementation,
                attempt: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Implemented> {
    pub fn review(self, review: Review) -> Task<Reviewed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Reviewed {
                plan: self.state.plan,
                implementation: self.state.implementation,
                review,
                attempt: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Reviewed> {
    /// The judge. Returns `Ok` on pass, `Err` on reject.
    pub fn judge(self, judgment: super::Judgment) -> Result<Task<Done>, Task<Rejected>> {
        use super::Judgment::*;
        match judgment {
            Pass(receipts) => Ok(Task {
                id: self.id,
                prompt: self.prompt,
                workdir: self.workdir,
                state: Done {
                    receipts,
                    attempts: self.state.attempt,
                },
                _phantom: PhantomData,
            }),
            Reject(feedback) => Err(Task {
                id: self.id,
                prompt: self.prompt,
                workdir: self.workdir,
                state: Rejected {
                    feedback,
                    attempts: self.state.attempt,
                    history: Vec::new(),
                },
                _phantom: PhantomData,
            }),
        }
    }

    pub fn fail(self, reason: FailureReason) -> Task<Failed> {
        Task {
            id: self.id,
            prompt: self.prompt,
            workdir: self.workdir,
            state: Failed {
                reason,
                attempts: self.state.attempt,
            },
            _phantom: PhantomData,
        }
    }
}

impl Task<Rejected> {
    /// The only way out of Rejected. Appends feedback to the prompt
    /// and returns to Idle. The next iteration starts a fresh attempt.
    pub fn reset(self, history: Vec<Attempt>) -> Task<Idle> {
        let _ = history; // history is recorded by persistence; not used in reset
        let new_prompt = Self::append_feedback(&self.prompt, &self.state.feedback, self.state.attempts);
        Task {
            id: self.id,
            prompt: new_prompt,
            workdir: self.workdir,
            state: Idle,
            _phantom: PhantomData,
        }
    }

    fn append_feedback(prompt: &Prompt, feedback: &Feedback, attempts: u32) -> Prompt {
        let mut s = prompt.0.clone();
        s.push_str(&format!(
            "\n\n## Previous attempt rejected (#{})\n\n\
             **Reason:** {}\n\n\
             **Failed assertions:**\n{}\n\n\
             **Retry hints:**\n{}\n",
            attempts,
            feedback.reason,
            feedback
                .failed_assertions
                .iter()
                .map(|a| format!("- {} {}", if a.passed { "[x]" } else { "[ ]" }, a.criterion))
                .collect::<Vec<_>>()
                .join("\n"),
            feedback
                .retry_hints
                .iter()
                .map(|h| format!("- {}", h))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        Prompt(s)
    }
}

impl Task<Failed> {
    pub fn reason(&self) -> &FailureReason {
        &self.state.reason
    }

    pub fn attempts(&self) -> u32 {
        self.state.attempts
    }
}

impl Task<Done> {
    pub fn receipts(&self) -> &Receipts {
        &self.state.receipts
    }

    pub fn attempts(&self) -> u32 {
        self.state.attempts
    }
}

// =================== Tests ===================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Plan, PlanId, Prompt, StoryId, UserStory, DefinitionOfDone};
    use uuid::Uuid;

    fn dummy_plan() -> Plan {
        Plan {
            id: PlanId(Uuid::new_v4()),
            goal: "test".to_string(),
            architecture: "test".to_string(),
            stories: vec![UserStory {
                id: StoryId("US-001".to_string()),
                title: "test".to_string(),
                description: "test".to_string(),
                acceptance_criteria: vec!["test".to_string()],
                priority: 1,
            }],
            dod: vec![DefinitionOfDone {
                criterion: "test".to_string(),
                verifiable: true,
            }],
        }
    }

    #[test]
    fn idle_to_planned() {
        let task = Task::<Idle>::new(
            super::super::TaskId::new(),
            PathBuf::from("/tmp"),
            Prompt::new("test"),
        );
        let planned = task.plan(dummy_plan());
        assert_eq!(planned.state.attempt, 1);
    }

    #[test]
    fn rejected_resets_to_idle_with_feedback() {
        // Build a Task<Rejected> directly (for testing)
        let task = Task {
            id: super::super::TaskId::new(),
            prompt: Prompt::new("original"),
            workdir: PathBuf::from("/tmp"),
            state: Rejected {
                feedback: Feedback {
                    reason: "test failed".to_string(),
                    failed_assertions: vec![],
                    retry_hints: vec!["hint 1".to_string()],
                },
                attempts: 1,
                history: vec![],
            },
            _phantom: PhantomData,
        };
        let reset = task.reset(vec![]);
        assert!(reset.prompt.as_str().contains("original"));
        assert!(reset.prompt.as_str().contains("Previous attempt rejected"));
        assert!(reset.prompt.as_str().contains("test failed"));
    }
}
