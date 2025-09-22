mod cli;
mod status;
pub mod storage;
pub mod task;
pub mod worker;

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use libc::{ENXIO, O_NONBLOCK};

use crate::cli::{
    ArchiveArgs, Cli, Command, LogArgs, LsArgs, SendArgs, StartArgs, StatusArgs, StopArgs,
    WorkerArgs,
};
use crate::status::{StatusCommandOptions, StatusFormat};
use crate::storage::{TaskPaths, TaskStore};
use crate::task::{TaskMetadata, TaskState};
use crate::worker::launcher::{spawn_worker, WorkerLaunchRequest};

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

    let mut metadata = TaskMetadata::new(task_id.clone(), title.clone(), TaskState::Running);
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

    let metadata = match store.load_metadata(task_id.clone()) {
        Ok(metadata) => metadata,
        Err(err) => {
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound)
            {
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

fn handle_log(_args: LogArgs) -> Result<()> {
    not_implemented("log")
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

fn handle_ls(_args: LsArgs) -> Result<()> {
    not_implemented("ls")
}

fn handle_archive(_args: ArchiveArgs) -> Result<()> {
    not_implemented("archive")
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
