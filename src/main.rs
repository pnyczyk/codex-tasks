mod cli;
pub mod storage;
pub mod task;
pub mod worker;

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{bail, Context, Result};
use clap::Parser;
use libc::{ENXIO, O_NONBLOCK};

use crate::cli::{
    ArchiveArgs, Cli, Command, LogArgs, LsArgs, SendArgs, StartArgs, StatusArgs, StopArgs,
    WorkerArgs,
};
use crate::storage::TaskStore;
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

fn handle_status(_args: StatusArgs) -> Result<()> {
    not_implemented("status")
}

fn handle_log(_args: LogArgs) -> Result<()> {
    not_implemented("log")
}

fn handle_stop(_args: StopArgs) -> Result<()> {
    not_implemented("stop")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_implemented_returns_err() {
        let err = not_implemented("start").unwrap_err();
        assert_eq!(
            "`start` is not implemented yet. Track progress in future issues.",
            err.to_string()
        );
    }
}
