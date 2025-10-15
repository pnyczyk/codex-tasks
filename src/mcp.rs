use std::env;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use mcp_types::{
    Implementation, InitializeRequestParams, InitializeResult, JSONRPC_VERSION, JSONRPCError,
    JSONRPCErrorError, JSONRPCMessage, JSONRPCNotification, JSONRPCRequest, JSONRPCResponse,
    MCP_SCHEMA_VERSION, RequestId, ServerCapabilities,
};
use serde_json::{Value as JsonValue, json};
use toml::Value as TomlValue;

use crate::cli::McpArgs;
use crate::storage::TaskStore;

/// Entry point for the `codex-tasks mcp` subcommand.
pub fn run(args: McpArgs) -> Result<()> {
    let config = McpConfig::from_args(args)?;
    let store_root = format!("{}", config.store.root().display());
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
                    tools: None,
                },
                instructions: Some(format!(
                    "Codex Tasks MCP server ready. store-root={}, allow-unsafe={}",
                    config.store.root().display(),
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
                    "storeRoot": config.store.root(),
                    "allowUnsafe": config.allow_unsafe
                }),
            )?;
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
