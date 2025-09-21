mod cli;
pub mod storage;
pub mod task;
pub mod worker;

use anyhow::{Result, bail};
use clap::Parser;

use crate::cli::{
    ArchiveArgs, Cli, Command, LogArgs, LsArgs, SendArgs, StartArgs, StatusArgs, StopArgs,
};

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
    }
}

fn handle_start(_args: StartArgs) -> Result<()> {
    not_implemented("start")
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
