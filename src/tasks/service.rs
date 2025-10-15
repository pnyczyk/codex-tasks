use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;

use crate::commands::common::is_process_running;
use crate::commands::tasks::{collect_active_tasks, collect_archived_tasks};
use crate::tasks::{
    LOG_FILE_NAME, TaskMetadata, TaskPaths, TaskState, TaskStore, derive_active_state,
};
use crate::worker::launcher::{WorkerLaunchRequest, spawn_worker};

const SHUTDOWN_TIMEOUT_SECS: u64 = 10;
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 100;

pub const LOG_WAIT_TIMEOUT_SECS: u64 = 10;
pub const LOG_WAIT_POLL_INTERVAL_MS: u64 = 100;

/// Shared task service that encapsulates task store interactions used by both the CLI and MCP
/// adapters.
#[derive(Clone, Debug)]
pub struct TaskService {
    store: TaskStore,
    _allow_unsafe: bool,
}

impl TaskService {
    /// Creates a service backed by an explicit task store.
    pub fn new(store: TaskStore, allow_unsafe: bool) -> Self {
        Self {
            store,
            _allow_unsafe: allow_unsafe,
        }
    }

    /// Creates a service using the default on-disk task store layout.
    pub fn with_default_store(allow_unsafe: bool) -> Result<Self> {
        Ok(Self {
            store: TaskStore::default()?,
            _allow_unsafe: allow_unsafe,
        })
    }

    /// Starts a new task worker using the provided parameters and returns the spawned thread id.
    pub fn start_task(&self, params: StartTaskParams) -> Result<StartTaskResult> {
        let StartTaskParams {
            title,
            prompt,
            config_file,
            working_dir,
            repo_url,
            repo_ref,
        } = params;

        if prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }

        self.store.ensure_layout()?;

        let config_file = resolve_config_file(config_file)?;
        let working_dir =
            prepare_working_directory(working_dir, repo_url.as_deref(), repo_ref.as_deref())?;
        let working_dir = match working_dir {
            Some(path) => Some(make_absolute(path)?),
            None => {
                let cwd = env::current_dir()
                    .context("failed to determine current working directory for worker")?;
                Some(make_absolute(cwd)?)
            }
        };

        let mut request = WorkerLaunchRequest::new(self.store.root().to_path_buf(), prompt);
        request.title = title;
        request.config_path = config_file;
        request.working_directory = working_dir.clone();

        let mut child = spawn_worker(request).context("failed to launch worker process")?;
        let thread_id = receive_thread_id(&mut child)?;
        drop(child);

        Ok(StartTaskResult { thread_id })
    }

    /// Restarts a task worker to process an additional prompt for an existing task.
    pub fn send_prompt(&self, params: SendPromptParams) -> Result<()> {
        let SendPromptParams { task_id, prompt } = params;

        if prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }

        let metadata = match self.store.load_metadata(task_id.clone()) {
            Ok(metadata) => metadata,
            Err(err) => {
                let not_found = err
                    .downcast_ref::<io::Error>()
                    .is_some_and(|io_err| io_err.kind() == io::ErrorKind::NotFound);
                if not_found {
                    if let Some((_, archived_metadata)) = self.store.find_archived_task(&task_id)? {
                        bail!(
                            "task {} is ARCHIVED and cannot receive prompts",
                            archived_metadata.id
                        );
                    }
                    bail!("task {task_id} was not found");
                }
                return Err(err);
            }
        };

        match metadata.state {
            TaskState::Archived => bail!(
                "task {} is ARCHIVED and cannot receive prompts",
                metadata.id
            ),
            TaskState::Died => bail!("task {} has DIED and cannot receive prompts", metadata.id),
            TaskState::Stopped | TaskState::Running => {}
        }

        let paths = self.store.task(metadata.id.clone());
        if let Some(pid) = paths.read_pid()? {
            if is_process_running(pid)? {
                bail!(
                    "task {} is currently running; wait for completion or stop it first",
                    metadata.id
                );
            }
            let _ = paths.remove_pid();
        }

        let mut request = WorkerLaunchRequest::new(self.store.root().to_path_buf(), prompt);
        request.task_id = Some(metadata.id.clone());
        request.title = metadata.title.clone();
        if let Some(path) = metadata.config_path.as_ref() {
            request.config_path = Some(PathBuf::from(path));
        }
        if let Some(dir) = metadata.working_dir.as_ref() {
            request.working_directory = Some(PathBuf::from(dir));
        }

        let mut child = spawn_worker(request).context("failed to launch worker process")?;
        if let Some(stdout) = child.stdout.take() {
            drop(stdout);
        }
        drop(child);

        Ok(())
    }

    /// Loads metadata and runtime information for the requested task.
    pub fn get_status(&self, task_id: &str) -> Result<TaskStatusSnapshot> {
        let paths = self.store.task(task_id.to_string());
        match paths.read_metadata() {
            Ok(mut metadata) => {
                let pid = paths.read_pid()?;
                let derived_state = derive_active_state(&metadata.state, pid);
                metadata.state = derived_state;
                if metadata.last_result.is_none() {
                    metadata.last_result = paths.read_last_result()?;
                }
                Ok(TaskStatusSnapshot { metadata, pid })
            }
            Err(err) => {
                let not_found = err
                    .downcast_ref::<io::Error>()
                    .is_some_and(|io_err| io_err.kind() == io::ErrorKind::NotFound);
                if !not_found {
                    return Err(err);
                }

                let Some((paths, mut metadata)) = self.store.find_archived_task(task_id)? else {
                    bail!("task {task_id} was not found in the task store");
                };
                metadata.state = TaskState::Archived;
                if metadata.last_result.is_none() {
                    metadata.last_result = paths.read_last_result()?;
                }
                Ok(TaskStatusSnapshot {
                    metadata,
                    pid: None,
                })
            }
        }
    }

    /// Lists tasks according to the provided options, sorted by most recently updated.
    pub fn list_tasks(&self, options: ListTasksOptions) -> Result<Vec<TaskListEntry>> {
        self.store.ensure_layout()?;

        let mut tasks = Vec::new();
        tasks.extend(collect_active_tasks(&self.store)?);
        if options.include_archived {
            tasks.extend(collect_archived_tasks(&self.store)?);
        }

        if !options.states.is_empty() {
            tasks.retain(|task| options.states.contains(&task.metadata.state));
        }

        tasks.sort_by(|a, b| b.metadata.updated_at.cmp(&a.metadata.updated_at));

        Ok(tasks
            .into_iter()
            .map(|task| TaskListEntry {
                metadata: task.metadata,
            })
            .collect())
    }

    /// Resolves the log path and metadata for the specified task, optionally waiting for the log
    /// file to appear.
    pub fn prepare_log_descriptor(&self, task_id: &str, wait: bool) -> Result<LogDescriptor> {
        self.store.ensure_layout()?;
        let path = resolve_log_path(&self.store, task_id, wait)?;
        let metadata = resolve_follow_metadata(&self.store, task_id)?;
        Ok(LogDescriptor {
            task_id: task_id.to_string(),
            path,
            metadata,
        })
    }

    /// Stops a specific task if it is running.
    pub fn stop_task(&self, task_id: &str) -> Result<StopOutcome> {
        self.store.ensure_layout()?;
        let paths = self.store.task(task_id.to_string());
        stop_task_paths(&paths)
    }

    /// Stops every running task and returns their outcomes.
    pub fn stop_all_running(&self) -> Result<Vec<StopTaskReport>> {
        self.store.ensure_layout()?;
        let mut running = Vec::new();
        for task in collect_active_tasks(&self.store)? {
            let paths = self.store.task(task.metadata.id.clone());
            let pid = paths.read_pid()?;
            if let Some(pid) = pid {
                if is_process_running(pid)? {
                    running.push(task.metadata.id.clone());
                }
            }
        }

        let mut reports = Vec::with_capacity(running.len());
        for task_id in running {
            let paths = self.store.task(task_id.clone());
            let outcome = stop_task_paths(&paths)?;
            reports.push(StopTaskReport { task_id, outcome });
        }

        Ok(reports)
    }

    /// Archives a specific task if it is stopped or died.
    pub fn archive_task(&self, task_id: &str) -> Result<ArchiveTaskOutcome> {
        self.store.ensure_layout()?;
        archive_task_inner(&self.store, task_id)
    }

    /// Archives all eligible tasks, returning a summary of actions taken.
    pub fn archive_all(&self) -> Result<ArchiveAllSummary> {
        self.store.ensure_layout()?;
        let tasks = collect_active_tasks(&self.store)?;

        let mut candidates = Vec::new();
        let mut skipped = Vec::new();

        for task in tasks {
            match task.metadata.state {
                TaskState::Stopped | TaskState::Died => candidates.push(task.metadata.id.clone()),
                TaskState::Running => skipped.push((task.metadata.id.clone(), task.metadata.state)),
                TaskState::Archived => {}
            }
        }

        let mut summary = ArchiveAllSummary {
            skipped,
            archived: Vec::new(),
            already: Vec::new(),
            failures: Vec::new(),
        };

        for task_id in candidates {
            match archive_task_inner(&self.store, &task_id) {
                Ok(ArchiveTaskOutcome::Archived { id, destination }) => {
                    summary.archived.push((id, destination));
                }
                Ok(ArchiveTaskOutcome::AlreadyArchived { id }) => {
                    summary.already.push(id);
                }
                Err(err) => {
                    summary.failures.push((task_id, err));
                }
            }
        }

        Ok(summary)
    }
}

/// Parameters required to start a task worker.
#[derive(Clone, Debug)]
pub struct StartTaskParams {
    pub title: Option<String>,
    pub prompt: String,
    pub config_file: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
}

/// Result of starting a task worker.
#[derive(Clone, Debug)]
pub struct StartTaskResult {
    pub thread_id: String,
}

/// Parameters required to send a prompt to an existing task.
#[derive(Clone, Debug)]
pub struct SendPromptParams {
    pub task_id: String,
    pub prompt: String,
}

/// Snapshot of task metadata and derived runtime state.
#[derive(Clone, Debug)]
pub struct TaskStatusSnapshot {
    pub metadata: TaskMetadata,
    pub pid: Option<i32>,
}

/// A task entry returned by list operations.
#[derive(Clone, Debug)]
pub struct TaskListEntry {
    pub metadata: TaskMetadata,
}

/// Options that influence task listing behaviour.
#[derive(Clone, Debug, Default)]
pub struct ListTasksOptions {
    pub include_archived: bool,
    pub states: Vec<TaskState>,
}

/// Outcome of attempting to stop a worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopOutcome {
    AlreadyStopped,
    Stopped,
}

/// Report produced when stopping multiple tasks.
#[derive(Clone, Debug)]
pub struct StopTaskReport {
    pub task_id: String,
    pub outcome: StopOutcome,
}

/// Outcome emitted when archiving an individual task.
#[derive(Clone, Debug)]
pub enum ArchiveTaskOutcome {
    Archived { id: String, destination: PathBuf },
    AlreadyArchived { id: String },
}

/// Summary of archiving multiple tasks.
#[derive(Debug, Default)]
pub struct ArchiveAllSummary {
    pub skipped: Vec<(String, TaskState)>,
    pub archived: Vec<(String, PathBuf)>,
    pub already: Vec<String>,
    pub failures: Vec<(String, anyhow::Error)>,
}

/// Metadata required to follow log updates.
#[derive(Clone, Debug)]
pub enum FollowMetadata {
    Active { store: TaskStore },
    Archived { state: TaskState },
    Missing,
}

/// Descriptor containing the log path and follow metadata.
#[derive(Clone, Debug)]
pub struct LogDescriptor {
    pub task_id: String,
    pub path: PathBuf,
    pub metadata: FollowMetadata,
}

fn archive_task_inner(store: &TaskStore, task_id: &str) -> Result<ArchiveTaskOutcome> {
    if let Some((_, metadata)) = store.find_archived_task(task_id)? {
        return Ok(ArchiveTaskOutcome::AlreadyArchived { id: metadata.id });
    }

    let paths = store.task(task_id.to_string());
    let mut metadata = match paths.read_metadata() {
        Ok(metadata) => metadata,
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound);
            if not_found {
                bail!("task {task_id} was not found");
            }
            return Err(err);
        }
    };

    let pid = paths.read_pid()?;
    let derived_state = derive_active_state(&metadata.state, pid);
    if metadata.state != derived_state {
        metadata.set_state(derived_state.clone());
        paths.write_metadata(&metadata)?;
    }

    if derived_state == TaskState::Running {
        bail!("task {} is RUNNING; stop it before archiving", metadata.id);
    }

    if let Some(pid) = pid {
        if is_process_running(pid)? {
            bail!("task {} is RUNNING; stop it before archiving", metadata.id);
        }
    }

    paths.remove_pid()?;
    paths.remove_pipe()?;

    let now = Utc::now();
    metadata.state = TaskState::Archived;
    metadata.updated_at = now;
    paths.write_metadata(&metadata)?;

    let bucket = store.ensure_archive_bucket(now)?;
    let destination = bucket.join(&metadata.id);
    if destination.exists() {
        bail!(
            "archive destination {} already exists for task {}",
            destination.display(),
            metadata.id
        );
    }

    std::fs::rename(paths.directory(), &destination).with_context(|| {
        format!(
            "failed to move task {} into archive at {}",
            metadata.id,
            destination.display()
        )
    })?;

    Ok(ArchiveTaskOutcome::Archived {
        id: metadata.id,
        destination,
    })
}

fn resolve_log_path(store: &TaskStore, task_id: &str, wait: bool) -> Result<PathBuf> {
    let active_path = store.task(task_id.to_string()).log_path();
    let deadline = if wait {
        Some(Instant::now() + Duration::from_secs(LOG_WAIT_TIMEOUT_SECS))
    } else {
        None
    };

    loop {
        if active_path.exists() {
            return Ok(active_path.clone());
        }

        if let Some(path) = find_archived_log_path(store, task_id)? {
            return Ok(path);
        }

        match deadline {
            Some(limit) if Instant::now() < limit => {
                thread::sleep(Duration::from_millis(LOG_WAIT_POLL_INTERVAL_MS));
            }
            Some(_) | None => {
                bail!(
                    "log file for task {task_id} was not found under {} or {}",
                    store.root().display(),
                    store.archive_root().display()
                );
            }
        }
    }
}

fn resolve_follow_metadata(store: &TaskStore, task_id: &str) -> Result<FollowMetadata> {
    let active_metadata_path = store.task(task_id.to_string()).metadata_path();
    if active_metadata_path.exists() {
        return Ok(FollowMetadata::Active {
            store: store.clone(),
        });
    }

    match store.find_archived_task(task_id) {
        Ok(Some((_, metadata))) => Ok(FollowMetadata::Archived {
            state: metadata.state,
        }),
        Ok(None) => Ok(FollowMetadata::Missing),
        Err(_) => Ok(FollowMetadata::Missing),
    }
}

fn find_archived_log_path(store: &TaskStore, task_id: &str) -> Result<Option<PathBuf>> {
    let archive_root = store.archive_root();
    if !archive_root.exists() {
        return Ok(None);
    }

    let mut stack = vec![archive_root];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read archive directory {}", dir.display())
                });
            }
        };

        for entry in entries {
            let entry = entry.with_context(|| {
                format!(
                    "failed to read entry in archive directory {}",
                    dir.display()
                )
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect archive entry {}", path.display()))?;

            if file_type.is_dir() {
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name == task_id)
                    .unwrap_or(false)
                {
                    let candidate = path.join(LOG_FILE_NAME);
                    if candidate.exists() {
                        return Ok(Some(candidate));
                    }
                }
                stack.push(path);
            }
        }
    }

    Ok(None)
}

fn resolve_config_file(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };

    let absolute = make_absolute(path)?;
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve config file at {}", absolute.display()))?;
    ensure!(
        canonical.is_file(),
        "config file {} does not exist or is not a file",
        canonical.display()
    );
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!(
                "config file path {} is missing file name",
                canonical.display()
            )
        })?;
    ensure!(
        name == "config.toml",
        "custom config file must be named `config.toml` (got {name})",
        name = name
    );
    Ok(Some(canonical))
}

fn prepare_working_directory(
    working_dir: Option<PathBuf>,
    repo: Option<&str>,
    repo_ref: Option<&str>,
) -> Result<Option<PathBuf>> {
    let resolved = match working_dir {
        Some(path) => Some(make_absolute(path)?),
        None => None,
    };

    if let Some(repo_url) = repo {
        let repo_spec_storage = if Path::new(repo_url).exists() {
            Some(make_absolute(PathBuf::from(repo_url))?.into_os_string())
        } else {
            None
        };
        let repo_spec: &OsStr = repo_spec_storage
            .as_ref()
            .map(|value| value.as_os_str())
            .unwrap_or_else(|| OsStr::new(repo_url));
        let target = resolved
            .as_ref()
            .ok_or_else(|| anyhow!("`--working-dir` is required when `--repo` is provided"))?;
        clone_repository(repo_spec, repo_ref, target)?;
    } else if let Some(path) = resolved.as_ref() {
        if !path.exists() {
            fs::create_dir_all(path).with_context(|| {
                format!("failed to create working directory {}", path.display())
            })?;
        }
    }

    match resolved {
        Some(path) => {
            let canonical = path.canonicalize().with_context(|| {
                format!("failed to resolve working directory {}", path.display())
            })?;
            Ok(Some(canonical))
        }
        None => Ok(None),
    }
}

fn clone_repository(repo_spec: &OsStr, repo_ref: Option<&str>, target_dir: &Path) -> Result<()> {
    let parent = target_dir.parent().ok_or_else(|| {
        anyhow!(
            "working directory {} is missing a parent directory",
            target_dir.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create parent directory {} for working dir",
            parent.display()
        )
    })?;

    if target_dir.exists() {
        bail!(
            "working directory {} already exists; refusing to overwrite",
            target_dir.display()
        );
    }

    let status = StdCommand::new("git")
        .arg("clone")
        .arg(repo_spec)
        .arg(target_dir)
        .status()
        .context("failed to run git clone")?;
    ensure!(
        status.success(),
        "`git clone` exited with status {status}",
        status = status
    );

    if let Some(reference) = repo_ref {
        let fetch_status = StdCommand::new("git")
            .current_dir(target_dir)
            .args(["fetch", "origin", reference])
            .status()
            .with_context(|| format!("failed to fetch {reference}"))?;
        ensure!(
            fetch_status.success(),
            "`git fetch origin {reference}` exited with status {fetch_status}",
            reference = reference,
            fetch_status = fetch_status
        );

        let checkout_status = StdCommand::new("git")
            .current_dir(target_dir)
            .args(["checkout", reference])
            .status()
            .with_context(|| format!("failed to checkout {reference} after fetch"))?;
        ensure!(
            checkout_status.success(),
            "`git checkout {reference}` exited with status {checkout_status}",
            reference = reference,
            checkout_status = checkout_status
        );
    }

    Ok(())
}

fn make_absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = env::current_dir().context("failed to resolve current working directory")?;
        Ok(cwd.join(path))
    }
}

fn receive_thread_id(child: &mut Child) -> Result<String> {
    let stdout = child
        .stdout
        .take()
        .context("worker did not expose stdout for handshake")?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader
            .read_line(&mut line)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                if bytes == 0 {
                    Err(anyhow!("worker exited before publishing thread id"))
                } else {
                    Ok(line.trim().to_string())
                }
            });
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(id)) if !id.is_empty() => Ok(id),
        Ok(Ok(_)) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("worker returned empty thread identifier");
        }
        Ok(Err(err)) => {
            let _ = child.kill();
            if let Ok(status) = child.wait() {
                bail!("failed to start worker: {err:#}. worker exited with {status}");
            } else {
                bail!("failed to start worker: {err:#}");
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out waiting for worker to publish thread id");
        }
    }
}

fn stop_task_paths(paths: &TaskPaths) -> Result<StopOutcome> {
    let pid = match paths.read_pid()? {
        Some(pid) => pid,
        None => return Ok(StopOutcome::AlreadyStopped),
    };

    if !is_process_running(pid)? {
        let _ = paths.remove_pid();
        return Ok(StopOutcome::AlreadyStopped);
    }

    send_signal(pid, libc::SIGTERM)?;
    wait_for_worker_shutdown(pid)?;
    let _ = paths.remove_pid();
    mark_task_state(paths, TaskState::Stopped)?;

    Ok(StopOutcome::Stopped)
}

fn wait_for_worker_shutdown(pid: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    loop {
        let mut status: libc::c_int = 0;
        let wait_result =
            unsafe { libc::waitpid(pid, &mut status as *mut libc::c_int, libc::WNOHANG) };
        if wait_result == pid {
            break;
        } else if wait_result == 0 {
            // child still running
        } else if wait_result == -1 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ECHILD) {
                if !is_process_running(pid)? {
                    break;
                }
            } else {
                return Err(err).with_context(|| format!("failed to wait for process {pid}"));
            }
        }

        if Instant::now() >= deadline {
            send_signal(pid, libc::SIGKILL)?;
            thread::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS));
            if !is_process_running(pid)? {
                break;
            }
            bail!("timed out waiting for worker {pid} to stop");
        }

        if !is_process_running(pid)? {
            break;
        }

        thread::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS));
    }
    Ok(())
}

fn send_signal(pid: i32, signal: libc::c_int) -> Result<()> {
    if pid <= 0 {
        return Ok(());
    }

    let result = unsafe { libc::kill(pid, signal) };
    if result == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err).with_context(|| format!("failed to signal process {pid}"))
        }
    } else {
        Ok(())
    }
}

fn mark_task_state(paths: &TaskPaths, state: TaskState) -> Result<()> {
    match paths.update_metadata(|metadata| metadata.set_state(state)) {
        Ok(_) => Ok(()),
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound);
            if not_found { Ok(()) } else { Err(err) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StopOutcome, TaskService, TaskStore};
    use anyhow::Result;
    use tempfile::tempdir;

    #[test]
    fn stop_task_reports_already_stopped_when_pid_missing() -> Result<()> {
        let tmp = tempdir()?;
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout()?;
        let service = TaskService::new(store.clone(), false);
        let paths = store.task("task-1".to_string());
        paths.ensure_directory()?;

        let outcome = service.stop_task("task-1")?;
        assert_eq!(outcome, StopOutcome::AlreadyStopped);
        Ok(())
    }
}
