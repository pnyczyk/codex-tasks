use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::fs::OpenOptions as TokioOpenOptions;
use tokio::io::{self as tokio_io, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, Lines};
use tokio::process::Command;

use crate::storage::{TaskPaths, TaskStore};
use crate::task::{TaskId, TaskMetadata, TaskState};

/// Environment variable that carries the optional title for the worker.
pub const TITLE_ENV_VAR: &str = "CODEX_TASK_TITLE";
/// Environment variable that carries the initial prompt for the worker.
pub const PROMPT_ENV_VAR: &str = "CODEX_TASK_PROMPT";
/// When set, the worker will exit immediately after it records its PID.
pub const EXIT_AFTER_START_ENV_VAR: &str = "CODEX_TASKS_EXIT_AFTER_START";

const THREAD_STARTED_EVENT: &str = "thread.started";
const STDERR_PREFIX: &[u8] = b"[stderr] ";

/// Configuration assembled from CLI arguments and environment variables for a worker.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub store_root: PathBuf,
    pub title: Option<String>,
    pub initial_prompt: String,
    pub config_path: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
}

impl WorkerConfig {
    /// Builds a configuration for the worker, preferring explicit CLI values and
    /// falling back to environment variables when they are absent.
    pub fn new(
        store_root: PathBuf,
        title: Option<String>,
        initial_prompt: Option<String>,
        config_path: Option<PathBuf>,
        working_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let title = title.or_else(|| env::var(TITLE_ENV_VAR).ok());
        let initial_prompt = initial_prompt
            .or_else(|| env::var(PROMPT_ENV_VAR).ok())
            .ok_or_else(|| anyhow!("initial prompt is required when launching a worker"))?;

        if initial_prompt.trim().is_empty() {
            bail!("initial prompt must not be empty");
        }

        let config_path = canonicalize_optional_path(config_path)
            .context("failed to prepare worker config path")?;
        let working_dir = canonicalize_optional_path(working_dir)
            .context("failed to prepare worker working directory")?;

        Ok(Self {
            store_root,
            title,
            initial_prompt,
            config_path,
            working_dir,
        })
    }

    /// Returns a [`TaskStore`] rooted at the configured location.
    pub fn store(&self) -> TaskStore {
        TaskStore::new(self.store_root.clone())
    }

    /// Directory that acts as `CODEX_HOME` override when a custom config file is provided.
    pub fn codex_home_override(&self) -> Result<Option<PathBuf>> {
        match &self.config_path {
            Some(path) => {
                let parent = path.parent().ok_or_else(|| {
                    anyhow!(
                        "config file {} does not have a parent directory",
                        path.display()
                    )
                })?;
                Ok(Some(parent.to_path_buf()))
            }
            None => Ok(None),
        }
    }
}

fn canonicalize_optional_path(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    match path {
        Some(p) => {
            Ok(Some(p.canonicalize().with_context(|| {
                format!("failed to resolve path {}", p.display())
            })?))
        }
        None => Ok(None),
    }
}

/// Runs the worker process until it is signalled to exit.
pub async fn run_worker(config: WorkerConfig) -> Result<()> {
    if should_exit_after_start() {
        return Ok(());
    }

    let worker = Worker::new(config).context("failed to initialize worker")?;
    worker.run().await
}

struct Worker {
    config: WorkerConfig,
    store: TaskStore,
    session: Option<ActiveSession>,
}

impl Worker {
    fn new(config: WorkerConfig) -> Result<Self> {
        let store = config.store();
        store.ensure_layout()?;
        Ok(Self {
            config,
            store,
            session: None,
        })
    }

    async fn run(mut self) -> Result<()> {
        let initial_prompt = self.config.initial_prompt.clone();
        self.run_invocation(initial_prompt, InvocationKind::Initial)
            .await?;

        {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| anyhow!("session not initialized after initial invocation"))?;
            session.prepare_prompt_reader().await?;
        }

        loop {
            let prompt_opt = {
                let session = self
                    .session
                    .as_mut()
                    .ok_or_else(|| anyhow!("session not initialized after initial invocation"))?;
                session.next_prompt().await?
            };

            let prompt = match prompt_opt {
                Some(prompt) => prompt,
                None => break,
            };

            let trimmed = prompt.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "/quit" {
                break;
            }

            self.run_invocation(prompt, InvocationKind::Resume).await?;
        }

        self.shutdown().await
    }

    async fn run_invocation(&mut self, prompt: String, kind: InvocationKind) -> Result<()> {
        if prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }

        if let Some(session) = self.session.as_mut() {
            session
                .paths
                .update_metadata(|metadata| {
                    metadata.set_state(TaskState::Running);
                    metadata.last_prompt = Some(prompt.clone());
                })
                .context("failed to update metadata before invocation")?;
        }

        let result_file = NamedTempFile::new_in(&self.config.store_root)
            .context("failed to allocate temp file for last message")?;
        let result_path = result_file.into_temp_path();

        let codex_home = self.config.codex_home_override()?;
        let mut command = Command::new("codex");
        command.arg("exec");
        command.arg("--json");
        command.arg("--output-last-message");
        command.arg(&result_path);

        if let Some(dir) = &self.config.working_dir {
            command.arg("--cd");
            command.arg(dir);
        }

        if let Some(home) = &codex_home {
            command.env("CODEX_HOME", home);
        }

        match (&self.session, kind) {
            (None, InvocationKind::Initial) => {
                command.arg(&prompt);
            }
            (Some(session), InvocationKind::Resume) => {
                command.arg("resume");
                command.arg(&session.thread_id);
                command.arg(&prompt);
            }
            (None, InvocationKind::Resume) => {
                bail!("cannot resume before establishing a Codex thread");
            }
            (Some(_), InvocationKind::Initial) => {
                bail!("initial invocation already performed for this worker");
            }
        }

        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        let mut child = command.spawn().context("failed to spawn `codex exec`")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture stdout of `codex exec`")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture stderr of `codex exec`")?;

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut buffered_stdout = Vec::new();
        let mut buffered_stderr = Vec::new();

        let wait_handle = tokio::spawn(async move { child.wait().await });

        let mut stdout_done = false;
        let mut stderr_done = false;

        loop {
            tokio::select! {
                line = stdout_reader.next_line(), if !stdout_done => {
                    match line {
                        Ok(Some(content)) => {
                            self.handle_stdout_line(&prompt, &mut buffered_stdout, &mut buffered_stderr, &content).await?;
                        }
                        Ok(None) => stdout_done = true,
                        Err(err) => return Err(err).context("failed to read stdout from `codex exec`"),
                    }
                }
                line = stderr_reader.next_line(), if !stderr_done => {
                    match line {
                        Ok(Some(content)) => {
                            self.handle_stderr_line(&mut buffered_stderr, &content).await?;
                        }
                        Ok(None) => stderr_done = true,
                        Err(err) => return Err(err).context("failed to read stderr from `codex exec`"),
                    }
                }
                else => {
                    if stdout_done && stderr_done {
                        break;
                    }
                }
            }
        }

        let status = wait_handle
            .await
            .context("failed to wait for `codex exec` child task")?
            .context("`codex exec` terminated unexpectedly")?;

        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("Codex thread was not established before process exit"))?;

        session.log.flush().await?;

        let exit_state = if status.success() {
            TaskState::Stopped
        } else {
            TaskState::Died
        };

        session
            .paths
            .update_metadata(|metadata| {
                metadata.set_state(exit_state.clone());
                metadata.last_prompt = Some(prompt.clone());
            })
            .context("failed to update task metadata after invocation")?;

        if result_path.exists() {
            let message =
                fs::read_to_string(&result_path).context("failed to read last message output")?;
            let final_path = session.paths.result_path();
            fs::write(&final_path, &message)
                .with_context(|| format!("failed to persist result at {}", final_path.display()))?;
            session
                .paths
                .update_metadata(|metadata| {
                    metadata.last_result = Some(message.clone());
                })
                .context("failed to update metadata with last result")?;
        }

        result_path
            .close()
            .context("failed to remove temporary result file")?;

        if exit_state == TaskState::Died {
            bail!("codex exec failed with status {}", status);
        }

        Ok(())
    }

    async fn handle_stdout_line(
        &mut self,
        prompt: &str,
        buffered_stdout: &mut Vec<String>,
        buffered_stderr: &mut Vec<String>,
        line: &str,
    ) -> Result<()> {
        if let Some(thread_id) = try_extract_thread_id(line) {
            if self.session.is_none() {
                self.initialize_session(thread_id, prompt, buffered_stdout, buffered_stderr)
                    .await?;
            }
        }

        if let Some(session) = self.session.as_mut() {
            session.write_stdout(line).await?;
        } else {
            buffered_stdout.push(line.to_string());
        }
        Ok(())
    }

    async fn handle_stderr_line(
        &mut self,
        buffered_stderr: &mut Vec<String>,
        line: &str,
    ) -> Result<()> {
        if let Some(session) = self.session.as_mut() {
            session.write_stderr(line).await?;
        } else {
            buffered_stderr.push(line.to_string());
        }
        Ok(())
    }

    async fn initialize_session(
        &mut self,
        thread_id: TaskId,
        prompt: &str,
        buffered_stdout: &mut Vec<String>,
        buffered_stderr: &mut Vec<String>,
    ) -> Result<()> {
        let paths = self.store.task(thread_id.clone());
        paths.ensure_directory()?;

        let pid =
            i32::try_from(std::process::id()).context("worker process id exceeds i32 range")?;
        paths.write_pid(pid)?;

        let mut metadata = TaskMetadata::new(
            thread_id.clone(),
            self.config.title.clone(),
            TaskState::Running,
        );
        metadata.initial_prompt = Some(prompt.to_string());
        metadata.last_prompt = Some(prompt.to_string());
        self.store.save_metadata(&metadata)?;

        let log_file = TokioOpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.log_path())
            .await
            .with_context(|| format!("failed to open log file for task {}", thread_id))?;
        let mut log = BufWriter::new(log_file);

        for line in buffered_stdout.drain(..) {
            log.write_all(line.as_bytes()).await?;
            log.write_all(b"\n").await?;
        }
        for line in buffered_stderr.drain(..) {
            log.write_all(STDERR_PREFIX).await?;
            log.write_all(line.as_bytes()).await?;
            log.write_all(b"\n").await?;
        }
        log.flush().await?;

        let pipe_path = paths.pipe_path();
        create_pipe(&pipe_path)
            .with_context(|| format!("failed to create prompt pipe for {}", thread_id))?;
        let prompt_reader = PromptReader::new(pipe_path)
            .await
            .with_context(|| format!("failed to initialize prompt reader for {}", thread_id))?;

        println!("{thread_id}");
        if let Err(err) = tokio_io::stdout().flush().await {
            eprintln!("failed to flush handshake stdout: {err:#}");
        }

        self.session = Some(ActiveSession {
            thread_id,
            paths,
            log,
            prompt_reader: Some(prompt_reader),
        });

        Ok(())
    }

    async fn shutdown(mut self) -> Result<()> {
        if let Some(mut session) = self.session.take() {
            if let Err(err) = session.paths.update_metadata(|metadata| {
                metadata.set_state(TaskState::Stopped);
            }) {
                eprintln!(
                    "failed to mark task {} as stopped: {err:#}",
                    session.paths.id()
                );
            }
            if let Err(err) = session.flush().await {
                eprintln!(
                    "failed to flush log for task {}: {err:#}",
                    session.paths.id()
                );
            }
            if let Err(err) = session.paths.remove_pipe() {
                eprintln!(
                    "failed to remove pipe for task {}: {err:#}",
                    session.paths.id()
                );
            }
            if let Err(err) = session.paths.remove_pid() {
                eprintln!(
                    "failed to remove pid for task {}: {err:#}",
                    session.paths.id()
                );
            }
        }
        Ok(())
    }
}

struct ActiveSession {
    thread_id: TaskId,
    paths: TaskPaths,
    log: BufWriter<tokio::fs::File>,
    prompt_reader: Option<PromptReader>,
}

impl ActiveSession {
    async fn write_stdout(&mut self, line: &str) -> io::Result<()> {
        self.log.write_all(line.as_bytes()).await?;
        self.log.write_all(b"\n").await
    }

    async fn write_stderr(&mut self, line: &str) -> io::Result<()> {
        self.log.write_all(STDERR_PREFIX).await?;
        self.log.write_all(line.as_bytes()).await?;
        self.log.write_all(b"\n").await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.log.flush().await
    }

    async fn prepare_prompt_reader(&mut self) -> Result<()> {
        if self.prompt_reader.is_none() {
            create_pipe(&self.paths.pipe_path())
                .with_context(|| format!("failed to create prompt pipe for {}", self.thread_id))?;
            self.prompt_reader = Some(
                PromptReader::new(self.paths.pipe_path())
                    .await
                    .with_context(|| {
                        format!("failed to initialize prompt reader for {}", self.thread_id)
                    })?,
            );
        }
        Ok(())
    }

    async fn next_prompt(&mut self) -> Result<Option<String>> {
        let reader = match self.prompt_reader.as_mut() {
            Some(reader) => reader,
            None => return Ok(None),
        };
        reader.next_prompt().await
    }
}

struct PromptReader {
    path: PathBuf,
    lines: Lines<BufReader<tokio::fs::File>>,
}

impl PromptReader {
    async fn new(path: PathBuf) -> Result<Self> {
        let file = TokioOpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .with_context(|| format!("failed to open prompt pipe at {}", path.display()))?;
        let reader = BufReader::with_capacity(4096, file).lines();
        Ok(Self {
            path,
            lines: reader,
        })
    }

    async fn reopen(&mut self) -> Result<()> {
        let file = TokioOpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .await
            .with_context(|| format!("failed to reopen prompt pipe at {}", self.path.display()))?;
        self.lines = BufReader::with_capacity(4096, file).lines();
        Ok(())
    }

    async fn next_prompt(&mut self) -> Result<Option<String>> {
        loop {
            match self.lines.next_line().await {
                Ok(Some(line)) => return Ok(Some(line)),
                Ok(None) => {
                    self.reopen().await?;
                    continue;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
                    self.reopen().await?;
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("failed to read prompt from {}", self.path.display())
                    });
                }
            }
        }
    }
}

fn should_exit_after_start() -> bool {
    env::var(EXIT_AFTER_START_ENV_VAR).is_ok()
}

fn create_pipe(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("failed to convert pipe path to CString"))?;
    let mode = libc::S_IRUSR | libc::S_IWUSR;
    let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
    if result == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::AlreadyExists {
        Ok(())
    } else {
        Err(err).with_context(|| format!("failed to create fifo at {}", path.display()))
    }
}

fn try_extract_thread_id(line: &str) -> Option<TaskId> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type = value.get("type")?.as_str()?;
    if event_type == THREAD_STARTED_EVENT {
        value
            .get("thread_id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
    } else {
        None
    }
}

enum InvocationKind {
    Initial,
    Resume,
}
