use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

use super::child::{PROMPT_ENV_VAR, TITLE_ENV_VAR};

/// Parameters required to spawn a detached worker process.
#[derive(Debug)]
pub struct WorkerLaunchRequest {
    pub store_root: PathBuf,
    pub title: Option<String>,
    pub prompt: String,
    pub executable: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub working_directory: Option<PathBuf>,
}

impl WorkerLaunchRequest {
    /// Creates a request for the provided store root and prompt with no optional metadata.
    pub fn new(store_root: PathBuf, prompt: String) -> Self {
        Self {
            store_root,
            title: None,
            prompt,
            executable: None,
            config_path: None,
            working_directory: None,
        }
    }
}

/// Spawns a detached worker process based on the provided request.
pub fn spawn_worker(request: WorkerLaunchRequest) -> Result<Child> {
    let WorkerLaunchRequest {
        store_root,
        title,
        prompt,
        executable,
        config_path,
        working_directory,
    } = request;

    let exe = match executable {
        Some(path) => path,
        None => std::env::current_exe().context("failed to locate current executable")?,
    };

    let mut command = Command::new(exe);
    command.arg("worker");
    command.arg("--store-root");
    command.arg(&store_root);

    if let Some(title) = title {
        command.env(TITLE_ENV_VAR, title);
    }

    command.env(PROMPT_ENV_VAR, &prompt);

    if let Some(config_path) = config_path {
        command.arg("--config-path");
        command.arg(config_path);
    }

    if let Some(working_directory) = working_directory {
        command.arg("--working-dir");
        command.arg(working_directory);
    }

    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::null());

    command.spawn().context("failed to spawn worker process")
}
