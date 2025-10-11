use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tempfile::NamedTempFile;
use tokio::fs::OpenOptions as TokioOpenOptions;
use tokio::io::{self as tokio_io, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;

use crate::storage::{TaskPaths, TaskStore};
use crate::task::{TaskId, TaskMetadata, TaskState};

pub const TITLE_ENV_VAR: &str = "CODEX_TASK_TITLE";
pub const PROMPT_ENV_VAR: &str = "CODEX_TASK_PROMPT";
pub const EXIT_AFTER_START_ENV_VAR: &str = "CODEX_TASKS_EXIT_AFTER_START";
const STDERR_PREFIX: &str = "[stderr]";

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub store_root: PathBuf,
    pub task_id: Option<TaskId>,
    pub title: Option<String>,
    pub prompt: String,
    pub config_path: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
}

impl WorkerConfig {
    pub fn new(
        store_root: PathBuf,
        task_id: Option<String>,
        title: Option<String>,
        prompt: Option<String>,
        config_path: Option<PathBuf>,
        working_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let title = title.or_else(|| env::var(TITLE_ENV_VAR).ok());
        let prompt = prompt
            .or_else(|| env::var(PROMPT_ENV_VAR).ok())
            .ok_or_else(|| anyhow!("prompt is required when launching a worker"))?;
        if prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }

        let config_path = canonicalize_optional(config_path)
            .context("failed to resolve config path for worker")?;
        let working_dir = canonicalize_optional(working_dir)
            .context("failed to resolve working directory for worker")?;

        Ok(Self {
            store_root,
            task_id,
            title,
            prompt,
            config_path,
            working_dir,
        })
    }

    pub fn store(&self) -> TaskStore {
        TaskStore::new(self.store_root.clone())
    }

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

fn canonicalize_optional(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    match path {
        Some(p) => {
            Ok(Some(p.canonicalize().with_context(|| {
                format!("failed to canonicalize {}", p.display())
            })?))
        }
        None => Ok(None),
    }
}

pub async fn run_worker(config: WorkerConfig) -> Result<()> {
    if env::var(EXIT_AFTER_START_ENV_VAR).is_ok() {
        return Ok(());
    }

    let worker = Worker::initialize(config).await?;
    worker.run().await
}

struct Worker {
    config: WorkerConfig,
    store: TaskStore,
    session: Option<ActiveSession>,
}

impl Worker {
    async fn initialize(mut config: WorkerConfig) -> Result<Self> {
        let store = config.store();
        store.ensure_layout()?;

        let session = if let Some(task_id) = &config.task_id {
            let paths = store.task(task_id.clone());
            if !paths.metadata_path().exists() {
                bail!("task {task_id} was not found");
            }
            let metadata = paths.read_metadata()?;
            if config.config_path.is_none() {
                config.config_path = metadata.config_path.as_ref().map(PathBuf::from);
            }
            if config.working_dir.is_none() {
                config.working_dir = metadata.working_dir.as_ref().map(PathBuf::from);
            }

            let log_file = TokioOpenOptions::new()
                .create(true)
                .append(true)
                .open(paths.log_path())
                .await
                .with_context(|| format!("failed to open log file for task {task_id}"))?;
            Some(ActiveSession::from_existing(
                task_id.clone(),
                paths,
                log_file,
            ))
        } else {
            None
        };

        Ok(Self {
            config,
            store,
            session,
        })
    }

    async fn run(mut self) -> Result<()> {
        let initial = self.session.is_none();
        let prompt = self.config.prompt.clone();
        let request = if initial {
            InvocationKind::Initial
        } else {
            InvocationKind::Resume
        };

        self.run_invocation(prompt, request).await?;
        self.finalize().await
    }

    async fn run_invocation(&mut self, prompt: String, kind: InvocationKind) -> Result<()> {
        let mut buffered_log: Vec<String> = Vec::new();
        let mut pending_pid: Option<i32> = None;

        if let Some(session) = self.session.as_mut() {
            session.record_user_prompt(&prompt).await?;
            session
                .paths
                .update_metadata(|metadata| {
                    metadata.set_state(TaskState::Running);
                    metadata.last_prompt = Some(prompt.clone());
                })
                .context("failed to update metadata before invocation")?;
        } else {
            buffered_log.push(format!("USER: {}", prompt.trim()));
        }

        let result_file = NamedTempFile::new_in(&self.config.store_root)
            .context("failed to create temporary result file")?;
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
                bail!("cannot resume without an existing task id");
            }
            (Some(_), InvocationKind::Initial) => {
                bail!("initial invocation already performed");
            }
        }

        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        let mut child = command.spawn().context("failed to spawn `codex exec`")?;
        let child_pid = child
            .id()
            .ok_or_else(|| anyhow!("failed to determine child pid"))?;

        let stdout = child
            .stdout
            .take()
            .context("failed to capture stdout of `codex exec`")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture stderr of `codex exec`")?;

        if let Some(session) = self.session.as_mut() {
            session.paths.write_pid(child_pid as i32)?;
            session
                .paths
                .update_metadata(|metadata| metadata.set_state(TaskState::Running))?;
        } else {
            pending_pid = Some(child_pid as i32);
        }

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let wait_handle = tokio::spawn(async move { child.wait().await });

        let mut stdout_done = false;
        let mut stderr_done = false;

        loop {
            tokio::select! {
                        line = stdout_lines.next_line(), if !stdout_done => {
                            match line {
                                Ok(Some(content)) => {
                                    self.handle_stdout_line(
                                        &prompt,
                                        &mut buffered_log,
                &mut pending_pid,
                &content,
            ).await?;
                                }
                            Ok(None) => stdout_done = true,
                            Err(err) => return Err(err).context("failed to read stdout from `codex exec`"),
                        }
                    }
                    line = stderr_lines.next_line(), if !stderr_done => {
                            match line {
                                Ok(Some(content)) => {
                                    self.write_log_line(format!("{STDERR_PREFIX} {content}").as_str()).await?;
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
            .context("failed to join exec child task")?
            .context("`codex exec` terminated unexpectedly")?;

        if status.success() {
            if let Some(session) = self.session.as_mut() {
                session
                    .paths
                    .update_metadata(|metadata| metadata.set_state(TaskState::Stopped))?;
            }
        } else if let Some(session) = self.session.as_mut() {
            session
                .paths
                .update_metadata(|metadata| metadata.set_state(TaskState::Died))?;
        }

        if result_path.exists() {
            let message =
                fs::read_to_string(&result_path).context("failed to read result output")?;
            if let Some(session) = self.session.as_mut() {
                session.record_last_result(&message).await?;
            }
            result_path
                .close()
                .context("failed to remove temporary result file")?;
        }

        Ok(())
    }

    async fn handle_stdout_line(
        &mut self,
        prompt: &str,
        buffered_log: &mut Vec<String>,
        pending_pid: &mut Option<i32>,
        line: &str,
    ) -> Result<()> {
        if self.session.is_none() {
            if let Some(thread_id) = extract_thread_id(line) {
                self.initialize_session(thread_id, prompt, buffered_log, pending_pid)
                    .await?;
                return Ok(());
            }
        }

        if self.session.is_none() {
            if let Some(rendered) = render_event(line) {
                buffered_log.push(rendered);
            }
            return Ok(());
        }

        if let Some(rendered) = render_event(line) {
            self.write_log_line(&rendered).await?;
        }

        Ok(())
    }

    async fn initialize_session(
        &mut self,
        thread_id: TaskId,
        prompt: &str,
        buffered_log: &mut Vec<String>,
        pending_pid: &mut Option<i32>,
    ) -> Result<()> {
        let paths = self.store.task(thread_id.clone());
        paths.ensure_directory()?;

        let mut metadata = if paths.metadata_path().exists() {
            paths.read_metadata()?
        } else {
            let mut meta = TaskMetadata::new(
                thread_id.clone(),
                self.config.title.clone(),
                TaskState::Running,
            );
            meta.initial_prompt = Some(prompt.to_string());
            meta.last_prompt = Some(prompt.to_string());
            meta
        };
        metadata.set_state(TaskState::Running);
        if metadata.config_path.is_none() {
            metadata.config_path = self
                .config
                .config_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string());
        }
        if metadata.working_dir.is_none() {
            metadata.working_dir = self
                .config
                .working_dir
                .as_ref()
                .map(|dir| dir.to_string_lossy().to_string());
        }
        self.store.save_metadata(&metadata)?;

        let log_file = TokioOpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.log_path())
            .await
            .with_context(|| format!("failed to open log file for task {}", thread_id))?;
        let mut session = ActiveSession::new(thread_id.clone(), paths, log_file);

        for line in buffered_log.drain(..) {
            session.write_line(&line).await?;
        }

        if let Some(pid) = pending_pid.take() {
            session.paths.write_pid(pid)?;
        }

        println!("{thread_id}");
        if let Err(err) = tokio_io::stdout().flush().await {
            eprintln!("failed to flush worker handshake: {err:#}");
        }

        self.session = Some(session);
        Ok(())
    }

    async fn write_log_line(&mut self, line: &str) -> Result<()> {
        if let Some(session) = self.session.as_mut() {
            session.write_line(line).await?;
        }
        Ok(())
    }

    async fn finalize(mut self) -> Result<()> {
        if let Some(mut session) = self.session.take() {
            if let Err(err) = session.flush().await {
                eprintln!(
                    "failed to flush log for task {}: {err:#}",
                    session.thread_id
                );
            }
            if let Err(err) = session.paths.remove_pid() {
                eprintln!(
                    "failed to remove pid file for task {}: {err:#}",
                    session.thread_id
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
}

impl ActiveSession {
    fn new(thread_id: TaskId, paths: TaskPaths, log: tokio::fs::File) -> Self {
        Self {
            thread_id,
            paths,
            log: BufWriter::new(log),
        }
    }

    fn from_existing(thread_id: TaskId, paths: TaskPaths, log: tokio::fs::File) -> Self {
        Self::new(thread_id, paths, log)
    }

    async fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.log.write_all(line.as_bytes()).await?;
        self.log.write_all(b"\n").await?;
        Ok(())
    }

    async fn record_user_prompt(&mut self, prompt: &str) -> io::Result<()> {
        if !prompt.trim().is_empty() {
            self.write_line(&format!("USER: {}", prompt.trim())).await?;
        }
        Ok(())
    }

    async fn record_last_result(&mut self, message: &str) -> Result<()> {
        self.paths.write_last_result(message)?;
        self.paths
            .update_metadata(|metadata| metadata.last_result = Some(message.to_string()))?;
        Ok(())
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.log.flush().await
    }
}

fn extract_thread_id(line: &str) -> Option<TaskId> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type")?.as_str()? {
        "thread.started" => value.get("thread_id")?.as_str().map(|s| s.to_string()),
        _ => None,
    }
}

fn render_event(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type = value.get("type")?.as_str()?;
    match event_type {
        "item.completed" => {
            let item = value.get("item")?;
            let item_type = item.get("type")?.as_str().unwrap_or_default();
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            match item_type {
                "agent_message" => Some(format!("ASSISTANT: {}", text.trim())),
                "reasoning" => Some(format!("ASSISTANT (reasoning): {}", text.trim())),
                _ => Some(format!("EVENT {}: {}", item_type, text.trim())),
            }
        }
        "turn.completed" => Some("ASSISTANT: <end>".to_string()),
        "error" => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(format!("ERROR: {}", message))
        }
        _ => None,
    }
}

enum InvocationKind {
    Initial,
    Resume,
}
