# Codex Protocol Specification

## Purpose of this document
This document describes the internal message-exchange protocol used by the Codex CLI/TUI and the server (`codex-core`). It contains complete definitions of the JSON structures and flows required to implement a client that connects to Codex, sends commands, and receives events. The spec covers both the `Submission`/`Event` queues and the JSON-RPC interface consumed by the MCP layer.

## Communication channels and serialization
- Communication between the client and the agent is bidirectional and asynchronous.
  - **Submission Queue (SQ):** the client publishes requests as `Submission` objects.
  - **Event Queue (EQ):** the agent (Codex) publishes responses and notifications as `Event` objects.
- All structures are serialized to JSON using `serde`. Enums use discriminator fields (`type`, or `method`/`mode`) whose values are in `snake_case` or `camelCase`, as dictated by the attributes in the code.
- Some types emit TypeScript definitions (`ts-rs`), therefore field and variant names must remain stable.

### Identifiers and correlation
- Every `Submission` has an `id: String` that is unique within the session. The value is echoed by corresponding events via `Event.id`.
- `Event.id` allows the client to pair the sequence of messages with a specific request. Additional background events can also reuse the most recent `id` or provide their own server-assigned identifiers.
- MCP uses `RequestId` (integer or string) per JSON-RPC 2.0.

### Serialization conventions
- `Duration` (for example in `ExecCommandEndEvent.duration`) serializes as `{ "secs": u64, "nanos": u32 }`, the default `serde` representation for `std::time::Duration`.
- `PathBuf` serializes as a string (absolute path or one relative to the server).
- `Option<T>` fields are omitted from JSON unless the value is `Some(None)`—in that case `null` is emitted (e.g. `OverrideTurnContext.effort: null` clears the setting).
- Byte buffers (`Vec<u8>`) in streaming payloads (e.g. `ExecCommandOutputDeltaEvent.chunk`) are Base64 encoded (`serde_with::base64`).

## Shared base types and enums
### Reasoning and verbosity (`config_types.rs`)
| Enum                | JSON value   | Meaning                                  |
|---------------------|--------------|------------------------------------------|
| `ReasoningEffort`   | `minimal`    | Minimal number of reasoning steps        |
|                     | `low`        | Low intensity                            |
|                     | `medium`     | Default (the enum's `default` value)     |
|                     | `high`       | Highest reasoning effort                 |
| `ReasoningSummary`  | `auto`       | Automatic choice                         |
|                     | `concise`    | Concise summary                          |
|                     | `detailed`   | Detailed summary                         |
|                     | `none`       | No reasoning summary                     |
| `Verbosity`         | `low`        | Shortest responses                       |
|                     | `medium`     | Default length                           |
|                     | `high`       | Most verbose responses                   |
| `SandboxMode`       | `read-only`  | Read-only access                           |
|                     | `workspace-write` | Global read, write limited to the workspace |
|                     | `danger-full-access` | Full access to disk and network         |

### Approval policy (`AskForApproval`)
`serde(rename_all = "kebab-case")`; textual values:
- `untrusted` (UnlessTrusted) – only safe read commands are auto-approved.
- `on-failure` – all commands auto-approve inside the sandbox; failures require explicit approval outside the sandbox.
- `on-request` – leaves the decision of requesting approval to the model (default).
- `never` – never ask the user; errors are returned to the model.

### Sandbox policy (`SandboxPolicy`)
Serialized with a `mode` field:
```json
{"mode": "danger-full-access"}
```
```json
{"mode": "read-only"}
```
```json
{
  "mode": "workspace-write",
  "writable_roots": ["/absolute/path"],
  "network_access": false,
  "exclude_tmpdir_env_var": false,
  "exclude_slash_tmp": false
}
```
- `writable_roots` extends the list of writable directories (in addition to `cwd`, `/tmp`, `$TMPDIR`).
- `network_access = true` permits network traffic inside the sandbox.
- `exclude_tmpdir_env_var` / `exclude_slash_tmp` let you remove the default temporary directories.
- `WritableRoot { root, read_only_subpaths }` controls write access (e.g. `.git`).

### Input items (`InputItem`)
`#[serde(tag = "type", rename_all = "snake_case")]`:
- `{"type":"text", "text":"..."}`
- `{"type":"image", "image_url":"data:image/png;base64,..."}`
- `{"type":"local_image", "path":"/absolute/path.png"}` – the server converts the file to the `image` variant.

## Submission Queue
### Submission format
```json
{
  "id": "unique-turn-id",
  "op": { ... }  // one of the Op operations
}
```
The `op` field contains exactly one of the operations below. The `type` values are in `snake_case`.

### `Op` operations
#### `interrupt`
No additional fields. Signals interruption of the current turn; expect `EventMsg::TurnAborted`.

#### `user_input`
```json
{
  "type": "user_input",
  "items": [InputItem, ...]
}
```
Sends a single user message without altering the turn context.

#### `user_turn`
A full conversation turn with execution context:
```json
{
  "type": "user_turn",
  "items": [InputItem, ...],
  "cwd": "/Users/alice/project",
  "approval_policy": "on-request",
  "sandbox_policy": {"mode": "workspace-write", "network_access": false},
  "model": "o4-mini",
  "effort": "medium",           // optional, when the model supports reasoning
  "summary": "auto"              // required, ReasoningSummary enum
}
```

#### `override_turn_context`
Updates the default context for upcoming turns:
```json
{
  "type": "override_turn_context",
  "cwd": "/new/path",                    // optional
  "approval_policy": "never",            // optional
  "sandbox_policy": { ... },
  "model": "o4-mini",
  "effort": null,                         // null clears the reasoning effort
  "summary": "concise"                    // optional
}
```

#### `exec_approval`
```json
{
  "type": "exec_approval",
  "id": "exec-call-id",
  "decision": "approved" | "approved_for_session" | "denied" | "abort"
}
```
`ReviewDecision` captures the user’s response to an `ExecApprovalRequest`.

#### `patch_approval`
Analogous to the above, but for `ApplyPatchApprovalRequest`.

#### `add_to_history`
```json
{"type":"add_to_history", "text":"Note"}
```

#### `get_history_entry_request`
```json
{
  "type": "get_history_entry_request",
  "offset": 0,
  "log_id": 123456
}
```
Requests a single global history entry.

#### `get_path`
Returns the path to the rollout file for the current session (`EventMsg::ConversationPath`).

#### `list_mcp_tools`
Requests the list of MCP tools available in the session (`EventMsg::McpListToolsResponse`).

#### `list_custom_prompts`
Requests the custom prompt catalog (`EventMsg::ListCustomPromptsResponse`).

#### `compact`
Asks the model to summarize the conversation context.

#### `review`
```json
{
  "type": "review",
  "review_request": {
    "prompt": "Review the changes...",
    "user_facing_hint": "Focus on performance"
  }
}
```
Starts a subordinate code-review session.

#### `shutdown`
Signals that the Codex instance should exit. Response: `EventMsg::ShutdownComplete`.

## Event Queue
### Event format
```json
{
  "id": "submission-id",
  "msg": {
    "type": "agent_message",
    ... payload ...
  }
}
```

### `EventMsg` variants
The list below contains every variant (`type` value → payload):

**Error and status handling**
- `error` → `{ "message": String }`
- `task_started` → `{ "model_context_window": u64? }`
- `task_complete` → `{ "last_agent_message": String? }`
- `token_count` → `{ "info": TokenUsageInfo? }`
- `turn_aborted` → `{ "reason": "interrupted" | "replaced" | "review_ended" }`
- `shutdown_complete` → empty object
- `stream_error` → `{ "message": String }`
- `background_event` → `{ "message": String }`

**Text messages and reasoning**
- `agent_message` → `{ "message": String }`
- `agent_message_delta` → `{ "delta": String }`
- `user_message` → `{ "message": String, "kind"?: "plain"|"user_instructions"|"environment_context", "images"?: [String] }`
- `agent_reasoning` → `{ "text": String }`
- `agent_reasoning_delta` → `{ "delta": String }`
- `agent_reasoning_raw_content` → `{ "text": String }`
- `agent_reasoning_raw_content_delta` → `{ "delta": String }`
- `agent_reasoning_section_break` → `{}` (marker that starts a new reasoning segment)
- `plan_update` → see [Plan tool](#plan-tool-updateplanargs)

**Configuration and history**
- `session_configured` → `{ "session_id": ConversationId, "model": String, "reasoning_effort"?: ReasoningEffort, "history_log_id": u64, "history_entry_count": usize, "initial_messages"?: [EventMsg], "rollout_path": PathBuf }`
- `conversation_path` → `{ "conversation_id": ConversationId, "path": PathBuf }`
- `get_history_entry_response` → `{ "offset": usize, "log_id": u64, "entry"?: HistoryEntry }`

**MCP / tool events**
- `mcp_tool_call_begin` → `{ "call_id": String, "invocation": McpInvocation }`
- `mcp_tool_call_end` → `{ "call_id": String, "invocation": McpInvocation, "duration": Duration, "result": CallToolResult | String }`
- `mcp_list_tools_response` → `{ "tools": { fullyQualifiedName: Tool } }`
- `list_custom_prompts_response` → `{ "custom_prompts": [CustomPrompt] }`

**Web search**
- `web_search_begin` → `{ "call_id": String }`
- `web_search_end` → `{ "call_id": String, "query": String }`

**Local commands / shell**
- `exec_command_begin` → `{ "call_id": String, "command": [String], "cwd": PathBuf, "parsed_cmd": [ParsedCommand] }`
- `exec_command_output_delta` → `{ "call_id": String, "stream": "stdout"|"stderr", "chunk": Base64 }`
- `exec_command_end` → `{ "call_id": String, "stdout": String, "stderr": String, "aggregated_output": String, "exit_code": i32, "duration": Duration, "formatted_output": String }`
- `exec_approval_request` → `{ "call_id": String, "command": [String], "cwd": PathBuf, "reason"?: String }`
- `patch_apply_begin` → `{ "call_id": String, "auto_approved": bool, "changes": { path: FileChange } }`
- `patch_apply_end` → `{ "call_id": String, "stdout": String, "stderr": String, "success": bool }`
- `apply_patch_approval_request` → `{ "call_id": String, "changes": { path: FileChange }, "reason"?: String, "grant_root"?: PathBuf }`
- `turn_diff` → `{ "unified_diff": String }`

**Review mode**
- `entered_review_mode` → `ReviewRequest`
- `exited_review_mode` → `{ "review_output"?: ReviewOutputEvent }`

## Structures associated with events
### TokenUsage and TokenUsageInfo
```json
{
  "total_token_usage": {
    "input_tokens": 123,
    "cached_input_tokens": 45,
    "output_tokens": 67,
    "reasoning_output_tokens": 10,
    "total_tokens": 245
  },
  "last_token_usage": { ... },
  "model_context_window": 128000
}
```
- `TokenUsage.blended_total()` = `non_cached_input + output_tokens`.
- The baseline constant `BASELINE_TOKENS = 12000` is used to estimate remaining context.

### ParsedCommand
`type` values: `read`, `list_files`, `search`, `unknown`. Payloads:
- `read { cmd: String, name: String }`
- `list_files { cmd: String, path?: String }`
- `search { cmd: String, query?: String, path?: String }`
- `unknown { cmd: String }`

### McpInvocation and CallToolResult
```json
{
  "server": "todo",
  "tool": "plan/update",
  "arguments": { ... } | null
}
```
`CallToolResult` comes from `mcp_types` and represents the MCP outcome (success or error).

### File changes (`FileChange`)
```json
{"type": "add", "content": "..."}
{"type": "delete", "content": "..."}
{"type": "update", "unified_diff": "@@ ...", "move_path": "/new/path.rs"}
```

### Review
- `ReviewRequest { "prompt": String, "user_facing_hint": String }`
- `ReviewOutputEvent` contains:
  - `findings: [ReviewFinding]`
  - `overall_correctness: String`
  - `overall_explanation: String`
  - `overall_confidence_score: f32`
- `ReviewFinding { title, body, confidence_score, priority, code_location }`
- `ReviewCodeLocation { absolute_file_path: PathBuf, line_range: { start: u32, end: u32 } }`

### Plan tool (`UpdatePlanArgs`)
- `UpdatePlanArgs { explanation?: String, plan: [PlanItemArg] }`
- `PlanItemArg { step: String, status: "pending" | "in_progress" | "completed" }`

## Model response structures (`models.rs`)
### `ResponseInputItem`
- `message { role: String, content: [ContentItem] }`
- `function_call_output { call_id: String, output: FunctionCallOutputPayload }`
- `mcp_tool_call_output { call_id: String, result: Result<CallToolResult,String> }`
- `custom_tool_call_output { call_id: String, output: String }`

### `ResponseItem`
- `message { id?: String, role: String, content: [ContentItem] }`
- `reasoning { id: String, summary: [ReasoningItemReasoningSummary], content?: [ReasoningItemContent], encrypted_content?: String }`
- `local_shell_call { id?: String, call_id?: String, status: "completed"|"in_progress"|"incomplete", action: LocalShellAction }`
- `function_call { id?: String, name: String, arguments: String, call_id: String }`
- `function_call_output { call_id: String, output: FunctionCallOutputPayload }`
- `custom_tool_call { id?: String, status?: String, call_id: String, name: String, input: String }`
- `custom_tool_call_output { call_id: String, output: String }`
- `web_search_call { id?: String, status?: String, action: WebSearchAction }`
- `other` – unrecognized data.

`ContentItem` kinds: `input_text`, `input_image`, `output_text`. `ReasoningItemContent`: `reasoning_text`, `text`.

`FunctionCallOutputPayload` serializes as a plain string (`content`). The `success` flag is only used locally.

### Local shell
- `LocalShellAction::Exec { command: [String], timeout_ms?: u64 (alias: timeout), working_directory?: String, env?: {k:v}, user?: String, with_escalated_permissions?: bool, justification?: String }`
- `LocalShellStatus`: `completed`, `in_progress`, `incomplete`.

### Web search
- `WebSearchAction::Search { query: String }`

### Converting `InputItem` → `ResponseInputItem`
A `Vec<InputItem>` (e.g. inside `ResponseInputItem::from`) is turned into a message with `role = user` and the appropriate `ContentItem` entries. Local files are converted into data URLs (MIME type determined via `mime_guess`).

## History and rollouts
- `InitialHistory`: `new`, `resumed { conversation_id, history: [RolloutItem], rollout_path }`, `forked([RolloutItem])`.
- `ResumedHistory { conversation_id, history: [RolloutItem], rollout_path }`.
- `RolloutItem` variants:
  - `session_meta(SessionMetaLine)`
  - `response_item(ResponseItem)`
  - `compacted(CompactedItem)`
  - `turn_context(TurnContextItem)`
  - `event_msg(EventMsg)`
- `RolloutLine { timestamp: String, item: RolloutItem }`.
- `SessionMeta { id: ConversationId, timestamp: String (RFC3339), cwd: PathBuf, originator: String, cli_version: String, instructions?: String }`.
- `SessionMetaLine { meta: SessionMeta, git?: GitInfo }`.
- `GitInfo { commit_hash?, branch?, repository_url? }`.
- `TurnContextItem { cwd: PathBuf, approval_policy: AskForApproval, sandbox_policy: SandboxPolicy, model: String, effort?: ReasoningEffort, summary: ReasoningSummary }`.
- `CompactedItem { message: String }` may be treated like `ResponseItem::Message` with `role = assistant`.

`HistoryEntry { conversation_id: String, ts: u64, text: String }` is returned in `GetHistoryEntryResponse`.

## Plan tool (`UpdatePlanArgs`)
See the [Plan tool](#plan-tool-updateplanargs) section. It ensures consistent plan updates via `EventMsg::PlanUpdate`.

## MCP protocol (JSON-RPC)
### Transport
- MCP uses JSON-RPC 2.0 with `camelCase` method names.
- Each request carries an `id` (`RequestId` – integer or string) and `params` when required.
- Responses (`*Response`) reuse the same `id` and follow the standard JSON-RPC format (`result` / `error`).
- The server may send:
  - **ServerRequest** (`applyPatchApproval`, `execCommandApproval`) – require a client response.
  - **ServerNotification** (`authStatusChange`, `loginChatGptComplete`) – one-way notifications without `id`.

### Client requests (`ClientRequest`)
| Method | Params | Result |
|--------|--------|--------|
| `newConversation` | `NewConversationParams { model?, profile?, cwd?, approval_policy?, sandbox?, config?, base_instructions?, include_plan_tool?, include_apply_patch_tool? }` | `NewConversationResponse { conversation_id, model, reasoning_effort?, rollout_path }`
| `listConversations` | `ListConversationsParams { page_size?, cursor? }` | `ListConversationsResponse { items: [ConversationSummary], next_cursor? }`
| `resumeConversation` | `ResumeConversationParams { path, overrides? }` | `ResumeConversationResponse { conversation_id, model, initial_messages? }`
| `archiveConversation` | `ArchiveConversationParams { conversation_id, rollout_path }` | `ArchiveConversationResponse {}`
| `sendUserMessage` | `SendUserMessageParams { conversation_id, items }` | `SendUserMessageResponse {}`
| `sendUserTurn` | `SendUserTurnParams { conversation_id, items, cwd, approval_policy, sandbox_policy, model, effort?, summary }` | `SendUserTurnResponse {}`
| `interruptConversation` | `InterruptConversationParams { conversation_id }` | `InterruptConversationResponse { abort_reason }`
| `addConversationListener` | `AddConversationListenerParams { conversation_id }` | `AddConversationSubscriptionResponse { subscription_id }`
| `removeConversationListener` | `RemoveConversationListenerParams { subscription_id }` | `RemoveConversationListenerResponse {}`
| `gitDiffToRemote` | `GitDiffToRemoteParams { cwd }` | `GitDiffToRemoteResponse { sha, diff }`
| `loginApiKey` | `LoginApiKeyParams { api_key }` | `LoginApiKeyResponse {}`
| `loginChatGpt` | none | `LoginChatGptResponse { login_id, auth_url }`
| `cancelLoginChatGpt` | `CancelLoginChatGptParams { login_id }` | `CancelLoginChatGptResponse {}`
| `logoutChatGpt` | none | `LogoutChatGptResponse {}`
| `getAuthStatus` | `GetAuthStatusParams { include_token?, refresh_token? }` | `GetAuthStatusResponse { auth_method?, auth_token?, requires_openai_auth? }`
| `getUserSavedConfig` | none | `GetUserSavedConfigResponse { config: UserSavedConfig }`
| `setDefaultModel` | `SetDefaultModelParams { model?, reasoning_effort? }` | `SetDefaultModelResponse {}`
| `getUserAgent` | none | `GetUserAgentResponse { user_agent }`
| `userInfo` | none | `UserInfoResponse { alleged_user_email? }`
| `execOneOffCommand` | `ExecOneOffCommandParams { command, timeout_ms?, cwd?, sandbox_policy? }` | `ExecArbitraryCommandResponse { exit_code, stdout, stderr }`

### ServerRequest (requests from the server)
- `applyPatchApproval` → `ApplyPatchApprovalParams { conversation_id, call_id, file_changes, reason?, grant_root? }` – respond with `ApplyPatchApprovalResponse { decision }`.
- `execCommandApproval` → `ExecCommandApprovalParams { conversation_id, call_id, command, cwd, reason? }` – respond with `ExecCommandApprovalResponse { decision }`.

### ServerNotification
- `authStatusChange` → `{ "auth_method"?: "apiKey" | "chatGpt" }`
- `loginChatGptComplete` → `{ "login_id": Uuid, "success": bool, "error"?: String }`

### User configuration (`UserSavedConfig`)
- Optional fields describe preferred settings (policy, sandbox, model, profiles, tools).
- `approval_policy?`, `sandbox_mode?`, `sandbox_settings?`, `model?`, `model_reasoning_effort?`, `model_reasoning_summary?`, `model_verbosity?`, `tools?`, `profile?`, `profiles`.
- `profiles` is a map from profile name to `Profile { model?, model_provider?, approval_policy?, model_reasoning_*?, model_verbosity?, chatgpt_base_url? }`.
- `tools`: `web_search?`, `view_image?` (bool).
- `sandbox_settings`: same structure as `SandboxPolicy::WorkspaceWrite`.

### Types shared between MCP and the protocol
- MCP reuses the same definitions for `InputItem`, `AskForApproval`, `SandboxPolicy`, `ReviewDecision`, `FileChange` as the SQ/EQ queues.
- All `camelCase` fields in MCP map to `snake_case` fields in the internal protocol.

### Example MCP sequence
1. The client sends `newConversation` with the chosen model.
2. After receiving `conversation_id`, it sends `addConversationListener` to subscribe to events.
3. It sends `sendUserTurn` (using the same `conversation_id`).
4. The server streams events (`EventMsg` encoded as JSON Lines or SSE—transport depends on the client layer) plus any `ServerRequest` approvals.
5. The client replies to `applyPatchApproval` / `execCommandApproval` according to the user’s decision.
6. When finished, it may call `interruptConversation` or `archiveConversation`.

## Turn lifecycle (SQ/EQ)
1. **Submission:** the client publishes a `Submission` (`user_turn` or `user_input`).
2. **Echo input:** the agent emits `EventMsg::UserMessage` (echoing what went to the model) and `SessionConfigured` at startup. Exception: in review mode the synthetic starting message (generated from `Op::Review`) is not emitted as `UserMessage`; only `EnteredReviewMode` appears.
3. **Stream reasoning:** `AgentMessageDelta`, `AgentReasoningDelta`, `TokenCount`, etc. arrive asynchronously.
4. **Tools:** when the model launches commands or MCP tools, expect `ExecCommand*`, `McpToolCall*`, `WebSearch*`, `PlanUpdate`.
5. **Approvals:** if approval is required, the agent emits `ExecApprovalRequest` / `ApplyPatchApprovalRequest`. The client must answer with `exec_approval` / `patch_approval`.
6. **Completion:** `TaskComplete` plus optional `TurnDiff`, `TokenCount`. Errors produce `Error` / `TurnAborted`.
7. **History:** the client can call `get_path` or `get_history_entry_request` to read logs.

## Implementation notes
- Prefer UUIDs (or monotonically increasing strings) for `Submission.id` to avoid collisions.
- Expect multiple events per `id`—`TaskStarted`, `AgentMessageDelta`, `TaskComplete` all share the same identifier.
- When decoding `Result<CallToolResult, String>` (e.g. `McpToolCallEndEvent.result`), remember the JSON payload may be either a tool object or an error string.
- Decode `ExecCommandOutputDeltaEvent.chunk` from Base64 before displaying it.
- `SessionConfigured.initial_messages` may contain a list of full `EventMsg`; treat them the same as regular events.
- `OverrideTurnContext.effort` has three states: absent (leave unchanged), `null` (clear), or one of the `ReasoningEffort` values (set).
- With MCP integration, maintain subscriptions (`removeConversationListener`) and handle multiple conversations concurrently.
- The rolling log (`rollout_path`) is a JSONL file—each line is a `RolloutLine`.
- When the sandbox mode is `workspace_write`, the server automatically adds the current `cwd`, `/tmp` (if it exists), and `$TMPDIR` (unless disabled by flags) to the writable list.

## Summary
This document defines all data structures, enumerations, and flows required to build a client that communicates with Codex through the `Submission`/`Event` queues or the MCP interface. An implementation must strictly follow the field names, `type` values, and JSON formats presented here to remain compatible with existing Codex components.
