# Product Requirements – `codex exec` Migration

## Summary
OpenAI has deprecated the legacy `codex proto` interface in favor of `codex exec`. The existing `codex-tasks` workers depend on `codex proto` for long-lived bidirectional communication, so the CLI will stop functioning as soon as the legacy binary disappears. This document defines the product requirements for migrating `codex-tasks` to run fully on top of `codex exec`, preserving today’s task lifecycle while embracing the new execution model and adopting Codex `thread_id` values as task identifiers that originate from the service.

## Background
- Workers currently spawn a single `codex proto` subprocess, pipe prompts through a FIFO (`<task_id>.pipe`), and stream JSON events back into per-task logs.
- `codex exec` is strictly non-interactive. Each invocation accepts an optional prompt, produces JSONL events, and exits. Resuming a conversation requires calling `codex exec resume <session_id>`.
- The new CLI ships additional options (`--json`, `--cd`, `-c/--config`, sandbox controls) that must map cleanly from `codex-tasks`.
- Maintaining feature parity (start, send, status, log, stop, archive) is mandatory for existing power users.

## Goals & Success Metrics
1. `codex-tasks start` launches a task that uses `codex exec` under the hood, requires an initial prompt, and returns the Codex `thread_id` (canonical task identifier) to the caller as soon as the first event is received.
2. `codex-tasks send` reuses the stored session ID to issue follow-up prompts via `codex exec --json resume`.
3. Logs, transcripts, and metadata remain usable (no format-breaking changes for downstream tooling).
4. Full CLI test suite passes, and QA confirms parity across critical workflows (start → send → status → stop, start without prompt, stop during in-flight prompt, archive workflow).
5. Documentation and change log clearly communicate the migration, including any new limitations.

## Non-Goals
- Containerization or deployment automation (tracked separately in issue #40).
- Introducing a real-time streaming transport; `codex exec`'s synchronous model is assumed.
- Redesigning the filesystem layout beyond fields required for session tracking.

## Stakeholders
- **CLI users** need uninterrupted background task automation.
- **Maintainers** require a maintainable integration that aligns with official Codex tooling.
- **Support/DevRel** must communicate changes and guide teams transitioning from `codex proto`.

## User Stories
1. *As a power user*, I can start a background task and retrieve its logs later, without noticing that `codex exec` replaced `codex proto`.
2. *As a maintainer*, I can inspect task metadata (`status`, `ls`) and see the Codex `thread_id` being reused for every follow-up prompt.
3. *As an operator*, I can gracefully stop a task even if a `codex exec` invocation is currently running.
4. *As a developer*, I can configure working directories and config overrides the same way I did with `codex proto`.

## Functional Requirements
### Session Tracking
- Use the Codex `thread_id` (conversation/session ID) as the canonical task identifier. The worker must not create on-disk state until the first `thread.started` event provides this identifier.
- Buffer startup events in-memory until the `thread_id` is known, then materialize the task directory structure (log, metadata, pipes, result file) under that identifier and replay buffered data.
- On sends, ensure the stored `thread_id` exists; surface actionable errors when resuming fails.

### Worker Lifecycle
- Replace the long-lived `codex proto` child process with on-demand `codex exec` invocations.
- Require an initial prompt to bootstrap the first invocation; subsequent prompts flow through the FIFO.
- Stream JSON events from each invocation in real time: capture `thread.started` immediately to persist the session ID, establish storage rooted at the new `thread_id`, forward buffered and subsequent events into the log, and detect completion without blocking for user-facing output.
- Launch `codex exec` with `--output-last-message <task_id>.result.tmp` so the final assistant reply is captured asynchronously; atomically promote the file to `<task_id>.result` when the process exits.
- Maintain graceful shutdown: if a `/quit` marker is received, wait only for the in-flight invocation to supply its session ID (if missing), then drain remaining events, skip queued prompts, mark the task `STOPPED`, and clean up files.

### CLI & UX
- Preserve existing CLI flags; translate them into `codex exec` options (`--json`, `--cd`, `-c`, sandbox controls).
- Update help text, README, docs, and changelog to reference `codex exec`.
- Ensure `status` and `ls` render the latest session ID and last result as before.

### State Model
- Remove the historical `IDLE` state. Tasks remain `RUNNING` while an invocation is active and transition directly to `STOPPED` (or `DIED` on failure) once the invocation finishes.
- Update status rendering and metadata helpers to reflect the simplified state machine.

### Logging & Storage
- Continue writing JSONL events to `<task_id>.log` (where `<task_id>` is the Codex `thread_id`). Normalize events so downstream consumers do not break (e.g., convert `codex exec` specific event types into the existing schema where possible).
- Use the `--output-last-message` artifact from each invocation to refresh `<task_id>.result` without waiting for the CLI to print the final assistant message.
- Ensure buffered startup events are persisted once storage is initialized so that no JSON output is lost prior to filesystem creation.

### Error Handling
- Handle retries for transient `codex exec` failures (e.g., non-zero exit code). Workers should mark the task `DIED` only after exhausting retry policy or encountering unrecoverable errors.
- Surface human-readable error details in logs and metadata.

## Non-Functional Requirements
- Maintain parity in startup latency (<1s overhead vs. current implementation).
- Ensure no orphan processes remain after stopping a task.
- Code must remain cross-platform (macOS/Linux); avoid shell-specific constructs that break portability.

## Proposed Approach
1. **Abstraction Layer:** Introduce a dedicated `exec_client` module that wraps `codex exec` invocations, handling command-line construction, JSONL parsing into existing `Event` types, and error classification.
2. **Worker Refactor:** Update `worker::runner` to:
   - Replace `spawn_codex_proto` with async helpers that call the new client.
   - Consume prompts from the FIFO, spawning an invocation per prompt and streaming stdout until the process exits.
   - Defer filesystem initialization until a `thread.started` event is observed; once available, persist the `thread_id`, create task artifacts, flush buffered events, and announce the ID on stdout.
   - Watch for process completion to rotate the `--output-last-message` file into the canonical result path.
3. **Metadata Changes:** Transition `TaskMetadata` and filesystem layout to key off the Codex `thread_id` without temporary identifiers, ensuring legacy tasks remain readable.
4. **Command Adjustments:** Ensure `start`, `send`, `stop`, `status`, and `log` continue to function using the new identifiers, and surface a clear error when `start` is invoked without a prompt. `stop` should interrupt pending prompts cleanly without leaving zombie invocations.
5. **Testing:** Create or update integration tests that mock `codex exec` output. Validate start→send→stop and error surfaces. Add negative tests for missing session ID on resume.
6. **Documentation & Release Notes:** Update `README.md`, `docs/prd.md`, and `CHANGELOG.md`. Provide migration guidance and troubleshooting tips.

## Timeline & Milestones
1. **Discovery (2–3 days):** Prototype `codex exec` JSON parsing, confirm compatibility with existing event models, document discovered differences.
2. **Implementation (1 week):** Build the new execution client, refactor worker lifecycle, add metadata updates.
3. **Quality (3–4 days):** Update/extend automated tests, perform manual E2E validation, document findings.
4. **Release Prep (1–2 days):** Finalize documentation, update changelog, prepare PR with detailed testing notes.

## Risks & Mitigations
- **Behavioral Differences:** Event schema mismatches could break logs. Mitigation: implement translation layer and add regression tests with captured real outputs.
- **Performance Regression:** Repeated process launches might slow interactions. Mitigation: measure typical prompt latency and cache heavy configuration (e.g., environment variables) to minimize overhead.
- **Error Surfacing:** `codex exec` may emit richer diagnostics on stderr. Ensure logs capture stderr output so operators can debug failed prompts.
- **Backward Compatibility:** Existing archived tasks lack a session ID. CLI should gracefully handle legacy metadata (e.g., hide resume option when absent).

## Open Questions
1. Does `codex exec` offer a streaming mode for stdin prompts that we can leverage to reduce process churn?
2. What is the recommended retry policy for network/transient errors (documented by OpenAI)?
3. How should we expose `thread_id` values to users (status output only, or new CLI flag for direct copy)?
4. Are there limits on session lifetime that impact long-running tasks when keyed by `thread_id`?

## Acceptance Criteria Recap
- Workers exclusively call `codex exec` and `codex exec resume` while maintaining existing CLI behavior and persist the Codex `thread_id` as the task identifier.
- All tests pass, and manual verification confirms parity for start/send/status/log/stop/archive flows.
- Documentation and release notes communicate the migration and any new considerations for operators.
