use std::collections::HashSet;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use crate::cli::StatusArgs;
use crate::tasks::{ListTasksOptions, TaskService, TaskState, TaskStatusSnapshot};
use crate::timefmt::{TimeFormat, format_time};

/// Output format supported by the status command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusFormat {
    Human,
    Json,
}

/// Wait semantics supported by the status command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitMode {
    None,
    All,
    Any,
}

/// Options accepted by the status command handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusCommandOptions {
    pub task_ids: Vec<String>,
    pub include_all: bool,
    pub include_all_running: bool,
    pub format: StatusFormat,
    pub time_format: TimeFormat,
    pub wait_mode: WaitMode,
}

pub fn handle_status(args: StatusArgs) -> Result<()> {
    let format = if args.json {
        StatusFormat::Json
    } else {
        StatusFormat::Human
    };

    let wait_mode = if args.wait_any {
        WaitMode::Any
    } else if args.wait {
        WaitMode::All
    } else {
        WaitMode::None
    };

    run(StatusCommandOptions {
        task_ids: args.task_ids,
        include_all: args.all,
        include_all_running: args.all_running,
        format,
        time_format: args.time_format,
        wait_mode,
    })
}

fn run(options: StatusCommandOptions) -> Result<()> {
    let service = TaskService::with_default_store(false)?;
    let targets = resolve_targets(&service, &options)?;
    if targets.is_empty() {
        bail!("no tasks matched the requested selectors");
    }

    let records = collect_statuses(&service, &targets, options.wait_mode)?;

    match options.format {
        StatusFormat::Human => render_human(&records, options.time_format),
        StatusFormat::Json => render_json(&records)?,
    }

    Ok(())
}

fn resolve_targets(service: &TaskService, options: &StatusCommandOptions) -> Result<Vec<String>> {
    if options.include_all {
        let entries = service.list_tasks(ListTasksOptions {
            include_archived: true,
            ..Default::default()
        })?;
        return Ok(entries.into_iter().map(|entry| entry.metadata.id).collect());
    }

    if options.include_all_running {
        let mut list_options = ListTasksOptions::default();
        list_options.states.push(TaskState::Running);
        let entries = service.list_tasks(list_options)?;
        return Ok(entries.into_iter().map(|entry| entry.metadata.id).collect());
    }

    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for task_id in &options.task_ids {
        if seen.insert(task_id.clone()) {
            targets.push(task_id.clone());
        }
    }
    Ok(targets)
}

fn collect_statuses(
    service: &TaskService,
    task_ids: &[String],
    wait_mode: WaitMode,
) -> Result<Vec<TaskStatusSnapshot>> {
    const POLL_INTERVAL_MS: u64 = 300;

    loop {
        let mut records = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            records.push(service.get_status(task_id)?);
        }

        if wait_mode.is_satisfied(&records) {
            return Ok(records);
        }

        sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

fn render_human(records: &[TaskStatusSnapshot], time_format: TimeFormat) {
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            println!();
        }
        render_human_record(record, time_format);
    }
}

fn render_human_record(record: &TaskStatusSnapshot, time_format: TimeFormat) {
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

fn render_json(records: &[TaskStatusSnapshot]) -> Result<()> {
    if records.len() == 1 {
        let payload = status_to_json(&records[0]);
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        let payload: Vec<_> = records.iter().map(status_to_json).collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    Ok(())
}

fn status_to_json(record: &TaskStatusSnapshot) -> serde_json::Value {
    json!({
        "id": record.metadata.id.clone(),
        "title": record.metadata.title.clone(),
        "state": record.metadata.state.clone(),
        "created_at": record.metadata.created_at.clone(),
        "updated_at": record.metadata.updated_at.clone(),
        "last_prompt": record.metadata.last_prompt.clone(),
        "last_result": record.metadata.last_result.clone(),
        "working_dir": record.metadata.working_dir.clone(),
        "pid": record.pid,
    })
}

impl WaitMode {
    fn is_satisfied(self, records: &[TaskStatusSnapshot]) -> bool {
        match self {
            WaitMode::None => true,
            WaitMode::All => records.iter().all(|record| is_terminal(record)),
            WaitMode::Any => records.iter().any(|record| is_terminal(record)),
        }
    }
}

fn is_terminal(record: &TaskStatusSnapshot) -> bool {
    matches!(
        record.metadata.state,
        TaskState::Stopped | TaskState::Archived | TaskState::Died
    )
}
