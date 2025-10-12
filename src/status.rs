use std::io::ErrorKind;

use anyhow::{Result, bail};
use serde_json::json;

use crate::storage::TaskStore;
use crate::task::{TaskId, TaskMetadata, TaskState};

/// Output format supported by the status command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusFormat {
    Human,
    Json,
}

/// Options accepted by the status command handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusCommandOptions {
    pub task_id: TaskId,
    pub format: StatusFormat,
}

/// Executes the status command with the provided options.
pub fn run(options: StatusCommandOptions) -> Result<()> {
    let store = TaskStore::default()?;
    let status = load_status_record(&store, &options.task_id)?;

    match options.format {
        StatusFormat::Human => render_human(&status),
        StatusFormat::Json => render_json(&status)?,
    }

    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TaskStatusRecord {
    metadata: TaskMetadata,
    pid: Option<i32>,
}

fn render_human(record: &TaskStatusRecord) {
    println!("Task ID: {}", record.metadata.id);
    if let Some(title) = &record.metadata.title {
        println!("Title: {}", title);
    }
    println!("State: {}", record.metadata.state);
    println!("Created At: {}", record.metadata.created_at.to_rfc3339());
    println!("Updated At: {}", record.metadata.updated_at.to_rfc3339());
    match record.metadata.working_dir.as_deref() {
        Some(dir) => println!("Working Dir: {}", dir),
        None => println!("Working Dir: <none>"),
    }
    if let Some(pid) = record.pid {
        println!("PID: {}", pid);
    }
    match &record.metadata.last_prompt {
        Some(prompt) => {
            println!("Last Prompt:");
            if prompt.trim().is_empty() {
                println!("<empty>");
            } else {
                println!("{}", prompt);
            }
        }
        None => println!("Last Prompt: <none>"),
    }
    println!("Last Result:");
    match &record.metadata.last_result {
        Some(result) if !result.trim().is_empty() => println!("{}", result),
        _ => println!("<none>"),
    }
}

fn render_json(record: &TaskStatusRecord) -> Result<()> {
    let payload = json!({
        "id": record.metadata.id.clone(),
        "title": record.metadata.title.clone(),
        "state": record.metadata.state.clone(),
        "created_at": record.metadata.created_at.clone(),
        "updated_at": record.metadata.updated_at.clone(),
        "last_prompt": record.metadata.last_prompt.clone(),
        "last_result": record.metadata.last_result.clone(),
        "working_dir": record.metadata.working_dir.clone(),
        "pid": record.pid,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn load_status_record(store: &TaskStore, task_id: &str) -> Result<TaskStatusRecord> {
    let paths = store.task(task_id.to_string());
    match paths.read_metadata() {
        Ok(mut metadata) => {
            let pid = paths.read_pid()?;
            let derived_state = derive_active_state(&metadata.state, pid);
            metadata.state = derived_state;
            if metadata.last_result.is_none() {
                metadata.last_result = paths.read_last_result()?;
            }
            Ok(TaskStatusRecord { metadata, pid })
        }
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if !not_found {
                return Err(err);
            }

            let Some((paths, mut metadata)) = store.find_archived_task(task_id)? else {
                bail!("task {task_id} was not found in the task store");
            };
            metadata.state = TaskState::Archived;
            if metadata.last_result.is_none() {
                metadata.last_result = paths.read_last_result()?;
            }
            Ok(TaskStatusRecord {
                metadata,
                pid: None,
            })
        }
    }
}

pub(crate) fn derive_active_state(metadata_state: &TaskState, pid: Option<i32>) -> TaskState {
    if let Some(pid) = pid {
        if is_process_running(pid) {
            return match metadata_state {
                TaskState::Running => TaskState::Running,
                TaskState::Stopped => TaskState::Stopped,
                TaskState::Archived => TaskState::Archived,
                TaskState::Died => TaskState::Running,
            };
        }
    }
    derive_state_without_pid(metadata_state.clone())
}

fn derive_state_without_pid(metadata_state: TaskState) -> TaskState {
    match metadata_state {
        TaskState::Running => TaskState::Died,
        other => other,
    }
}

fn is_process_running(pid: i32) -> bool {
    // SAFETY: libc::kill is called with signal 0 which performs error checking without
    // delivering a signal to the target process.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }

    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => true,
        Some(libc::ESRCH) | None => false,
        _ => false,
    }
}
