# Implementation Plan – `codex-tasks`

## Legend
- **Task** – coding or integration work unit.
- **Depends on** – tasks that must be completed first.

## Tasks

1. **Scaffold CLI project structure**
   - Create module layout for `codex-tasks` (CLI entrypoint, worker module, task model).
   - Set up `clap` command definitions for all subcommands (including stub handlers).

2. **File-based metadata layer**
   - Implement helper utilities for `~/.codex/tasks` layout (ensure directories, path helpers).
   - Persist Codex `thread_id` values, title storage, and timestamps.
   - Provide read/write APIs for `pid`, `log`, `result`, and metadata files.
   - **Depends on:** Task 1

3. **Worker process launcher**
   - Implement fork/exec logic that spawns the task worker binary/process.
   - Worker receives an initial prompt (and optional existing thread ID/title when resuming).
   - Ensure worker inherits the existing transcript renderer and storage helpers.
   - **Depends on:** Tasks 1 & 2

4. **Worker invocation flow**
   - Launch `codex exec --json` for the initial prompt and wait for `thread.started` to provide the canonical identifier.
   - Buffer early events until the identifier is known, then persist metadata/log files and flush buffered output.
   - For resumed prompts, call `codex exec --json resume <thread_id>` and append streamed events to the existing log.
   - Promote the `--output-last-message` artifact into `task.result` and refresh metadata after each invocation.
   - **Depends on:** Task 3

5. **Implement `start` command**
   - Glue CLI + metadata + worker spawn.
   - Wait for the worker handshake, then print the Codex `thread_id`.
   - **Depends on:** Tasks 2–4

6. **Implement `send` command**
   - Validate task state, ensure no worker is currently running, and spawn a resume worker with the stored `thread_id`.
   - Handle errors (ARCHIVED/DIED tasks, running worker PIDs, missing metadata).
   - **Depends on:** Task 2

7. **Implement `status` command**
   - Inspect `.pid`, `.log`, `.result`, timestamps.
   - Detect states (`RUNNING`, `STOPPED`, `ARCHIVED`, `DIED`).
   - Render human-readable summary (JSON/table).
   - **Depends on:** Task 2

8. **Implement `stop` command**
   - Signal the worker process, wait for exit with a timeout, and clean up PID metadata.
   - Update metadata/timestamps, handle already-stopped tasks.
   - **Depends on:** Tasks 2 & 4

9. **Implement `ls` command**
   - Enumerate active tasks under `tasks/`; include `tasks/archive/**` when `-a/--all` is provided.
   - Aggregate metadata for each task; support `--state` filters and multiple values.
   - **Depends on:** Task 2

10. **Implement `log` command**
    - Tail/read `<task_id>.log` with options `-n`, `-f` (follow).
    - Respect archived tasks (log location under `archive/…`).
    - **Depends on:** Task 2

11. **Implement `archive` command**
    - Move task files to dated `archive/` subdirectory.
    - Prevent archiving RUNNING tasks without `stop`.
    - **Depends on:** Tasks 2, 8

12. **State detection & robustness pass**
    - Ensure tasks detect `DIED` (missing PID but leftover log).
    - Confirm restart safety (no .pid ⇒ status updates, `send`/`stop` behaviour).
    - **Depends on:** Tasks 5–11

13. **Integration tests / end-to-end scripts**
    - Write CLI-driven tests for each command.
    - Cover sequences: start→send→status→stop, archive flows, DIED detection, log tailing.
    - **Depends on:** Tasks 5–12

14. **Packaging & documentation updates**
    - Update README/usage instructions.
    - Provide examples for piping prompts, viewing logs, archiving.
    - **Depends on:** Tasks 5–13
