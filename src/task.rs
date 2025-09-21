use chrono::{DateTime, Utc};
use clap::ValueEnum;

/// Identifier used for a Codex task.
pub type TaskId = String;

/// Possible lifecycle states for a Codex task.
#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum TaskState {
    #[value(name = "IDLE")]
    Idle,
    #[value(name = "RUNNING")]
    Running,
    #[value(name = "STOPPED")]
    Stopped,
    #[value(name = "ARCHIVED")]
    Archived,
    #[value(name = "DIED")]
    Died,
}

/// Core metadata tracked for each task on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskMetadata {
    pub id: TaskId,
    pub title: Option<String>,
    pub state: TaskState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_result: Option<String>,
}

impl TaskMetadata {
    /// Convenience constructor for building a new metadata record.
    pub fn new(id: TaskId, title: Option<String>, state: TaskState) -> Self {
        let now = Utc::now();
        Self {
            id,
            title,
            state,
            created_at: now,
            updated_at: now,
            last_result: None,
        }
    }
}
