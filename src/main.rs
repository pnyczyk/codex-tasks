mod cli;
mod status;
pub mod storage;
pub mod task;
pub mod worker;

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use clap::Parser;
use libc::{ENXIO, O_NONBLOCK};

use crate::cli::{
    ArchiveArgs, Cli, Command, LogArgs, LsArgs, SendArgs, StartArgs, StatusArgs, StopArgs,
    WorkerArgs,
};
use crate::status::{StatusCommandOptions, StatusFormat, derive_active_state};
use crate::storage::{LOG_FILE_NAME, METADATA_FILE_NAME, TaskPaths, TaskStore};
use crate::task::{TaskMetadata, TaskState};
use crate::worker::launcher::{WorkerLaunchRequest, spawn_worker};

fn main() -> Result<()> {
    let cli = Cli::parse();
    dispatch(cli)
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Start(args) => handle_start(args),
        Command::Send(args) => handle_send(args),
        Command::Status(args) => handle_status(args),
        Command::Log(args) => handle_log(args),
        Command::Stop(args) => handle_stop(args),
        Command::Ls(args) => handle_ls(args),
        Command::Archive(args) => handle_archive(args),
        Command::Worker(args) => handle_worker(args),
    }
}

fn handle_start(args: StartArgs) -> Result<()> {
    let StartArgs { title, prompt } = args;

    let store = TaskStore::default().context("failed to locate task store")?;
    store
        .ensure_layout()
        .context("failed to prepare task store layout")?;

    let task_id = store.generate_task_id();
    let task_paths = store.task(task_id.clone());

    let initial_state = if prompt.is_some() {
        TaskState::Running
    } else {
        TaskState::Idle
    };
    let mut metadata = TaskMetadata::new(task_id.clone(), title.clone(), initial_state);
    metadata.initial_prompt = prompt.clone();

    store
        .save_metadata(&metadata)
        .with_context(|| format!("failed to persist metadata for task {task_id}"))?;

    let mut request = WorkerLaunchRequest::new(task_paths);
    request.title = title;
    request.prompt = prompt;
    let _child = spawn_worker(request).context("failed to launch worker process")?;

    println!("{task_id}");

    Ok(())
}

fn handle_send(args: SendArgs) -> Result<()> {
    let store = TaskStore::default().context("failed to locate task store")?;
    let task_id = args.task_id;
    let prompt = args.prompt;

    let mut metadata = match store.load_metadata(task_id.clone()) {
        Ok(metadata) => metadata,
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if not_found {
                if let Some((_, archived_metadata)) = store.find_archived_task(&task_id)? {
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
        TaskState::Archived => {
            bail!(
                "task {} is ARCHIVED and cannot receive prompts",
                metadata.id
            )
        }
        TaskState::Stopped => {
            bail!("task {} is STOPPED and cannot receive prompts", metadata.id)
        }
        TaskState::Died => {
            bail!("task {} has DIED and cannot receive prompts", metadata.id)
        }
        TaskState::Idle | TaskState::Running => {}
    }

    let paths = store.task(task_id.clone());

    if metadata.state != TaskState::Running {
        metadata = paths.update_metadata(|record| record.set_state(TaskState::Running))?;
    }
    let pipe_path = paths.pipe_path();
    let mut pipe = match OpenOptions::new()
        .write(true)
        .custom_flags(O_NONBLOCK)
        .open(&pipe_path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!(missing_pipe_error(&metadata.id))
        }
        Err(err) if err.raw_os_error() == Some(ENXIO) => {
            bail!(worker_inactive_error(&metadata.id))
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to open prompt pipe at {}", pipe_path.display()));
        }
    };

    set_blocking(&pipe).with_context(|| {
        format!(
            "failed to configure prompt pipe at {} for blocking mode",
            pipe_path.display()
        )
    })?;

    let mut payload = prompt.into_bytes();
    payload.push(b'\n');

    if let Err(err) = pipe.write_all(&payload) {
        if pipe_connection_lost(&err) {
            bail!(worker_inactive_error(&metadata.id));
        }
        return Err(err)
            .with_context(|| format!("failed to write prompt to {}", pipe_path.display()));
    }

    if let Err(err) = pipe.flush() {
        if pipe_connection_lost(&err) {
            bail!(worker_inactive_error(&metadata.id));
        }
        return Err(err)
            .with_context(|| format!("failed to flush prompt pipe at {}", pipe_path.display()));
    }

    Ok(())
}

fn missing_pipe_error(task_id: &str) -> String {
    format!(
        "prompt pipe for task {task_id} is missing; the worker may have STOPPED, DIED, or been ARCHIVED"
    )
}

fn worker_inactive_error(task_id: &str) -> String {
    format!(
        "task {task_id} is not accepting prompts; the worker may have STOPPED, DIED, or been ARCHIVED"
    )
}

fn pipe_connection_lost(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
    )
}

fn set_blocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn handle_status(args: StatusArgs) -> Result<()> {
    let format = if args.json {
        StatusFormat::Json
    } else {
        StatusFormat::Human
    };
    status::run(StatusCommandOptions {
        task_id: args.task_id,
        format,
    })
}

fn handle_log(args: LogArgs) -> Result<()> {
    let store = TaskStore::default()?;
    let log_path = resolve_log_path(&store, &args.task_id)?;
    let file = File::open(&log_path).with_context(|| {
        format!(
            "failed to open log for task {} at {}",
            args.task_id,
            log_path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    print_initial_log(&mut reader, args.lines)?;
    if args.follow {
        follow_log(&mut reader)?;
    }
    Ok(())
}

fn handle_stop(args: StopArgs) -> Result<()> {
    let store = TaskStore::default()?;
    store.ensure_layout()?;
    let paths = store.task(args.task_id.clone());
    let outcome = stop_task(&paths)?;
    match outcome {
        StopOutcome::AlreadyStopped => {
            println!("Task {} is not running; nothing to stop.", args.task_id);
        }
        StopOutcome::Stopped => {
            println!("Task {} stopped.", args.task_id);
        }
    }
    Ok(())
}

fn handle_ls(args: LsArgs) -> Result<()> {
    let store = TaskStore::default()?;
    store.ensure_layout()?;

    let include_archived = args.include_archived;
    let mut tasks = Vec::new();
    tasks.extend(collect_active_tasks(&store)?);
    if include_archived {
        tasks.extend(collect_archived_tasks(&store)?);
    }

    let states = args.states;
    if !states.is_empty() {
        tasks.retain(|task| states.contains(&task.metadata.state));
    }

    tasks.sort_by(|a, b| b.metadata.updated_at.cmp(&a.metadata.updated_at));

    if tasks.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }

    println!(
        "{:<36}  {:<20}  {:<10}  {:<25}  {:<25}  {}",
        "ID", "Title", "State", "Created At", "Updated At", "Location"
    );
    for entry in tasks {
        let title = entry.metadata.title.as_deref().unwrap_or("-");
        let created = entry.metadata.created_at.to_rfc3339();
        let updated = entry.metadata.updated_at.to_rfc3339();
        let location = if entry.archived { "ARCHIVE" } else { "ACTIVE" };
        println!(
            "{:<36}  {:<20}  {:<10}  {:<25}  {:<25}  {}",
            entry.metadata.id, title, entry.metadata.state, created, updated, location
        );
    }

    Ok(())
}

fn handle_archive(args: ArchiveArgs) -> Result<()> {
    let store = TaskStore::default()?;
    store.ensure_layout()?;

    if let Some((_, metadata)) = store.find_archived_task(&args.task_id)? {
        println!("Task {} is already archived.", metadata.id);
        return Ok(());
    }

    let paths = store.task(args.task_id.clone());
    let mut metadata = match paths.read_metadata() {
        Ok(metadata) => metadata,
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if not_found {
                bail!("task {} was not found", args.task_id);
            }
            return Err(err);
        }
    };

    let pid = paths.read_pid()?;
    let derived_state = derive_active_state(&metadata.state, pid);
    if derived_state == TaskState::Running {
        bail!("task {} is RUNNING; stop it before archiving", metadata.id);
    }

    if metadata.state != derived_state {
        metadata.set_state(derived_state.clone());
        paths.write_metadata(&metadata)?;
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

    fs::rename(paths.directory(), &destination).with_context(|| {
        format!(
            "failed to move task {} into archive at {}",
            metadata.id,
            destination.display()
        )
    })?;

    println!(
        "Task {} archived to {}.",
        metadata.id,
        destination.display()
    );

    Ok(())
}

fn handle_worker(args: WorkerArgs) -> Result<()> {
    let config = crate::worker::child::WorkerConfig::new(
        args.task_id,
        args.store_root,
        args.title,
        args.prompt,
    )?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize async runtime for worker")?
        .block_on(crate::worker::child::run_worker(config))
}

#[allow(dead_code)]
fn not_implemented(command: &str) -> Result<()> {
    bail!("`{command}` is not implemented yet. Track progress in future issues.")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopOutcome {
    AlreadyStopped,
    Stopped,
}

const SHUTDOWN_TIMEOUT_SECS: u64 = 10;
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 100;

fn stop_task(paths: &TaskPaths) -> Result<StopOutcome> {
    let pid = match paths.read_pid()? {
        Some(pid) => pid,
        None => {
            cleanup_task_files(paths)?;
            return Ok(StopOutcome::AlreadyStopped);
        }
    };

    if !is_process_running(pid)? {
        cleanup_task_files(paths)?;
        return Ok(StopOutcome::AlreadyStopped);
    }

    match send_quit_signal(paths) {
        Ok(true) => {}
        Ok(false) => {
            cleanup_task_files(paths)?;
            return Ok(StopOutcome::AlreadyStopped);
        }
        Err(err) => return Err(err),
    }

    wait_for_worker_shutdown(paths, pid)?;
    cleanup_task_files(paths)?;

    mark_task_state(paths, TaskState::Stopped)?;

    Ok(StopOutcome::Stopped)
}

fn send_quit_signal(paths: &TaskPaths) -> Result<bool> {
    let pipe_path = paths.pipe_path();
    let mut pipe = match OpenOptions::new().write(true).open(&pipe_path) {
        Ok(pipe) => pipe,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            if err.raw_os_error() == Some(libc::ENXIO) {
                return Ok(false);
            }
            return Err(err)
                .with_context(|| format!("failed to open pipe for task {}", paths.id()));
        }
    };

    if let Err(err) = pipe.write_all(b"/quit\n") {
        if err.kind() != ErrorKind::BrokenPipe {
            return Err(err)
                .with_context(|| format!("failed to write stop signal for task {}", paths.id()));
        }
    }

    if let Err(err) = pipe.flush() {
        if err.kind() != ErrorKind::BrokenPipe {
            return Err(err)
                .with_context(|| format!("failed to flush stop signal for task {}", paths.id()));
        }
    }

    Ok(true)
}

fn wait_for_worker_shutdown(paths: &TaskPaths, pid: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    loop {
        if Instant::now() >= deadline {
            bail!("timed out waiting for task {} to stop", paths.id());
        }

        if paths.read_pid()?.is_none() {
            break;
        }

        if !is_process_running(pid)? {
            break;
        }

        thread::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS));
    }
    Ok(())
}

fn cleanup_task_files(paths: &TaskPaths) -> Result<()> {
    paths.remove_pid()?;
    paths.remove_pipe()?;
    Ok(())
}

fn mark_task_state(paths: &TaskPaths, state: TaskState) -> Result<()> {
    match paths.update_metadata(|metadata| metadata.set_state(state)) {
        Ok(_) => Ok(()),
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if not_found { Ok(()) } else { Err(err) }
        }
    }
}

fn is_process_running(pid: i32) -> Result<bool> {
    if pid <= 0 {
        return Ok(false);
    }

    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return Ok(true);
    }

    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(false),
        Some(code) if code == libc::EPERM => Ok(true),
        _ => Err(err).with_context(|| format!("failed to query status of process {pid}")),
    }
}

struct ListedTask {
    metadata: TaskMetadata,
    archived: bool,
}

fn collect_active_tasks(store: &TaskStore) -> Result<Vec<ListedTask>> {
    let mut tasks = Vec::new();
    let root = store.root().to_path_buf();
    if !root.exists() {
        return Ok(tasks);
    }

    for entry in fs::read_dir(&root)
        .with_context(|| format!("failed to read task directory {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let metadata_path = path.join(METADATA_FILE_NAME);
        if !metadata_path.exists() {
            continue;
        }

        let mut metadata = read_metadata_file(&metadata_path)?;
        let task_paths = store.task(metadata.id.clone());
        let pid = task_paths.read_pid()?;
        metadata.state = derive_active_state(&metadata.state, pid);
        if metadata.last_result.is_none() {
            metadata.last_result = task_paths.read_last_result()?;
        }
        tasks.push(ListedTask {
            metadata,
            archived: false,
        });
    }

    Ok(tasks)
}

fn collect_archived_tasks(store: &TaskStore) -> Result<Vec<ListedTask>> {
    let mut tasks = Vec::new();
    let archive_root = store.archive_root();
    if !archive_root.exists() {
        return Ok(tasks);
    }

    let mut queue = VecDeque::from([archive_root]);
    while let Some(dir) = queue.pop_front() {
        let metadata_path = dir.join(METADATA_FILE_NAME);
        if metadata_path.exists() {
            let metadata = read_metadata_file(&metadata_path)?;
            tasks.push(ListedTask {
                metadata,
                archived: true,
            });
            continue;
        }

        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read archive directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read archive entry in {}", dir.display()))?;
            if entry.file_type()?.is_dir() {
                queue.push_back(entry.path());
            }
        }
    }

    Ok(tasks)
}

fn read_metadata_file(path: &Path) -> Result<TaskMetadata> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read metadata file {}", path.display()))?;
    let metadata: TaskMetadata = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse metadata file {}", path.display()))?;

    if let Some(parent) = path
        .parent()
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
    {
        ensure!(
            metadata.id == parent,
            "metadata id {} does not match directory {}",
            metadata.id,
            parent
        );
    }

    Ok(metadata)
}

fn resolve_log_path(store: &TaskStore, task_id: &str) -> Result<PathBuf> {
    let active_path = store.task(task_id.to_string()).log_path();
    if active_path.exists() {
        return Ok(active_path);
    }

    if let Some(path) = find_archived_log_path(store, task_id)? {
        return Ok(path);
    }

    bail!(
        "log file for task {task_id} was not found under {} or {}",
        store.root().display(),
        store.archive_root().display()
    )
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
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
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

fn print_initial_log(reader: &mut BufReader<File>, limit: Option<usize>) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();

    match limit {
        Some(limit) => {
            let mut lines = VecDeque::new();
            loop {
                buffer.clear();
                let bytes = read_line_retry(reader, &mut buffer)
                    .context("failed to read from log while preparing output")?;
                if bytes == 0 {
                    break;
                }

                if limit == 0 {
                    continue;
                }

                if lines.len() == limit {
                    lines.pop_front();
                }
                lines.push_back(buffer.clone());
            }

            for line in lines {
                stdout
                    .write_all(line.as_bytes())
                    .context("failed to write log output")?;
            }
        }
        None => loop {
            buffer.clear();
            let bytes = read_line_retry(reader, &mut buffer)
                .context("failed to read from log while preparing output")?;
            if bytes == 0 {
                break;
            }
            stdout
                .write_all(buffer.as_bytes())
                .context("failed to write log output")?;
        },
    }

    stdout
        .flush()
        .context("failed to flush log output to stdout")?;
    Ok(())
}

fn follow_log(reader: &mut BufReader<File>) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();
    loop {
        buffer.clear();
        match read_line_retry(reader, &mut buffer) {
            Ok(0) => {
                stdout
                    .flush()
                    .context("failed to flush log output to stdout")?;
                thread::sleep(Duration::from_millis(250));
            }
            Ok(_) => {
                stdout
                    .write_all(buffer.as_bytes())
                    .context("failed to write log output")?;
                stdout
                    .flush()
                    .context("failed to flush log output to stdout")?;
            }
            Err(err) => {
                return Err(err).context("failed to read from log while following");
            }
        }
    }
}

fn read_line_retry<R: BufRead>(reader: &mut R, buffer: &mut String) -> io::Result<usize> {
    loop {
        match reader.read_line(buffer) {
            Ok(bytes) => return Ok(bytes),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn not_implemented_returns_err() {
        let err = not_implemented("start").unwrap_err();
        assert_eq!(
            "`start` is not implemented yet. Track progress in future issues.",
            err.to_string()
        );
    }

    #[test]
    fn stop_task_reports_already_stopped_when_pid_missing() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout().expect("layout");
        let paths = store.task("task-1".to_string());
        paths.ensure_directory().expect("directory");

        let outcome = stop_task(&paths).expect("stop task");
        assert_eq!(outcome, StopOutcome::AlreadyStopped);
    }

    #[test]
    fn stop_task_sends_quit_and_cleans_up_files() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout().expect("layout");
        let paths = store.task("task-2".to_string());
        paths.ensure_directory().expect("directory");

        create_fifo(paths.pipe_path().as_path()).expect("create fifo");
        let pid = i32::try_from(std::process::id()).expect("pid fits in i32");
        paths.write_pid(pid).expect("write pid");

        let pipe_path = paths.pipe_path();
        let pid_path = paths.pid_path();
        let reader_handle = std::thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&pipe_path)
                .expect("open pipe for read");
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read line");
            assert_eq!(line.trim(), "/quit");
            std::fs::remove_file(&pid_path).expect("remove pid");
        });

        let outcome = stop_task(&paths).expect("stop task");
        assert_eq!(outcome, StopOutcome::Stopped);

        reader_handle.join().expect("reader thread");

        assert!(!paths.pid_path().exists());
        assert!(!paths.pipe_path().exists());
    }

    fn create_fifo(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("pipe path");
        let mode = 0o600;
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to create fifo at {}", path.display()));
        }
        Ok(())
    }
}
