# Product Requirements – MCP Server Mode

## Summary
Introduce a minimal `codex-tasks mcp` subcommand that runs codex-tasks itself as an MCP (Model Context Protocol) server. When started, the command speaks MCP over STDIN/STDOUT and exposes the existing codex-tasks workflow (start, send, status, log, stop, list, archive) as MCP tools/functions so any MCP-compatible client can automate task management without shelling out to the CLI.

## Background
- MCP is becoming the preferred way for clients (IDEs, agents, automation hubs) to invoke tool capabilities over a standard protocol.
- Today those clients must spawn `codex-tasks` as a CLI process for every operation, incurring startup cost and complicating streaming interactions.
- Running codex-tasks in “server mode” provides a persistent bridge: the client maintains an MCP session and calls tools that delegate to the existing Rust code paths already used by the CLI.
- The scope explicitly excludes managing external binaries; the codex-tasks executable **is** the MCP server.

## Goals & Success Metrics
1. Provide a single command `codex-tasks mcp` that turns the binary into a long-lived MCP server speaking over stdio.
2. Surface a curated set of MCP tools that map 1:1 to codex task operations (e.g., `task.start`, `task.send`, `task.status`, `task.log`, `task.stop`, `task.list`, `task.archive`).
3. Support streaming responses where the underlying CLI already supports streaming (notably task logs).
4. Keep configuration lightweight—primarily store root overrides, optional feature flags, and authentication hooks for future growth.

Success criteria:
- An MCP client can start the server once and invoke multiple codex task actions without re-spawning the binary.
- Task metadata and filesystem layout remain unchanged relative to the existing CLI.
- The MCP contract is documented and covered by automated tests that exercise at least one tool invocation per category.

## Non-Goals
- No additional CLI subcommands (e.g., `mcp status`, `mcp log`). The MCP server is only reachable via the MCP protocol once launched.
- No attempt to serve HTTP/WebSocket endpoints—transport is stdio-based MCP.
- No redesign of codex storage; the server uses the same TaskStore as the CLI.

## User Stories
1. *As an IDE integration developer*, I can launch `codex-tasks mcp` in the background and call `task.start` from my MCP client to kick off a Codex worker.
2. *As an automation engineer*, I can stream logs by invoking the `task.log` MCP tool and receiving incremental outputs without re-running the CLI repeatedly.
3. *As a maintainer*, I can audit the exposed tool list and ensure permission gating aligns with existing CLI behavior.

## Functional Requirements

### Command Surface
```
codex-tasks mcp [--store-root <PATH>] [--config <PATH>] [--allow-unsafe]
```
- Without arguments the server reads/writes `~/.codex/tasks` as usual.
- `--store-root` lets operators point to an alternate task store.
- `--config` references an optional TOML file containing server settings (tool enablement, auth token, logging verbosity).
- `--allow-unsafe` (placeholder) can gate operations such as executing `task.stop -a` if we decide to restrict them by default.

The process binds stdio to MCP. It should log a brief startup banner to stderr (for operator clarity) but never emit non-MCP data on stdout.

### Tool Catalog
Expose the following MCP tools (tentative names, subject to alignment with protocol conventions):

| Tool ID       | Description                                    | Input payload                                         | Output payload                                     |
|---------------|------------------------------------------------|-------------------------------------------------------|----------------------------------------------------|
| `task.start`  | Starts a new Codex task                         | Initial prompt, optional title/config/working_dir     | Task metadata incl. thread_id                      |
| `task.send`   | Sends a follow-up prompt to a task              | task_id, prompt                                       | Updated metadata (last prompt timestamp, etc.)     |
| `task.status` | Returns current status for a task               | task_id                                               | Task state, timestamps, last result preview        |
| `task.log`    | Streams log lines for a task                    | task_id, tail options (lines, follow)                 | Streaming events delivering log chunks             |
| `task.stop`   | Stops a running task                            | task_id (or flag all?)                                | Outcome summary                                    |
| `task.list`   | Lists tasks with optional filters               | state filters, include archived flag                  | Array of task summaries                            |
| `task.archive`| Archives a task                                 | task_id, optional force flag                          | Archive location + confirmation                    |

Each tool should reuse the existing Rust modules (e.g., `commands::start`, `commands::status`) rather than re-implementing business logic. Inputs/outputs should be serialized as JSON structures conforming to MCP expectations.

### Protocol Behavior
- The server implements the MCP Server role, handling `initialize`, `shutdown`, and `ping` according to the spec.
- Tool invocations execute asynchronously; long-running operations (start, log follow) must send interim events so clients remain responsive.
- `task.log` in follow mode should emit streaming events until the client cancels the tool invocation.
- Error responses should embed codex-tasks error codes/messages while using MCP-standard error envelopes.

### Configuration & Security
- Optional config keys:
  ```toml
  [server]
  store_root = "/var/lib/codex/tasks"
  allow_stop_all = false
  log_level = "info"
  auth_token = "s3cr3t" # if provided, require clients to present matching token via MCP metadata
  [tools]
  enable_archive = false
  ```
- Authentication (if enabled) can use a shared secret conveyed via MCP `initialize` options; requests lacking the token receive an `unauthorized` error.
- Respect existing environment variables (e.g., `CODEX_TASKS_EXIT_AFTER_START`) for testability.

### Logging & Observability
- Write server diagnostics to stderr (timestamps, tool invocation summaries) without polluting MCP stdout.
- Expose a `--verbose` flag to increase logging detail.
- Consider an optional `tool.invoke` event stream for clients that want metadata about operations (subject to MCP capabilities).

## Non-Functional Requirements
- Compatible with macOS 13+ and modern Linux.
- Low idle CPU usage; the server mostly blocks on MCP input.
- Graceful shutdown on SIGINT/SIGTERM (propagate `shutdown` to clients and exit with code 0).
- Reuse existing async runtime (Tokio) where feasible to handle concurrent tool requests.

## Dependencies
- MCP protocol crates (likely `mcp` from OpenAI OSS) for request/response handling.
- Existing codex-tasks modules for task management.

## Rollout Plan
1. Finalize PRD and secure stakeholder approval.
2. Draft implementation plan covering MCP transport, tool adapters, and cancellation.
3. Build minimal skeleton that answers MCP `initialize` and `ping` and exposes a stub tool.
4. Incrementally wire tool implementations, mirroring CLI command behavior.
5. Add integration tests using a lightweight MCP client harness.
6. Document usage in README (brief section) and produce release notes.

## Risks & Mitigations
- **Protocol drift**: keep MCP dependencies pinned and add compatibility tests.
- **Long-running operations block server**: ensure each tool runs on async tasks; enforce cancellation support.
- **Security exposure**: default to local/authorized usage; document risks if exposing to remote clients.
- **Client expectations**: clearly document which tool operations stream vs. return once.

## Open Questions
1. Should we expose every CLI option (e.g., repo cloning flags) through MCP, or start with a core subset?
2. Do we need rate limiting or concurrency caps to prevent runaway clients?
3. How should we return large log histories—single response with pagination options, or require follow mode?
4. Is authentication mandatory for first release, or acceptable as a future enhancement?

## Acceptance Criteria
- `codex-tasks mcp` runs an MCP server that successfully handles at least `task.start`, `task.status`, and `task.log` via an automated test client.
- Documentation enumerates available tools and their payload schemas.
- Existing CLI functionality remains unchanged; running the command without MCP clients has no side effects beyond stdout/stderr noise.
