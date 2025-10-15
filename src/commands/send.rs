use anyhow::Result;

use crate::cli::SendArgs;
use crate::tasks::{SendPromptParams, TaskService};

pub fn handle_send(args: SendArgs) -> Result<()> {
    let service = TaskService::with_default_store(false)?;
    service.send_prompt(SendPromptParams {
        task_id: args.task_id,
        prompt: args.prompt,
    })
}
