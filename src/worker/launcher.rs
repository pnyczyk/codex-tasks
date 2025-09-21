use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use crate::storage::TaskPaths;

use super::child::{PROMPT_ENV_VAR, TITLE_ENV_VAR};

/// Parameters required to spawn a detached worker process.
#[derive(Debug)]
pub struct WorkerLaunchRequest {
    pub task_paths: TaskPaths,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub executable: Option<PathBuf>,
}

impl WorkerLaunchRequest {
    /// Creates a request for the provided task paths with no optional metadata.
    pub fn new(task_paths: TaskPaths) -> Self {
        Self {
            task_paths,
            title: None,
            prompt: None,
            executable: None,
        }
    }
}

/// Spawns a detached worker process based on the provided request.
pub fn spawn_worker(request: WorkerLaunchRequest) -> Result<Child> {
    let WorkerLaunchRequest {
        task_paths,
        title,
        prompt,
        executable,
    } = request;

    task_paths.ensure_directory()?;

    let exe = match executable {
        Some(path) => path,
        None => std::env::current_exe().context("failed to locate current executable")?,
    };

    let mut command = Command::new(exe);
    command.arg("worker");
    command.arg("--task-id");
    command.arg(task_paths.id());
    command.arg("--store-root");
    command.arg(task_paths.directory());

    if let Some(title) = title {
        command.env(TITLE_ENV_VAR, title);
    }

    if let Some(prompt) = prompt {
        command.env(PROMPT_ENV_VAR, prompt);
    }

    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());

    command.spawn().context("failed to spawn worker process")
}
