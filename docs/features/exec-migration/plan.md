# Implementation Plan – `codex exec` Migration

## Objective
Deliver a `codex-tasks` release that replaces all `codex proto` integrations with `codex exec`, adopts Codex `thread_id` as the canonical task identifier, and preserves the end-user workflow (start → send → status → log → stop → archive).

## Workstreams & Tasks

### 1. Discovery & Compatibility Audit
- Capture reference event logs from `codex exec --json` and `codex exec resume` for common flows (start prompt, follow-up prompt, stop mid-stream, error cases).
- Confirm JSON event types align with `codex_core::protocol::Event`; document gaps that require translation.
- Identify CLI flag parity (`--config`, `--cd`, sandbox overrides, `--output-last-message`) and note required overrides for OSS/Windows scenarios.
- Inventory touch points in current code that assume a long-lived process or local `task_id`.

### 2. Exec Client Abstraction
- Add `exec_client` module encapsulating command construction, stdin piping, stdout/stderr streaming, and lifecycle hooks.
- Support configuration inputs: working directory, config overrides (`-c`), sandbox mode, images, include-plan-tool flag, and `--output-last-message` target.
- Expose async API returning a stream of parsed `Event` objects plus side-channel info (exit status, stderr, final message path).
- Implement error taxonomy (retryable vs. fatal) with structured context.

### 3. Worker Bootstrap & Identifier Handling
- Modify worker entrypoint to defer filesystem initialization until the first `thread.started` event arrives.
- Buffer initial events; once `thread_id` is known, create task directory tree using that value, materialize metadata/log files, flush buffered events, and print the `thread_id` on stdout.
- Update metadata schema to store worker PID, `thread_id`, and state transitions keyed by the new identifier; ensure legacy metadata remains readable (migration on read).

### 4. Prompt Processing Pipeline
- Replace FIFO writer logic to enqueue prompts; for each prompt spawn a new exec client invocation.
- Write prompt text into the process stdin once, then close stdin immediately; monitor event stream until completion.
- Consume `--output-last-message` temp file on exit and atomically move it to `<thread_id>.result`.
- Handle `/quit` by marking shutdown intent, waiting for the in-flight invocation to finish streaming, then skipping queued prompts and transitioning to `STOPPED`.

### 5. Metadata & State Machine Updates
- Simplify `TaskState` to omit `IDLE`; states become `RUNNING`, `STOPPED`, `DIED`, `ARCHIVED`.
- Audit callers (`status`, `ls`, `storage`) to ensure they operate on the new identifier and state set.
- Update persistence helpers to drop assumptions about FIFO creation before task ID availability.

### 6. CLI & UX Adjustments
- Modify `start`, `send`, `status`, `log`, `stop`, and `archive` command handlers to operate on `thread_id`.
- Ensure `start` prints the `thread_id` returned by the worker.
- Refresh help text and usage strings to reference `codex exec`, remove references to `codex proto`, and document the new identifier semantics.

### 7. Test Strategy
- Extend integration tests to mock `codex exec` responses: start→send→stop, error propagation, missing `thread.started`, resume with stale ID.
- Add unit tests for exec client error classification and buffered event replay logic.
- Regression tests for storage layout compatibility (pre-migration tasks can still be listed/archived).

### 8. Documentation & Change Management
- Update README, top-level PRD, changelog, and any tutorials referencing `codex proto`.
- Document new operational considerations (e.g., logs delayed until `thread_id` acquisition, reliance on `--output-last-message`).
- Coordinate release notes highlighting the identifier change and implications for scripts integrating with `codex-tasks`.

## Dependencies & Sequencing
1. Complete discovery (Workstream 1) before implementing exec client.
2. Exec client (Workstream 2) blocks worker refactor (Workstreams 3 & 4).
3. Metadata/state updates (Workstream 5) can proceed in parallel with CLI adjustments (Workstream 6) once identifier semantics are nailed down.
4. Testing and documentation (Workstreams 7 & 8) finalize after core refactor stabilizes.

## Rollout & Verification
- Run full `cargo test` with exec client fakes.
- Perform manual smoke test using real `codex exec` (start, send, stop, archive).
- Verify compatibility with archived tasks created under `codex proto` (read-only operations).
- Prepare migration advisory for users, including suggested cleanup of stale `~/.codex/tasks` entries if necessary.
