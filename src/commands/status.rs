use anyhow::Result;
use serde_json::json;

use crate::cli::StatusArgs;
use crate::tasks::{TaskService, TaskStatusSnapshot};
use crate::timefmt::{TimeFormat, format_time};

/// Output format supported by the status command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusFormat {
    Human,
    Json,
}

/// Options accepted by the status command handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusCommandOptions {
    pub task_id: String,
    pub format: StatusFormat,
    pub time_format: TimeFormat,
}

pub fn handle_status(args: StatusArgs) -> Result<()> {
    let format = if args.json {
        StatusFormat::Json
    } else {
        StatusFormat::Human
    };

    run(StatusCommandOptions {
        task_id: args.task_id,
        format,
        time_format: args.time_format,
    })
}

fn run(options: StatusCommandOptions) -> Result<()> {
    let service = TaskService::with_default_store(false)?;
    let status = service.get_status(&options.task_id)?;

    match options.format {
        StatusFormat::Human => render_human(&status, options.time_format),
        StatusFormat::Json => render_json(&status)?,
    }

    Ok(())
}

fn render_human(record: &TaskStatusSnapshot, time_format: TimeFormat) {
    println!("Task ID: {}", record.metadata.id);
    if let Some(title) = &record.metadata.title {
        println!("Title: {}", title);
    }
    println!("State: {}", record.metadata.state);
    println!(
        "Created At: {}",
        format_time(record.metadata.created_at, time_format)
    );
    println!(
        "Updated At: {}",
        format_time(record.metadata.updated_at, time_format)
    );
    match record.metadata.working_dir.as_deref() {
        Some(dir) => println!("Working Dir: {}", dir),
        None => println!("Working Dir: <none>"),
    }
    if let Some(pid) = record.pid {
        println!("PID: {}", pid);
    }
    match &record.metadata.last_prompt {
        Some(prompt) => {
            println!("Last Prompt:");
            if prompt.trim().is_empty() {
                println!("<empty>");
            } else {
                println!("{}", prompt);
            }
        }
        None => println!("Last Prompt: <none>"),
    }
    println!("Last Result:");
    match &record.metadata.last_result {
        Some(result) if !result.trim().is_empty() => println!("{}", result),
        _ => println!("<none>"),
    }
}

fn render_json(record: &TaskStatusSnapshot) -> Result<()> {
    let payload = json!({
        "id": record.metadata.id.clone(),
        "title": record.metadata.title.clone(),
        "state": record.metadata.state.clone(),
        "created_at": record.metadata.created_at.clone(),
        "updated_at": record.metadata.updated_at.clone(),
        "last_prompt": record.metadata.last_prompt.clone(),
        "last_result": record.metadata.last_result.clone(),
        "working_dir": record.metadata.working_dir.clone(),
        "pid": record.pid,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
