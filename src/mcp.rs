use std::io::{self, BufRead, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use mcp_types::{
    Implementation, InitializeRequestParams, InitializeResult, JSONRPC_VERSION, JSONRPCError,
    JSONRPCErrorError, JSONRPCMessage, JSONRPCNotification, JSONRPCRequest, JSONRPCResponse,
    MCP_SCHEMA_VERSION, RequestId, ServerCapabilities,
};
use serde_json::{Value, json};

use crate::cli::McpArgs;

/// Entry point for the `codex-tasks mcp` subcommand.
pub fn run(args: McpArgs) -> Result<()> {
    let config = McpConfig::from_args(args);
    let store_root = config
        .store_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<default>".to_string());
    let config_path = config
        .config_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<none>".to_string());
    eprintln!(
        "[mcp] configuration stub -> store_root={}, config={}, allow_unsafe={}",
        store_root, config_path, config.allow_unsafe
    );
    run_server(config)
}

#[derive(Debug)]
struct McpConfig {
    store_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    allow_unsafe: bool,
}

impl McpConfig {
    fn from_args(args: McpArgs) -> Self {
        Self {
            store_root: args.store_root,
            config_path: args.config,
            allow_unsafe: args.allow_unsafe,
        }
    }
}

fn run_server(_config: McpConfig) -> Result<()> {
    let stdin = io::stdin();
    let mut writer = BufWriter::new(io::stdout());

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
                if handle_request(request, &mut writer)? {
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

fn handle_request<W: Write>(request: JSONRPCRequest, writer: &mut W) -> Result<bool> {
    let JSONRPCRequest {
        id, method, params, ..
    } = request;
    match method.as_str() {
        "initialize" => {
            let params_value = params.unwrap_or(Value::Null);
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
                    tools: None,
                },
                instructions: Some("Codex Tasks MCP server skeleton ready.".to_owned()),
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
            respond_success(writer, id, json!({"status": "ok"}))?;
            Ok(false)
        }
        "shutdown" => {
            respond_success(writer, id, json!({"status": "shutting_down"}))?;
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

fn respond_success<W: Write>(writer: &mut W, id: RequestId, result: Value) -> Result<()> {
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
