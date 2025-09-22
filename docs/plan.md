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
   - Implement UUID generation, title storage, timestamps.
   - Provide read/write APIs for `pid`, `pipe`, `log`, `result` metadata.
   - **Depends on:** Task 1

3. **Worker process launcher**
   - Implement fork/exec logic that spawns the task worker binary/process.
   - Worker receives task ID, initial prompt, optional title.
   - Ensure worker inherits/rendering code from existing `codex-task` implementation.
   - **Depends on:** Tasks 1 & 2

4. **Worker main loop**
   - Reuse `codex-task` communication layer to talk to `codex proto`.
   - Manage `.pipe` lifecycle (open FIFO, handle multiple writers, detect `/quit`).
   - Stream events into `.log` using existing renderer, update `.result` on completion.
   - Handle shutdown (`/quit`, stop command, EOF) with timeout + kill.
   - **Depends on:** Task 3

5. **Implement `start` command**
   - Glue CLI + metadata + worker spawn.
   - Write initial files, start worker, return UUID.
   - **Depends on:** Tasks 2–4

6. **Implement `send` command**
   - Open `<task_id>.pipe`, write prompt line, validate task state.
   - Handle errors (missing pipe ⇒ STOPPED/ARCHIVED/DIED).
   - **Depends on:** Task 2

7. **Implement `status` command**
   - Inspect `.pid`, `.log`, `.result`, timestamps.
   - Detect states (`RUNNING`, `IDLE`, `STOPPED`, `ARCHIVED`, `DIED`).
   - Render human-readable summary (JSON/table).
   - **Depends on:** Task 2

8. **Implement `stop` command**
   - Write `/quit` into task pipe, wait for worker to exit, confirm cleanup.
   - Update metadata/timestamps, handle already-stopped tasks.
   - **Depends on:** Tasks 2 & 4

9. **Implement `ls` command**
   - Enumerate `tasks/` and `tasks/archive/**` directories.
   - Aggregate metadata for each task; support `--state` filters.
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
