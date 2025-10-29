# Implementation Plan – MCP Server Mode

## Legend
- **Task** – discrete engineering deliverable.
- **Depends on** – prerequisite tasks that must be completed first.

## Workstreams & Tasks

1. **MCP Transport Skeleton**
   - Add the `codex-tasks mcp` CLI entry point (flag parsing, config loading stub).
   - Wire up stdio-based MCP server initialization using the chosen protocol crate.
   - Handle `initialize`, `ping`, and `shutdown` requests with placeholder responses.

2. **Configuration Plumbing**
   - Implement config loader (`--store-root`, `--config`, `--allow-unsafe`).
   - Validate inputs and propagate settings into the server context.
   - **Depends on:** Task 1

3. **Shared Task Services & Tool Layer**
   - Extract reusable service functions from existing CLI commands (start/send/status/log/stop/list/archive) so both CLI and MCP paths call the same logic.
   - Introduce a thin adapter layer that turns MCP tool invocations into calls to the shared services.
   - Ensure the CLI subcommands are refactored to consume the shared services without regressing current behaviour.
   - Normalize error handling: map service errors into MCP error envelopes with stable codes.
   - **Depends on:** Task 1

4. **Resource Subscriptions & Status Updates**
   - Advertise MCP resource capabilities and register per-task status resources (`task://{id}/status`).
   - Emit `notifications/resources/list_changed` when tasks are created or archived so controllers discover additions/removals quickly.
   - Support `resources/subscribe` / `resources/unsubscribe` for task status URIs and fire `notifications/resources/updated` when a subscribed task changes state.
   - Ensure `resources/read` returns task status snapshots for pull-style access while `resources/list` exposes all known status URIs.
   - Drop log follow/streaming mode for now; keep `task_log` as a snapshot-only tool.
   - **Depends on:** Task 3

5. **State & Store Integration**
   - Thread store configuration through adapters, ensuring commands use the specified root.
   - Audit commands for stdout/tty assumptions; add abstractions where necessary (e.g., injecting writers for MCP responses instead of printing).
   - Expose shared listing helpers that gather the same data the CLI prints while returning structured payloads for MCP clients.
   - **Depends on:** Task 3

6. **Diagnostics & Logging**
   - Route server diagnostics to stderr with structured logging (level filtering, optional `--verbose`).
   - Emit per-invocation summaries for observability without contaminating MCP stdout.
   - **Depends on:** Tasks 2 & 3

7. **Testing & Tooling**
   - Build a lightweight MCP client harness (Tokio-based) for integration tests.
   - Write tests covering:
     - Successful `initialize`/`shutdown` lifecycle.
     - `task_start` + `task_status` round trip.
     - Resource subscription notifications for task status and task list changes.
     - `resources/read` snapshots and `resources/unsubscribe` behaviour.
   - Add unit tests for config parsing and error mapping.
   - **Depends on:** Tasks 2–4

8. **Documentation & Release Prep**
   - Update README with a new MCP section (usage, available tools, sample JSON payloads).
   - Document configuration file schema in `docs/features/mcp-server/prd.md` or a dedicated reference.
   - Add changelog entry and release notes snippet.
   - **Depends on:** Tasks 2–7

## Timeline & Sequencing
1. Skeleton (Task 1)
2. Config/Auth (Task 2)
3. Tool Adapters (Task 3)
4. Store & Logging (Tasks 5 & 6) in parallel once adapters exist
5. Resource Subscriptions (Task 4)
6. Testing (Task 7)
7. Docs/Release (Task 8)

## Risks & Mitigations
- **Adapter output incompatibility** – some CLI commands write directly to stdout; mitigate by refactoring to accept trait-based writers before wiring adapters.
- **Resource notification volume** – frequent status changes could trigger notification storms; add debounce or batching if needed.
- **Protocol mismatch** – stay aligned with MCP spec; add compatibility tests with the official client library.

## Open Questions
1. Do we expose advanced CLI flags (repo cloning, JSON output) immediately or phase them in?
2. Should `task_stop` support `--all` through MCP, and is it gated behind `--allow-unsafe`?
3. What payload shape should resource reads use (full snapshot vs. deltas) to keep notifications lightweight?
4. Do we need metrics (counts, durations) emitted somewhere for ops visibility?
