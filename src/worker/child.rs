use std::convert::TryFrom;
use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::storage::{TaskPaths, TaskStore};
use crate::task::TaskId;

/// Environment variable that carries the optional title for the worker.
pub const TITLE_ENV_VAR: &str = "CODEX_TASK_TITLE";
/// Environment variable that carries the initial prompt for the worker.
pub const PROMPT_ENV_VAR: &str = "CODEX_TASK_PROMPT";
/// When set, the worker will exit immediately after it records its PID.
pub const EXIT_AFTER_START_ENV_VAR: &str = "CODEX_TASKS_EXIT_AFTER_START";

/// Configuration assembled from CLI arguments and environment variables for a worker.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub task_id: TaskId,
    pub store_root: PathBuf,
    pub title: Option<String>,
    pub initial_prompt: Option<String>,
}

impl WorkerConfig {
    /// Builds a configuration for the worker, preferring explicit CLI values and
    /// falling back to environment variables when they are absent.
    pub fn new(
        task_id: TaskId,
        store_root: PathBuf,
        title: Option<String>,
        initial_prompt: Option<String>,
    ) -> Result<Self> {
        let title = title.or_else(|| env::var(TITLE_ENV_VAR).ok());
        let initial_prompt = initial_prompt.or_else(|| env::var(PROMPT_ENV_VAR).ok());
        Ok(Self {
            task_id,
            store_root,
            title,
            initial_prompt,
        })
    }

    /// Returns a [`TaskStore`] rooted at the configured location.
    pub fn store(&self) -> TaskStore {
        TaskStore::new(self.store_root.clone())
    }

    /// Returns helpers that operate on files for this task.
    pub fn task_paths(&self) -> TaskPaths {
        self.store().task(self.task_id.clone())
    }
}

/// Runs the worker process until it is signalled to exit.
pub async fn run_worker(config: WorkerConfig) -> Result<()> {
    let store = config.store();
    store.ensure_layout()?;
    let paths = store.task(config.task_id.clone());
    paths.ensure_directory()?;

    let pid = std::process::id();
    let pid = i32::try_from(pid).context("worker process id exceeds i32 range")?;
    paths
        .write_pid(pid)
        .context("failed to persist worker pid file")?;

    if should_exit_after_start() {
        return Ok(());
    }

    tokio::signal::ctrl_c()
        .await
        .context("failed while waiting for shutdown signal")?;

    Ok(())
}

fn should_exit_after_start() -> bool {
    match env::var(EXIT_AFTER_START_ENV_VAR) {
        Ok(value) => {
            let trimmed = value.trim();
            trimmed.is_empty() || trimmed.eq_ignore_ascii_case("true") || trimmed == "1"
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    use crate::storage::TaskStore;
    use tempfile::tempdir;

    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: &str) {
        // Protected by ENV_GUARD to avoid concurrent mutations.
        unsafe { env::set_var(key, value) };
    }

    fn remove_env(key: &str) {
        unsafe { env::remove_var(key) };
    }

    #[test]
    fn cli_values_override_environment() {
        let _guard = ENV_GUARD.lock().expect("lock env");
        set_env(TITLE_ENV_VAR, "env title");
        set_env(PROMPT_ENV_VAR, "env prompt");
        let tmp = tempdir().expect("tempdir");
        let config = WorkerConfig::new(
            "task-1".to_string(),
            tmp.path().to_path_buf(),
            Some("cli title".to_string()),
            Some("cli prompt".to_string()),
        )
        .expect("config");
        assert_eq!(config.title.as_deref(), Some("cli title"));
        assert_eq!(config.initial_prompt.as_deref(), Some("cli prompt"));
        remove_env(TITLE_ENV_VAR);
        remove_env(PROMPT_ENV_VAR);
    }

    #[test]
    fn environment_values_fill_missing_fields() {
        let _guard = ENV_GUARD.lock().expect("lock env");
        set_env(TITLE_ENV_VAR, "env title");
        set_env(PROMPT_ENV_VAR, "env prompt");
        let tmp = tempdir().expect("tempdir");
        let config = WorkerConfig::new("task-2".to_string(), tmp.path().to_path_buf(), None, None)
            .expect("config");
        assert_eq!(config.title.as_deref(), Some("env title"));
        assert_eq!(config.initial_prompt.as_deref(), Some("env prompt"));
        remove_env(TITLE_ENV_VAR);
        remove_env(PROMPT_ENV_VAR);
    }

    #[test]
    fn run_worker_writes_pid_file() {
        let _guard = ENV_GUARD.lock().expect("lock env");
        set_env(EXIT_AFTER_START_ENV_VAR, "1");
        let tmp = tempdir().expect("tempdir");
        let task_id = "task-3".to_string();
        let store_root = tmp.path().to_path_buf();
        let config =
            WorkerConfig::new(task_id.clone(), store_root.clone(), None, None).expect("config");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(run_worker(config))
            .expect("worker should run");

        let store = TaskStore::new(store_root);
        let pid_path = store.task(task_id.clone()).pid_path();
        assert!(pid_path.exists());
        let contents = fs::read_to_string(pid_path).expect("read pid");
        let expected = i32::try_from(std::process::id()).expect("pid fits in i32");
        assert_eq!(contents.trim(), expected.to_string());
        remove_env(EXIT_AFTER_START_ENV_VAR);
    }
}
