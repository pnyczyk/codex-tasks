use std::collections::VecDeque;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
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
    location: TaskLocation,
    pid: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TaskLocation {
    Active(PathBuf),
    Archived(PathBuf),
}

impl TaskLocation {
    fn kind(&self) -> &'static str {
        match self {
            TaskLocation::Active(_) => "active",
            TaskLocation::Archived(_) => "archived",
        }
    }

    fn directory(&self) -> &Path {
        match self {
            TaskLocation::Active(dir) | TaskLocation::Archived(dir) => dir,
        }
    }
}

fn render_human(record: &TaskStatusRecord) {
    println!("Task ID: {}", record.metadata.id);
    if let Some(title) = &record.metadata.title {
        println!("Title: {}", title);
    }
    println!("State: {}", record.metadata.state);
    println!("Created At: {}", record.metadata.created_at.to_rfc3339());
    println!("Updated At: {}", record.metadata.updated_at.to_rfc3339());
    println!(
        "Location: {} ({})",
        record.location.kind(),
        record.location.directory().display()
    );
    if let Some(pid) = record.pid {
        println!("PID: {}", pid);
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
        "last_result": record.metadata.last_result.clone(),
        "location": record.location.kind(),
        "directory": record.location.directory().display().to_string(),
        "pid": record.pid,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn load_status_record(store: &TaskStore, task_id: &str) -> Result<TaskStatusRecord> {
    let paths = store.task(task_id.to_string());
    match paths.read_metadata() {
        Ok(mut metadata) => {
            let directory = paths.directory().to_path_buf();
            let pid = paths.read_pid()?;
            let derived_state = derive_active_state(&metadata.state, pid);
            metadata.state = derived_state;
            if metadata.last_result.is_none() {
                metadata.last_result = read_result_file(&directory, &metadata.id)?;
            }
            Ok(TaskStatusRecord {
                metadata,
                location: TaskLocation::Active(directory),
                pid,
            })
        }
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if !not_found {
                return Err(err);
            }

            let Some((directory, mut metadata)) = find_archived_metadata(store, task_id)? else {
                bail!("task {task_id} was not found in the task store");
            };
            metadata.state = TaskState::Archived;
            if metadata.last_result.is_none() {
                metadata.last_result = read_result_file(&directory, &metadata.id)?;
            }
            Ok(TaskStatusRecord {
                metadata,
                location: TaskLocation::Archived(directory),
                pid: None,
            })
        }
    }
}

fn read_result_file(directory: &Path, task_id: &str) -> Result<Option<String>> {
    let path = directory.join(format!("{}.result", task_id));
    match fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read last result from {}", path.display()))
        }
    }
}

fn find_archived_metadata(
    store: &TaskStore,
    task_id: &str,
) -> Result<Option<(PathBuf, TaskMetadata)>> {
    let archive_root = store.archive_root();
    if !archive_root.exists() {
        return Ok(None);
    }

    let mut queue = VecDeque::from([archive_root]);
    let target_file = format!("{}.json", task_id);

    while let Some(dir) = queue.pop_front() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read archive directory {}", dir.display())
                });
            }
        };

        for entry in entries {
            let entry = entry.with_context(|| {
                format!("failed to iterate archive directory {}", dir.display())
            })?;
            let path = entry.path();
            if path.is_dir() {
                queue.push_back(path);
                continue;
            }

            if path
                .file_name()
                .is_some_and(|name| name == target_file.as_str())
            {
                let data = fs::read_to_string(&path).with_context(|| {
                    format!("failed to read archived metadata at {}", path.display())
                })?;
                let metadata: TaskMetadata = serde_json::from_str(&data).with_context(|| {
                    format!("failed to parse archived metadata at {}", path.display())
                })?;
                if metadata.id != task_id {
                    continue;
                }
                let directory = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| store.archive_root());
                return Ok(Some((directory, metadata)));
            }
        }
    }

    Ok(None)
}

fn derive_active_state(metadata_state: &TaskState, pid: Option<i32>) -> TaskState {
    match pid {
        Some(pid) => {
            if is_process_running(pid) {
                TaskState::Running
            } else {
                TaskState::Died
            }
        }
        None => match metadata_state {
            TaskState::Running => TaskState::Died,
            other => other.clone(),
        },
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
