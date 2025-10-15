use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use codex_protocol::num_format::format_with_separators;
use serde_json::Value;

use crate::cli::LogArgs;
use crate::services::tasks::{FollowMetadata, TaskService};
use crate::task::TaskState;

pub fn handle_log(args: LogArgs) -> Result<()> {
    let service = TaskService::with_default_store(false)?;
    let wait_for_log = args.follow || args.forever;
    let descriptor = service.prepare_log_descriptor(&args.task_id, wait_for_log)?;
    let log_path = descriptor.path.clone();
    let file = File::open(&log_path).with_context(|| {
        format!(
            "failed to open log for task {} at {}",
            args.task_id,
            log_path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    if args.json {
        print_initial_log(&mut reader, args.lines)?;

        let should_follow = args.follow || args.forever;
        if should_follow {
            let context = FollowContext {
                task_id: args.task_id,
                metadata: descriptor.metadata.clone(),
                forever: args.forever,
            };
            follow_log(&mut reader, context)?;
        }
    } else {
        let mut human_state = HumanRenderState::new();
        print_initial_log_human(&mut reader, args.lines, &mut human_state)?;

        let should_follow = args.follow || args.forever;
        if should_follow {
            let context = FollowContext {
                task_id: args.task_id,
                metadata: descriptor.metadata,
                forever: args.forever,
            };
            follow_log_human(&mut reader, context, &mut human_state)?;
        }
    }
    Ok(())
}

fn print_initial_log(reader: &mut BufReader<File>, limit: Option<usize>) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();

    match limit {
        Some(limit) => {
            let mut lines = VecDeque::new();
            loop {
                buffer.clear();
                let bytes = read_line_retry(reader, &mut buffer)
                    .context("failed to read from log while preparing output")?;
                if bytes == 0 {
                    break;
                }

                if limit == 0 {
                    continue;
                }

                if lines.len() == limit {
                    lines.pop_front();
                }
                lines.push_back(buffer.clone());
            }

            for line in lines {
                stdout
                    .write_all(line.as_bytes())
                    .context("failed to write log output")?;
            }
        }
        None => loop {
            buffer.clear();
            let bytes = read_line_retry(reader, &mut buffer)
                .context("failed to read from log while preparing output")?;
            if bytes == 0 {
                break;
            }
            stdout
                .write_all(buffer.as_bytes())
                .context("failed to write log output")?;
        },
    }

    stdout
        .flush()
        .context("failed to flush log output to stdout")?;
    Ok(())
}

fn follow_log(reader: &mut BufReader<File>, context: FollowContext) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();
    let mut idle_pending = false;

    loop {
        buffer.clear();
        match read_line_retry(reader, &mut buffer) {
            Ok(0) => {
                stdout
                    .flush()
                    .context("failed to flush log output to stdout")?;

                if context.forever {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }

                match context.current_state() {
                    Ok(Some(TaskState::Running)) => {
                        idle_pending = false;
                    }
                    Ok(Some(TaskState::Stopped)) => {
                        if idle_pending {
                            eprintln!("Task {} is STOPPED; stopping log follow.", context.task_id);
                            break;
                        }
                        idle_pending = true;
                    }
                    Ok(Some(state @ (TaskState::Died | TaskState::Archived))) => {
                        eprintln!(
                            "Task {} is {}; stopping log follow.",
                            context.task_id,
                            state.as_str()
                        );
                        break;
                    }
                    Ok(None) => {
                        eprintln!(
                            "Task {} state unavailable; stopping log follow.",
                            context.task_id
                        );
                        break;
                    }
                    Err(err) => {
                        eprintln!("Failed to read state for task {}: {err:#}", context.task_id);
                        break;
                    }
                }

                thread::sleep(Duration::from_millis(250));
            }
            Ok(_) => {
                idle_pending = false;
                stdout
                    .write_all(buffer.as_bytes())
                    .context("failed to write log output")?;
                stdout
                    .flush()
                    .context("failed to flush log output to stdout")?;
            }
            Err(err) => {
                return Err(err).context("failed to read from log while following");
            }
        }
    }

    Ok(())
}

fn print_initial_log_human(
    reader: &mut BufReader<File>,
    limit: Option<usize>,
    state: &mut HumanRenderState,
) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();

    match limit {
        Some(limit) => {
            let mut lines = VecDeque::new();
            loop {
                buffer.clear();
                let bytes = read_line_retry(reader, &mut buffer)
                    .context("failed to read from log while preparing output")?;
                if bytes == 0 {
                    break;
                }

                if limit == 0 {
                    continue;
                }

                if lines.len() == limit {
                    lines.pop_front();
                }
                lines.push_back(buffer.clone());
            }

            for line in lines {
                write_humanized_line(&line, state, &mut stdout)?;
            }
        }
        None => loop {
            buffer.clear();
            let bytes = read_line_retry(reader, &mut buffer)
                .context("failed to read from log while preparing output")?;
            if bytes == 0 {
                break;
            }
            write_humanized_line(&buffer, state, &mut stdout)?;
        },
    }

    stdout
        .flush()
        .context("failed to flush log output to stdout")?;
    Ok(())
}

fn follow_log_human(
    reader: &mut BufReader<File>,
    context: FollowContext,
    state: &mut HumanRenderState,
) -> Result<()> {
    let mut buffer = String::new();
    let mut stdout = io::stdout();
    let mut idle_pending = false;

    loop {
        buffer.clear();
        match read_line_retry(reader, &mut buffer) {
            Ok(0) => {
                stdout
                    .flush()
                    .context("failed to flush log output to stdout")?;

                if context.forever {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }

                match context.current_state() {
                    Ok(Some(TaskState::Running)) => {
                        idle_pending = false;
                    }
                    Ok(Some(TaskState::Stopped)) => {
                        if idle_pending {
                            eprintln!("Task {} is STOPPED; stopping log follow.", context.task_id);
                            break;
                        }
                        idle_pending = true;
                    }
                    Ok(Some(state @ (TaskState::Died | TaskState::Archived))) => {
                        eprintln!(
                            "Task {} is {}; stopping log follow.",
                            context.task_id,
                            state.as_str()
                        );
                        break;
                    }
                    Ok(None) => {
                        eprintln!(
                            "Task {} state unavailable; stopping log follow.",
                            context.task_id
                        );
                        break;
                    }
                    Err(err) => {
                        eprintln!("Failed to read state for task {}: {err:#}", context.task_id);
                        break;
                    }
                }

                thread::sleep(Duration::from_millis(250));
            }
            Ok(_) => {
                idle_pending = false;
                write_humanized_line(&buffer, state, &mut stdout)?;
            }
            Err(err) => {
                return Err(err).context("failed to read from log while following");
            }
        }
    }

    Ok(())
}

fn write_humanized_line(
    raw_line: &str,
    state: &mut HumanRenderState,
    stdout: &mut io::Stdout,
) -> Result<()> {
    let trimmed = raw_line.trim_end();
    if trimmed.is_empty() {
        return Ok(());
    }

    let value: Value = match serde_json::from_str(trimmed) {
        Ok(val) => val,
        Err(err) => {
            eprintln!("failed to parse log line as JSON: {err}");
            return Ok(());
        }
    };

    let lines = state.render_event(&value);
    for line in lines {
        stdout
            .write_all(line.as_bytes())
            .context("failed to write log output")?;
        stdout
            .write_all(b"\n")
            .context("failed to write log output")?;
    }

    Ok(())
}

struct HumanRenderState {
    last_agent_message: Option<String>,
}

impl HumanRenderState {
    fn new() -> Self {
        Self {
            last_agent_message: None,
        }
    }

    fn render_event(&mut self, value: &Value) -> Vec<String> {
        let Some(event_type) = value.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };

        match event_type {
            "thread.started" => Vec::new(),
            "user_message" => render_user_message(value),
            "item.completed" => self.render_item_completed(value),
            "turn.completed" => self.render_turn_completed(value),
            "turn.failed" => value
                .get("error")
                .and_then(|err| err.get("message"))
                .and_then(Value::as_str)
                .map(|msg| vec![format!("ERROR: {msg}")])
                .unwrap_or_else(|| vec!["ERROR: turn failed".to_string()]),
            "stderr" => value
                .get("message")
                .and_then(Value::as_str)
                .map(|msg| vec![format!("[stderr] {msg}")])
                .unwrap_or_default(),
            "error" => value
                .get("message")
                .and_then(Value::as_str)
                .map(|msg| vec![format!("ERROR: {msg}")])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn render_item_completed(&mut self, value: &Value) -> Vec<String> {
        let item = match value.get("item") {
            Some(item) => item,
            None => return Vec::new(),
        };

        let item_type = match item.get("type").and_then(Value::as_str) {
            Some(kind) => kind,
            None => return Vec::new(),
        };

        match item_type {
            "agent_message" => item
                .get("text")
                .and_then(Value::as_str)
                .map(|text| {
                    let trimmed = text.trim_end().to_string();
                    self.last_agent_message = Some(trimmed.clone());
                    vec!["codex".to_string(), trimmed]
                })
                .unwrap_or_default(),
            "reasoning" => item
                .get("text")
                .and_then(Value::as_str)
                .map(|text| {
                    let mut lines = vec!["thinking".to_string()];
                    lines.push(text.trim_end().to_string());
                    lines.push(String::new());
                    lines
                })
                .unwrap_or_default(),
            "command_execution" => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let exit_code = item
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");

                let mut lines = vec!["exec".to_string()];
                lines.push(command.to_string());
                let status_line = if exit_code == 0 {
                    format!("succeeded (exit {exit_code})")
                } else {
                    format!("exited {exit_code} ({status})")
                };
                lines.push(status_line);
                if let Some(output) = item
                    .get("aggregated_output")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    for line in output.lines() {
                        lines.push(line.to_string());
                    }
                }
                lines
            }
            "file_change" => render_file_change_item(item),
            "web_search" => item
                .get("query")
                .and_then(Value::as_str)
                .map(|query| vec![format!("🌐 Searched: {query}")])
                .unwrap_or_default(),
            "mcp_tool_call" => {
                let server = item
                    .get("server")
                    .and_then(Value::as_str)
                    .unwrap_or("server");
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                vec![format!("tool {server}.{tool} {status}")]
            }
            _ => Vec::new(),
        }
    }

    fn render_turn_completed(&mut self, value: &Value) -> Vec<String> {
        let usage = match value.get("usage") {
            Some(u) => u,
            None => return Vec::new(),
        };

        let total = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("total_token_usage")
                    .and_then(|v| v.get("blended_total"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or_else(|| {
                let input = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let cached = usage
                    .get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let output = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                input.saturating_sub(cached) + output
            });

        vec!["tokens used".to_string(), format_with_separators(total)]
    }
}

fn render_user_message(value: &Value) -> Vec<String> {
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    lines.push("user".to_string());
    for line in message.lines() {
        lines.push(line.to_string());
    }
    lines.push(String::new());
    lines
}

fn render_file_change_item(item: &Value) -> Vec<String> {
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let mut lines = vec![format!("file update ({status})")];

    if let Some(changes) = item.get("changes").and_then(Value::as_array) {
        for change in changes {
            let path = change
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let kind = change
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("update");
            let marker = match kind {
                "add" => 'A',
                "delete" => 'D',
                "update" => 'M',
                _ => '?',
            };
            lines.push(format!("{marker} {path}"));
        }
    }

    lines
}

struct FollowContext {
    task_id: String,
    metadata: FollowMetadata,
    forever: bool,
}

impl FollowContext {
    fn current_state(&self) -> Result<Option<TaskState>> {
        match &self.metadata {
            FollowMetadata::Active { store } => match store.load_metadata(self.task_id.clone()) {
                Ok(metadata) => Ok(Some(metadata.state)),
                Err(err) => {
                    if err
                        .downcast_ref::<io::Error>()
                        .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound)
                    {
                        Ok(None)
                    } else {
                        Err(err)
                    }
                }
            },
            FollowMetadata::Archived { state } => Ok(Some(state.clone())),
            FollowMetadata::Missing => Ok(None),
        }
    }
}

fn read_line_retry<R: BufRead>(reader: &mut R, buffer: &mut String) -> io::Result<usize> {
    loop {
        match reader.read_line(buffer) {
            Ok(bytes) => return Ok(bytes),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
}
