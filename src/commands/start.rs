use std::io::{self, Read};

use anyhow::{Context, Result, bail};

use crate::cli::StartArgs;
use crate::services::tasks::{StartTaskParams, TaskService};

pub fn handle_start(args: StartArgs) -> Result<()> {
    let StartArgs {
        title,
        prompt,
        config_file,
        working_dir,
        repo,
        repo_ref,
    } = args;

    let prompt = resolve_start_prompt(prompt)?;

    let service = TaskService::with_default_store(false)?;
    let result = service.start_task(StartTaskParams {
        title,
        prompt,
        config_file,
        working_dir,
        repo_url: repo,
        repo_ref,
    })?;

    println!("{}", result.thread_id);

    Ok(())
}

fn resolve_start_prompt(raw_prompt: String) -> Result<String> {
    if raw_prompt == "-" {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read prompt from stdin")?;
        if buffer.trim().is_empty() {
            bail!("no prompt provided via stdin");
        }
        Ok(buffer)
    } else if raw_prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    } else {
        Ok(raw_prompt)
    }
}
