use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::task::TaskState;

/// Top-level CLI definition for the `codex-tasks` binary.
#[derive(Debug, Parser)]
#[command(
    name = "codex-tasks",
    about = "Manage long-running Codex helper tasks",
    author,
    version,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Supported subcommands for the CLI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start a new Codex task worker.
    Start(StartArgs),
    /// Send a prompt to an existing task.
    Send(SendArgs),
    /// Inspect metadata and status for a task.
    Status(StatusArgs),
    /// Stream the transcript log for a task.
    Log(LogArgs),
    /// Gracefully stop a running task.
    Stop(StopArgs),
    /// List known tasks, optionally filtered by state.
    Ls(LsArgs),
    /// Archive a completed task.
    Archive(ArchiveArgs),
    /// Internal entry-point used to run a worker process.
    #[command(hide = true)]
    Worker(WorkerArgs),
}

/// Arguments for the `start` subcommand.
#[derive(Debug, Args)]
pub struct StartArgs {
    /// Optional human readable title for the new task.
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Initial prompt to send immediately after the worker launches.
    pub prompt: Option<String>,
}

/// Arguments for the `send` subcommand.
#[derive(Debug, Args)]
pub struct SendArgs {
    /// Identifier of the task that should receive the prompt.
    pub task_id: String,
    /// Prompt that will be forwarded to the task worker.
    pub prompt: String,
}

/// Arguments for the `status` subcommand.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Identifier of the task that should be inspected.
    pub task_id: String,
}

/// Arguments for the `log` subcommand.
#[derive(Debug, Args)]
pub struct LogArgs {
    /// Follow the log output until interrupted.
    #[arg(short = 'f', long)]
    pub follow: bool,
    /// Only print the last N lines before optionally following.
    #[arg(short = 'n', long)]
    pub lines: Option<usize>,
    /// Identifier of the task whose log should be streamed.
    pub task_id: String,
}

/// Arguments for the `stop` subcommand.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// Identifier of the task that should be stopped.
    pub task_id: String,
}

/// Arguments for the `ls` subcommand.
#[derive(Debug, Args)]
pub struct LsArgs {
    /// Restrict results to tasks that match the provided states.
    #[arg(long = "state", value_enum)]
    pub states: Vec<TaskState>,
}

/// Arguments for the `archive` subcommand.
#[derive(Debug, Args)]
pub struct ArchiveArgs {
    /// Identifier of the task that should be archived.
    pub task_id: String,
}

/// Hidden arguments used when the CLI binary is re-executed as a worker.
#[derive(Debug, Args)]
pub struct WorkerArgs {
    /// Identifier associated with the task managed by this worker.
    #[arg(long = "task-id")]
    pub task_id: String,
    /// Filesystem root containing task artifacts.
    #[arg(long = "store-root")]
    pub store_root: PathBuf,
    /// Optional task title (primarily used for diagnostics).
    #[arg(long)]
    pub title: Option<String>,
    /// Optional prompt to send once the worker is fully initialized.
    #[arg(long)]
    pub prompt: Option<String>,
}
