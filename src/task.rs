use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Identifier used for a Codex task.
pub type TaskId = String;

/// Possible lifecycle states for a Codex task.
#[derive(Clone, Debug, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskMetadata {
    pub id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: TaskState,
    #[serde(with = "serde_datetime")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "serde_datetime")]
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
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
            initial_prompt: None,
        }
    }
}

mod serde_datetime {
    use chrono::{DateTime, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_rfc3339())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&value)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}
