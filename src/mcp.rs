use std::env;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use mcp_types::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, InitializeRequestParams,
    InitializeResult, JSONRPC_VERSION, JSONRPCError, JSONRPCErrorError, JSONRPCMessage,
    JSONRPCNotification, JSONRPCRequest, JSONRPCResponse, ListToolsResult, MCP_SCHEMA_VERSION,
    RequestId, ServerCapabilities, ServerCapabilitiesTools, TextContent, Tool, ToolAnnotations,
    ToolInputSchema,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value as JsonValue, json};
use toml::Value as TomlValue;

use crate::cli::McpArgs;
use crate::services::tasks::{
    ArchiveAllSummary, ArchiveTaskOutcome, FollowMetadata, ListTasksOptions, LogDescriptor,
    SendPromptParams, StartTaskParams, StopOutcome, StopTaskReport, TaskListEntry, TaskService,
    TaskStatusSnapshot,
};
use crate::storage::TaskStore;
use crate::task::{TaskMetadata, TaskState};

const DEFAULT_LOG_TAIL: usize = 200;

/// Entry point for the `codex-tasks mcp` subcommand.
pub fn run(args: McpArgs) -> Result<()> {
    let config = McpConfig::from_args(args)?;
    let store_root = format!("{}", config.store_root().display());
    let config_path = config
        .config_path
        .as_ref()
        .map(|path| format!("{}", path.display()))
        .unwrap_or_else(|| "<none>".to_string());
    eprintln!(
        "[mcp] configuration -> store_root={}, config={}, allow_unsafe={}",
        store_root, config_path, config.allow_unsafe
    );
    run_server(config)
}

struct McpConfig {
    store: TaskStore,
    config_path: Option<PathBuf>,
    config_document: Option<TomlValue>,
    allow_unsafe: bool,
}

impl McpConfig {
    fn from_args(args: McpArgs) -> Result<Self> {
        let store = resolve_store_root(args.store_root)?;
        let (config_path, config_document) = resolve_config(args.config)?;
        Ok(Self {
            store,
            config_path,
            config_document,
            allow_unsafe: args.allow_unsafe,
        })
    }

    fn task_service(&self) -> TaskService {
        TaskService::new(self.store.clone(), self.allow_unsafe)
    }

    fn store_root(&self) -> &Path {
        self.store.root()
    }
}

fn run_server(config: McpConfig) -> Result<()> {
    let stdin = io::stdin();
    let mut writer = BufWriter::new(io::stdout());

    if let Some(doc) = config.config_document.as_ref() {
        let top_level = doc.as_table().map(|table| table.len()).unwrap_or_default();
        eprintln!(
            "[mcp] loaded config document with {top_level} top-level item{}",
            if top_level == 1 { "" } else { "s" }
        );
    }

    for line_result in stdin.lock().lines() {
        let line = line_result.context("failed to read MCP input from stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let message: JSONRPCMessage = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(err) => {
                eprintln!("[mcp] ignoring malformed message: {err}");
                continue;
            }
        };

        match message {
            JSONRPCMessage::Request(request) => {
                if handle_request(request, &mut writer, &config)? {
                    break;
                }
            }
            JSONRPCMessage::Notification(notification) => {
                eprintln!(
                    "[mcp] ignoring unsupported client notification: {}",
                    notification.method
                );
            }
            JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {
                eprintln!("[mcp] ignoring unexpected client response/error");
            }
        }
    }

    Ok(())
}

fn handle_request<W: Write>(
    request: JSONRPCRequest,
    writer: &mut W,
    config: &McpConfig,
) -> Result<bool> {
    let JSONRPCRequest {
        id, method, params, ..
    } = request;
    match method.as_str() {
        "initialize" => {
            let params_value = params.unwrap_or(JsonValue::Null);
            let params: InitializeRequestParams = match serde_json::from_value(params_value) {
                Ok(value) => value,
                Err(err) => {
                    respond_error(
                        writer,
                        id,
                        -32602,
                        format!("invalid initialize params: {err}"),
                    )?;
                    return Ok(false);
                }
            };

            eprintln!(
                "[mcp] initialize from {} {} (protocol {})",
                params.client_info.name, params.client_info.version, params.protocol_version
            );

            let result = InitializeResult {
                capabilities: ServerCapabilities {
                    completions: None,
                    experimental: None,
                    logging: None,
                    prompts: None,
                    resources: None,
                    tools: Some(ServerCapabilitiesTools {
                        list_changed: Some(false),
                    }),
                },
                instructions: Some(format!(
                    "Codex Tasks MCP server ready. store-root={}, allow-unsafe={}",
                    config.store_root().display(),
                    config.allow_unsafe
                )),
                protocol_version: MCP_SCHEMA_VERSION.to_owned(),
                server_info: Implementation {
                    name: "codex-tasks".to_owned(),
                    title: Some("Codex Tasks MCP Server".to_owned()),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    user_agent: Some(format!("codex-tasks/{}", env!("CARGO_PKG_VERSION"))),
                },
            };

            respond_success(writer, id, serde_json::to_value(result)?)?;
            send_initialized(writer)?;
            Ok(false)
        }
        "ping" => {
            respond_success(
                writer,
                id,
                json!({
                    "status": "ok",
                    "storeRoot": config.store_root().display().to_string(),
                    "allowUnsafe": config.allow_unsafe
                }),
            )?;
            Ok(false)
        }
        "tools/list" => {
            let result = ListToolsResult {
                tools: build_tools(),
                next_cursor: None,
            };
            respond_success(writer, id, serde_json::to_value(result)?)?;
            Ok(false)
        }
        "tools/call" => {
            let params_json = params.unwrap_or(JsonValue::Null);
            let params: CallToolRequestParams = match serde_json::from_value(params_json) {
                Ok(value) => value,
                Err(err) => {
                    respond_error(writer, id, -32602, format!("invalid call params: {err}"))?;
                    return Ok(false);
                }
            };
            let result = handle_tool_call(config, params);
            respond_success(writer, id, serde_json::to_value(result)?)?;
            Ok(false)
        }
        "shutdown" => {
            respond_success(
                writer,
                id,
                json!({
                    "status": "shutting_down"
                }),
            )?;
            Ok(true)
        }
        other => {
            respond_error(
                writer,
                id,
                -32601,
                format!("method '{other}' is not implemented"),
            )?;
            Ok(false)
        }
    }
}

fn respond_success<W: Write>(writer: &mut W, id: RequestId, result: JsonValue) -> Result<()> {
    let response = JSONRPCResponse {
        id,
        jsonrpc: JSONRPC_VERSION.to_owned(),
        result,
    };
    write_message(writer, JSONRPCMessage::Response(response))
}

fn respond_error<W: Write>(
    writer: &mut W,
    id: RequestId,
    code: i64,
    message: String,
) -> Result<()> {
    let error = JSONRPCError {
        id,
        jsonrpc: JSONRPC_VERSION.to_owned(),
        error: JSONRPCErrorError {
            code,
            data: None,
            message,
        },
    };
    write_message(writer, JSONRPCMessage::Error(error))
}

fn send_initialized<W: Write>(writer: &mut W) -> Result<()> {
    let notification = JSONRPCNotification {
        jsonrpc: JSONRPC_VERSION.to_owned(),
        method: "notifications/initialized".to_string(),
        params: None,
    };
    write_message(writer, JSONRPCMessage::Notification(notification))
}

fn write_message<W: Write>(writer: &mut W, message: JSONRPCMessage) -> Result<()> {
    let encoded =
        serde_json::to_string(&message).context("failed to serialize MCP response message")?;
    writer
        .write_all(encoded.as_bytes())
        .context("failed to write MCP response")?;
    writer
        .write_all(b"\n")
        .context("failed to write MCP response terminator")?;
    writer.flush().context("failed to flush MCP response")
}

fn make_text_result(text: String, structured: Option<JsonValue>, is_error: bool) -> CallToolResult {
    CallToolResult {
        content: vec![text_block(text)],
        is_error: if is_error { Some(true) } else { None },
        structured_content: structured,
    }
}

fn success_text_result(text: impl Into<String>, structured: Option<JsonValue>) -> CallToolResult {
    make_text_result(text.into(), structured, false)
}

fn error_text_result(text: impl Into<String>) -> CallToolResult {
    make_text_result(text.into(), None, true)
}

fn text_block(text: String) -> ContentBlock {
    ContentBlock::TextContent(TextContent {
        annotations: None,
        text,
        r#type: "text".to_string(),
    })
}

fn parse_arguments<T: DeserializeOwned>(arguments: Option<JsonValue>) -> Result<T> {
    let value = arguments.unwrap_or_else(|| JsonValue::Object(Default::default()));
    serde_json::from_value(value).map_err(|err| anyhow!("invalid arguments: {err}"))
}

fn optional_path(value: Option<String>) -> Option<PathBuf> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    })
}

fn build_tools() -> Vec<Tool> {
    vec![
        make_tool(
            "task.start",
            "Start Task",
            "Start a new Codex task worker",
            json!({
                "prompt": {
                    "type": "string",
                    "description": "Prompt to send to the newly created worker"
                },
                "title": { "type": "string" },
                "configFile": { "type": "string" },
                "workingDir": { "type": "string" },
                "repoUrl": { "type": "string" },
                "repoRef": { "type": "string" }
            }),
            &["prompt"],
            false,
            false,
            true,
        ),
        make_tool(
            "task.send",
            "Send Prompt",
            "Send a follow-up prompt to an existing task",
            json!({
                "taskId": { "type": "string" },
                "prompt": { "type": "string" }
            }),
            &["taskId", "prompt"],
            false,
            false,
            true,
        ),
        make_tool(
            "task.status",
            "Get Status",
            "Retrieve the latest status for a task",
            json!({
                "taskId": { "type": "string" }
            }),
            &["taskId"],
            true,
            true,
            false,
        ),
        make_tool(
            "task.list",
            "List Tasks",
            "List tasks stored on disk",
            json!({
                "includeArchived": { "type": "boolean" },
                "states": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }),
            &[],
            true,
            true,
            false,
        ),
        make_tool(
            "task.log",
            "Read Log",
            "Read recent log output for a task",
            json!({
                "taskId": { "type": "string" },
                "tail": { "type": "integer" }
            }),
            &["taskId"],
            true,
            true,
            false,
        ),
        make_tool(
            "task.stop",
            "Stop Task",
            "Stop a running task or all running tasks",
            json!({
                "taskId": { "type": "string" },
                "all": { "type": "boolean" }
            }),
            &[],
            false,
            false,
            true,
        ),
        make_tool(
            "task.archive",
            "Archive Task",
            "Archive a stopped task or all completed tasks",
            json!({
                "taskId": { "type": "string" },
                "all": { "type": "boolean" }
            }),
            &[],
            false,
            false,
            true,
        ),
    ]
}

fn make_tool(
    name: &str,
    title: &str,
    description: &str,
    properties: JsonValue,
    required: &[&str],
    idempotent: bool,
    read_only: bool,
    destructive: bool,
) -> Tool {
    Tool {
        annotations: Some(ToolAnnotations {
            destructive_hint: Some(destructive),
            idempotent_hint: Some(idempotent),
            open_world_hint: None,
            read_only_hint: Some(read_only),
            title: Some(title.to_string()),
        }),
        description: Some(description.to_string()),
        input_schema: ToolInputSchema {
            properties: if properties
                .as_object()
                .map(|map| map.is_empty())
                .unwrap_or(true)
            {
                None
            } else {
                Some(properties)
            },
            required: if required.is_empty() {
                None
            } else {
                Some(required.iter().map(|value| value.to_string()).collect())
            },
            r#type: "object".to_string(),
        },
        name: name.to_string(),
        output_schema: None,
        title: Some(title.to_string()),
    }
}

fn handle_tool_call(config: &McpConfig, params: CallToolRequestParams) -> CallToolResult {
    let name = params.name;
    let arguments = params.arguments;
    match name.as_str() {
        "task.start" => call_task_start(config, arguments),
        "task.send" => call_task_send(config, arguments),
        "task.status" => call_task_status(config, arguments),
        "task.list" => call_task_list(config, arguments),
        "task.log" => call_task_log(config, arguments),
        "task.stop" => call_task_stop(config, arguments),
        "task.archive" => call_task_archive(config, arguments),
        other => error_text_result(format!("unknown tool '{other}'")),
    }
}

fn call_task_start(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<StartToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            let params = StartTaskParams {
                title: args.title,
                prompt: args.prompt,
                config_file: optional_path(args.config_file),
                working_dir: optional_path(args.working_dir),
                repo_url: args.repo_url,
                repo_ref: args.repo_ref,
            };
            match service.start_task(params) {
                Ok(result) => {
                    let structured = json!({
                        "threadId": result.thread_id,
                    });
                    success_text_result(
                        format!("Task started with thread id {}", result.thread_id),
                        Some(structured),
                    )
                }
                Err(err) => error_text_result(format!("Failed to start task: {err:#}")),
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_send(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<SendToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            let params = SendPromptParams {
                task_id: args.task_id,
                prompt: args.prompt,
            };
            match service.send_prompt(params) {
                Ok(()) => success_text_result("Prompt sent successfully", None),
                Err(err) => error_text_result(format!("Failed to send prompt: {err:#}")),
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_status(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<StatusToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            match service.get_status(&args.task_id) {
                Ok(status) => {
                    let structured = status_to_json(&status);
                    success_text_result(format_status_text(&status), Some(structured))
                }
                Err(err) => error_text_result(format!("Failed to load status: {err:#}")),
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_list(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<ListToolArgs>(arguments) {
        Ok(args) => {
            let states = match parse_task_states(&args.states) {
                Ok(states) => states,
                Err(err) => return error_text_result(err.to_string()),
            };
            let service = config.task_service();
            match service.list_tasks(ListTasksOptions {
                include_archived: args.include_archived,
                states,
            }) {
                Ok(entries) => {
                    let structured = list_to_json(&entries);
                    let text = format_list_text(&entries);
                    success_text_result(text, Some(structured))
                }
                Err(err) => error_text_result(format!("Failed to list tasks: {err:#}")),
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_log(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<LogToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            match service.prepare_log_descriptor(&args.task_id, false) {
                Ok(descriptor) => match read_log_tail(&descriptor, args.tail) {
                    Ok((lines, state)) => {
                        let structured = log_to_json(&descriptor, &lines, state.clone());
                        let text = format_log_text(&descriptor, &lines, state);
                        success_text_result(text, Some(structured))
                    }
                    Err(err) => error_text_result(format!("Failed to read log: {err:#}")),
                },
                Err(err) => error_text_result(format!("Failed to resolve log: {err:#}")),
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_stop(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<StopToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            if args.all.unwrap_or(false) {
                match service.stop_all_running() {
                    Ok(reports) => {
                        let structured = stop_reports_to_json(&reports);
                        if reports.is_empty() {
                            success_text_result("No running tasks to stop.", Some(structured))
                        } else {
                            let text = format_stop_reports(&reports);
                            success_text_result(text, Some(structured))
                        }
                    }
                    Err(err) => error_text_result(format!("Failed to stop tasks: {err:#}")),
                }
            } else {
                let task_id = match args.task_id {
                    Some(id) => id,
                    None => {
                        return error_text_result(
                            "`taskId` is required unless `all` is set to true",
                        );
                    }
                };
                match service.stop_task(&task_id) {
                    Ok(outcome) => {
                        let structured = json!({
                            "taskId": task_id,
                            "outcome": format_stop_outcome(&outcome),
                        });
                        success_text_result(
                            format_stop_outcome_text(&task_id, outcome),
                            Some(structured),
                        )
                    }
                    Err(err) => error_text_result(format!("Failed to stop task: {err:#}")),
                }
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn call_task_archive(config: &McpConfig, arguments: Option<JsonValue>) -> CallToolResult {
    match parse_arguments::<ArchiveToolArgs>(arguments) {
        Ok(args) => {
            let service = config.task_service();
            if args.all.unwrap_or(false) {
                match service.archive_all() {
                    Ok(summary) => {
                        let structured = archive_summary_to_json(&summary);
                        let text = archive_summary_to_text(&summary);
                        if summary.failures.is_empty() {
                            success_text_result(text, Some(structured))
                        } else {
                            make_text_result(text, Some(structured), true)
                        }
                    }
                    Err(err) => error_text_result(format!("Failed to archive tasks: {err:#}")),
                }
            } else {
                let task_id = match args.task_id {
                    Some(id) => id,
                    None => {
                        return error_text_result(
                            "`taskId` is required unless `all` is set to true",
                        );
                    }
                };
                match service.archive_task(&task_id) {
                    Ok(ArchiveTaskOutcome::Archived { id, destination }) => {
                        let destination_str = destination.display().to_string();
                        let structured = json!({
                            "taskId": id,
                            "destination": destination_str,
                        });
                        success_text_result(
                            format!("Task {} archived to {}.", task_id, destination_str),
                            Some(structured),
                        )
                    }
                    Ok(ArchiveTaskOutcome::AlreadyArchived { id }) => success_text_result(
                        format!("Task {} is already archived.", id),
                        Some(json!({ "taskId": id, "alreadyArchived": true })),
                    ),
                    Err(err) => error_text_result(format!("Failed to archive task: {err:#}")),
                }
            }
        }
        Err(err) => error_text_result(err.to_string()),
    }
}

fn status_to_json(status: &TaskStatusSnapshot) -> JsonValue {
    json!({
        "id": status.metadata.id,
        "title": status.metadata.title,
        "state": status.metadata.state.as_str(),
        "createdAt": status.metadata.created_at,
        "updatedAt": status.metadata.updated_at,
        "lastPrompt": status.metadata.last_prompt,
        "lastResult": status.metadata.last_result,
        "workingDir": status.metadata.working_dir,
        "pid": status.pid,
    })
}

fn format_status_text(status: &TaskStatusSnapshot) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Task ID: {}", status.metadata.id));
    if let Some(title) = &status.metadata.title {
        lines.push(format!("Title: {}", title));
    }
    lines.push(format!("State: {}", status.metadata.state));
    lines.push(format!(
        "Created At: {}",
        status.metadata.created_at.to_rfc3339()
    ));
    lines.push(format!(
        "Updated At: {}",
        status.metadata.updated_at.to_rfc3339()
    ));
    lines.push(format!(
        "Working Dir: {}",
        status.metadata.working_dir.as_deref().unwrap_or("<none>")
    ));
    if let Some(pid) = status.pid {
        lines.push(format!("PID: {}", pid));
    }
    lines.push(format!(
        "Last Prompt: {}",
        status
            .metadata
            .last_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("<none>")
    ));
    lines.push(format!(
        "Last Result: {}",
        status
            .metadata
            .last_result
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("<none>")
    ));
    lines.join("\n")
}

fn list_to_json(entries: &[TaskListEntry]) -> JsonValue {
    JsonValue::Array(
        entries
            .iter()
            .map(|entry| metadata_to_json(&entry.metadata))
            .collect(),
    )
}

fn metadata_to_json(metadata: &TaskMetadata) -> JsonValue {
    json!({
        "id": metadata.id,
        "title": metadata.title,
        "state": metadata.state.as_str(),
        "createdAt": metadata.created_at,
        "updatedAt": metadata.updated_at,
        "workingDir": metadata.working_dir,
    })
}

fn format_list_text(entries: &[TaskListEntry]) -> String {
    if entries.is_empty() {
        return "No tasks found.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("Found {} task(s):", entries.len()));
    for entry in entries {
        lines.push(format!(
            "- {} ({})",
            entry.metadata.id, entry.metadata.state
        ));
    }
    lines.join("\n")
}

fn parse_task_states(values: &[String]) -> Result<Vec<TaskState>> {
    let mut states = Vec::new();
    for value in values {
        let parsed = match value.to_uppercase().as_str() {
            "RUNNING" => TaskState::Running,
            "STOPPED" => TaskState::Stopped,
            "ARCHIVED" => TaskState::Archived,
            "DIED" => TaskState::Died,
            other => bail!("unknown task state '{other}'"),
        };
        states.push(parsed);
    }
    Ok(states)
}

fn format_stop_outcome(outcome: &StopOutcome) -> &'static str {
    match outcome {
        StopOutcome::AlreadyStopped => "already_stopped",
        StopOutcome::Stopped => "stopped",
    }
}

fn format_stop_outcome_text(task_id: &str, outcome: StopOutcome) -> String {
    match outcome {
        StopOutcome::AlreadyStopped => {
            format!("Task {} is not running; nothing to stop.", task_id)
        }
        StopOutcome::Stopped => format!("Task {} stopped.", task_id),
    }
}

fn stop_reports_to_json(reports: &[StopTaskReport]) -> JsonValue {
    let mut stopped = 0usize;
    let mut already = 0usize;
    let items: Vec<JsonValue> = reports
        .iter()
        .map(|report| {
            match report.outcome {
                StopOutcome::Stopped => stopped += 1,
                StopOutcome::AlreadyStopped => already += 1,
            }
            json!({
                "taskId": report.task_id,
                "outcome": format_stop_outcome(&report.outcome)
            })
        })
        .collect();

    json!({
        "reports": items,
        "summary": {
            "stopped": stopped,
            "alreadyStopped": already,
        }
    })
}

fn format_stop_reports(reports: &[StopTaskReport]) -> String {
    if reports.is_empty() {
        return "No running tasks to stop.".to_string();
    }

    let mut stopped = 0usize;
    let mut already = 0usize;
    let mut lines = Vec::new();
    for report in reports {
        lines.push(format_stop_outcome_text(&report.task_id, report.outcome));
        match report.outcome {
            StopOutcome::Stopped => stopped += 1,
            StopOutcome::AlreadyStopped => already += 1,
        }
    }
    lines.push(format!(
        "Stopped {stopped} running task(s); {already} already stopped.",
        stopped = stopped,
        already = already
    ));
    lines.join("\n")
}

fn archive_summary_to_json(summary: &ArchiveAllSummary) -> JsonValue {
    json!({
        "skipped": summary
            .skipped
            .iter()
            .map(|(id, state)| json!({ "taskId": id, "state": state.as_str() }))
            .collect::<Vec<_>>(),
        "archived": summary
            .archived
            .iter()
            .map(|(id, destination)| json!({
                "taskId": id,
                "destination": destination.display().to_string()
            }))
            .collect::<Vec<_>>(),
        "already": summary.already.iter().cloned().collect::<Vec<_>>(),
        "failures": summary
            .failures
            .iter()
            .map(|(id, err)| json!({
                "taskId": id,
                "error": err.to_string()
            }))
            .collect::<Vec<_>>(),
    })
}

fn archive_summary_to_text(summary: &ArchiveAllSummary) -> String {
    let mut lines = Vec::new();
    if summary.skipped.is_empty()
        && summary.archived.is_empty()
        && summary.already.is_empty()
        && summary.failures.is_empty()
    {
        lines.push("No STOPPED or DIED tasks were found to archive.".to_string());
        return lines.join("\n");
    }

    for (id, state) in &summary.skipped {
        lines.push(format!("Skipping task {} ({}).", id, state.as_str()));
    }
    for (id, destination) in &summary.archived {
        lines.push(format!(
            "Task {} archived to {}.",
            id,
            destination.display()
        ));
    }
    for id in &summary.already {
        lines.push(format!("Task {} is already archived.", id));
    }
    if !summary.failures.is_empty() {
        for (id, err) in &summary.failures {
            lines.push(format!("Failed to archive task {}: {err:#}", id));
        }
    } else if summary.archived.is_empty() && summary.already.is_empty() {
        lines.push("No STOPPED or DIED tasks were archived.".to_string());
    }
    lines.join("\n")
}

fn read_log_tail(
    descriptor: &LogDescriptor,
    tail: Option<usize>,
) -> Result<(Vec<String>, Option<TaskState>)> {
    let content = fs::read_to_string(&descriptor.path)
        .with_context(|| format!("failed to read log at {}", descriptor.path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let tail_count = tail.unwrap_or(DEFAULT_LOG_TAIL).min(lines.len());
    let start = lines.len().saturating_sub(tail_count);
    let selected: Vec<String> = lines[start..].iter().map(|line| line.to_string()).collect();

    let state = match &descriptor.metadata {
        FollowMetadata::Active { store } => store
            .load_metadata(descriptor.task_id.clone())
            .ok()
            .map(|metadata| metadata.state),
        FollowMetadata::Archived { state } => Some(state.clone()),
        FollowMetadata::Missing => None,
    };

    Ok((selected, state))
}

fn log_to_json(
    descriptor: &LogDescriptor,
    lines: &[String],
    state: Option<TaskState>,
) -> JsonValue {
    json!({
        "taskId": descriptor.task_id,
        "path": descriptor.path.display().to_string(),
        "state": state.map(|s| s.as_str().to_string()),
        "lines": lines,
    })
}

fn format_log_text(
    descriptor: &LogDescriptor,
    lines: &[String],
    state: Option<TaskState>,
) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "Task {} log at {}\n",
        descriptor.task_id,
        descriptor.path.display()
    ));
    if let Some(state) = state {
        output.push_str(&format!("State: {}\n", state));
    }
    if lines.is_empty() {
        output.push_str("<empty>");
    } else {
        output.push_str("---\n");
        output.push_str(&lines.join("\n"));
    }
    output
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartToolArgs {
    prompt: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    config_file: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default)]
    repo_ref: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendToolArgs {
    task_id: String,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusToolArgs {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListToolArgs {
    #[serde(default)]
    include_archived: bool,
    #[serde(default)]
    states: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogToolArgs {
    task_id: String,
    #[serde(default)]
    tail: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopToolArgs {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    all: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveToolArgs {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    all: Option<bool>,
}

fn resolve_store_root(candidate: Option<PathBuf>) -> Result<TaskStore> {
    match candidate {
        Some(path) => {
            let absolute = make_absolute(&path)?;
            if absolute.exists() {
                ensure!(
                    absolute.is_dir(),
                    "store root {} exists but is not a directory",
                    absolute.display()
                );
            } else {
                fs::create_dir_all(&absolute).with_context(|| {
                    format!(
                        "failed to create store root directory {}",
                        absolute.display()
                    )
                })?;
            }
            let canonical = absolute.canonicalize().with_context(|| {
                format!(
                    "failed to resolve canonical path for store root {}",
                    absolute.display()
                )
            })?;
            Ok(TaskStore::new(canonical))
        }
        None => TaskStore::default().context("failed to determine default store root"),
    }
}

fn resolve_config(candidate: Option<PathBuf>) -> Result<(Option<PathBuf>, Option<TomlValue>)> {
    let Some(path) = candidate else {
        return Ok((None, None));
    };

    let absolute = make_absolute(&path)?;
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve config file at {}", absolute.display()))?;
    ensure!(
        canonical.is_file(),
        "config path {} is not a file",
        canonical.display()
    );
    let file_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    ensure!(
        file_name == "config.toml",
        "config file must be named `config.toml` (got {file_name})"
    );

    let contents = fs::read_to_string(&canonical)
        .with_context(|| format!("failed to read config file {}", canonical.display()))?;
    let document: TomlValue = toml::from_str(&contents)
        .with_context(|| format!("failed to parse config.toml at {}", canonical.display()))?;

    Ok((Some(canonical), Some(document)))
}

fn make_absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let cwd = env::current_dir().context("failed to determine current working directory")?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_store_root_creates_directory() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let desired = temp.path().join("store");
        let store = resolve_store_root(Some(desired.clone()))?;
        assert!(desired.exists());
        assert_eq!(
            store.root(),
            &desired.canonicalize().context("canonicalize store")?
        );
        Ok(())
    }

    #[test]
    fn resolve_store_root_rejects_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file_path = temp.path().join("not_a_dir");
        fs::write(&file_path, "data")?;
        let err = resolve_store_root(Some(file_path)).expect_err("expected error");
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error message: {err:#}"
        );
        Ok(())
    }

    #[test]
    fn resolve_config_parses_toml() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config_path = temp.path().join("config.toml");
        fs::write(&config_path, "foo = \"bar\"")?;
        let (resolved, document) = resolve_config(Some(config_path.clone()))?;
        assert_eq!(
            resolved.expect("path"),
            config_path.canonicalize().context("canonicalize config")?
        );
        let doc = document.expect("document");
        assert_eq!(doc["foo"].as_str(), Some("bar"));
        Ok(())
    }

    #[test]
    fn resolve_config_rejects_wrong_filename() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config_path = temp.path().join("custom.toml");
        fs::write(&config_path, "foo = 1")?;
        let err = resolve_config(Some(config_path)).expect_err("expected error");
        assert!(
            err.to_string().contains("must be named `config.toml`"),
            "unexpected error: {err:#}"
        );
        Ok(())
    }
}
