mod cli;
mod commands;
mod status;
pub mod storage;
pub mod task;
pub mod worker;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    dispatch(cli)
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Start(args) => commands::handle_start(args),
        Command::Send(args) => commands::handle_send(args),
        Command::Status(args) => commands::handle_status(args),
        Command::Log(args) => commands::handle_log(args),
        Command::Stop(args) => commands::handle_stop(args),
        Command::Ls(args) => commands::handle_ls(args),
        Command::Archive(args) => commands::handle_archive(args),
        Command::Worker(args) => commands::handle_worker(args),
    }
}
