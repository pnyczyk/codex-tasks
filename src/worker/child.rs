use std::convert::TryFrom;
use std::env;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codex_core::config::{Config, ConfigOverrides};
use codex_core::protocol::Event;
use tokio::fs::OpenOptions as TokioOpenOptions;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::storage::{TaskPaths, TaskStore};
use crate::task::TaskId;
use crate::worker::event_processor_with_human_output::EventProcessorWithHumanOutput;
use crate::worker::runner;

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
    let worker_config = config;
    let store = worker_config.store();
    store.ensure_layout()?;
    let paths = store.task(worker_config.task_id.clone());
    paths.ensure_directory()?;

    let pid = std::process::id();
    let pid = i32::try_from(pid).context("worker process id exceeds i32 range")?;
    paths
        .write_pid(pid)
        .context("failed to persist worker pid file")?;

    if should_exit_after_start() {
        return Ok(());
    }

    let pipe_path = paths.pipe_path();
    create_pipe(&pipe_path)
        .with_context(|| format!("failed to create prompt pipe at {}", pipe_path.display()))?;

    let log_path = paths.log_path();
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file at {}", log_path.display()))?;

    redirect_stdio_to(&log_file).context("failed to redirect worker output to log file")?;

    let codex_config = Config::load_with_cli_overrides(Vec::new(), ConfigOverrides::default())
        .context("failed to load Codex configuration")?;

    let mut event_processor = EventProcessorWithHumanOutput::create_with_ansi(
        false,
        &codex_config,
        Some(paths.result_path()),
    );

    let runner::ChildHandles {
        child,
        stdout,
        stdin,
    } = runner::spawn_codex_proto().await?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(trimmed) {
                Ok(event) => {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("Failed to parse event from codex proto: {err}");
                }
            }
        }
    });

    let pipe_file = TokioOpenOptions::new()
        .read(true)
        .write(true)
        .open(&pipe_path)
        .await
        .with_context(|| format!("failed to open prompt pipe at {}", pipe_path.display()))?;

    let result = runner::run_event_loop(
        child,
        stdin,
        &mut event_processor,
        &codex_config,
        &mut event_rx,
        pipe_file,
        worker_config.initial_prompt.clone(),
    )
    .await;

    if let Err(err) = std::io::stdout().flush() {
        eprintln!("failed to flush worker log: {err:#}");
    }

    if let Err(err) = paths.remove_pipe() {
        eprintln!("failed to remove pipe for task {}: {err:#}", paths.id());
    }
    if let Err(err) = paths.remove_pid() {
        eprintln!("failed to remove pid file for task {}: {err:#}", paths.id());
    }

    drop(log_file);

    result
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

fn create_pipe(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove existing pipe at {}", path.display()))?;
    }

    let c_path = CString::new(path.as_os_str().as_bytes())
        .context("pipe path contained interior null bytes")?;
    let mode = 0o600;
    let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to create pipe at {}", path.display()));
    }

    Ok(())
}

fn redirect_stdio_to(log: &std::fs::File) -> Result<()> {
    let fd = log.as_raw_fd();
    unsafe {
        if libc::dup2(fd, libc::STDOUT_FILENO) == -1 {
            return Err(std::io::Error::last_os_error())
                .context("failed to redirect stdout to log file");
        }
        if libc::dup2(fd, libc::STDERR_FILENO) == -1 {
            return Err(std::io::Error::last_os_error())
                .context("failed to redirect stderr to log file");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::FileTypeExt;
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

    #[test]
    fn create_pipe_creates_fifo() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("pipe");
        create_pipe(&path).expect("create pipe");
        let metadata = fs::metadata(&path).expect("metadata");
        assert!(metadata.file_type().is_fifo(), "expected FIFO at {path:?}");
    }
}
