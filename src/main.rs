mod cli;
pub mod storage;
pub mod task;
pub mod worker;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{
    ArchiveArgs, Cli, Command, LogArgs, LsArgs, SendArgs, StartArgs, StatusArgs, StopArgs,
    WorkerArgs,
};
use crate::storage::TaskStore;
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

fn handle_send(_args: SendArgs) -> Result<()> {
    not_implemented("send")
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
