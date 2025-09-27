# Product Requirements – `codex-tasks`

## 1. Overview
`codex-tasks` is a standalone CLI for launching and managing multiple Codex sessions in the background. Each session ("task") runs as its own helper process built on the existing `codex-task` implementation. The helper process is responsible for starting `codex proto`, streaming and rendering its transcript, and maintaining a simple filesystem-based “database” under `~/.codex/tasks`.

## 2. Goals
- Allow users to start Codex jobs that continue running after the CLI exits.
- Provide lightweight commands to send additional prompts, inspect results, and stop/archive tasks.
- Preserve the existing transcript formatting, shutdown semantics, and UTF-8 handling from `codex-task`.
- Avoid a central daemon; each task is self-contained in its own process with a small set of files.

## 3. CLI surface
```
codex-tasks start [-t <title>] [--config-file <PATH>] [--working-dir <DIR>] [--repo <URL>] [--repo-ref <REF>] [prompt]
codex-tasks send <task_id> <prompt>
codex-tasks status <task_id>
codex-tasks log [-f] [-n <lines>] <task_id>
codex-tasks stop <task_id>
codex-tasks ls [--state <STATE> ...]
codex-tasks archive <task_id>
```
### 3.1 `start`
- Generate a new UUID (`task_id`).
- Create task files in `~/.codex/tasks/` (`pid`, `pipe`, `log`, `result`).
- Fork a helper process (the “task worker”).
  - Worker launches `codex proto`, reusing the current `codex-task` I/O pipeline.
  - If an initial prompt is provided, send it immediately.
- Optional inputs that tailor worker launch:
  - `--config-file PATH`: load a custom `config.toml` for Codex with the path’s parent treated as `CODEX_HOME`.
  - `--working-dir DIR`: run `codex proto` from this directory (created if missing, unless repo cloning is requested).
  - `--repo URL`: clone the Git repository into the parent of `DIR` and use the directory name as the clone target (requires `--working-dir`).
  - `--repo-ref REF`: checkout the named branch/tag/commit after cloning.
- Return `task_id` to stdout.

### 3.2 `send`
- Resolve the task directory and open `<task_id>.pipe`.
- Write the prompt (newline-terminated UTF-8) so the worker can forward it to Codex.

### 3.3 `status`
- Inspect the task files and process state.
- Determine status (`IDLE`, `RUNNING`, `STOPPED`, `ARCHIVED`, `DIED`).
- Output JSON or table with status, title (if any), timestamps, and last result (if available).

### 3.4 `log`
- Stream the rendered transcript from `<task_id>.log`.
- `-f/--follow` streams until the worker returns to `IDLE` (or reaches a terminal state), exiting automatically afterwards.
- `--forever`/`-F` implies `--follow` and retains the original "tail indefinitely" behavior for users who want to keep the stream open.
- `-n/--lines <N>` restricts the initial dump to the last *N* lines before optionally following.
- Intended as the primary observability tool without attaching a TTY to the worker.

### 3.5 `stop`
- Notify the worker to send `shutdown` to Codex, wait for `ShutdownComplete`, and clean up (`.pid`, `.pipe`).
- After graceful exit, status becomes `STOPPED`.

### 3.6 `ls`
- List active tasks in `~/.codex/tasks/`.
- Use `-a/--all` to include archived entries from `archive/YYYY/MM/DD/`.
- Optional `--state` filters (multiple allowed via repeated flags or comma-delimited values).

### 3.7 `archive`
- Move task files into `archive/<YYYY>/<MM>/<DD>/<task_id>/`.
- Status becomes `ARCHIVED`.

## 4. Task data model
- `task_id`: UUID (e.g., `7df7c873-...`).
- `title`: optional string provided at `start`.
- `state`: one of {`IDLE`, `RUNNING`, `STOPPED`, `ARCHIVED`, `DIED`}.
- `created_at`, `updated_at` timestamps (recorded by worker).
- `last_result`: UTF-8 text of the most recent Codex answer (available in `IDLE`, `STOPPED`, `ARCHIVED`).
- `last_prompt`: UTF-8 text of the most recent user prompt (updated on `start` and each `send`).

## 5. Filesystem layout (`~/.codex/tasks/`)
- Active tasks live under `~/.codex/tasks/<task_id>/`, keeping related files grouped together.
- Each task directory stores `task.pid`, `task.pipe`, `task.log`, `task.result`, and `task.json`.
- Archived tasks move to `~/.codex/tasks/archive/<YYYY>/<MM>/<DD>/<task_id>/` with the same filenames.

```
~/.codex/tasks/
  <task_id>/
    task.pid
    task.pipe
    task.log
    task.result
    task.json
  archive/<YYYY>/<MM>/<DD>/<task_id>/...
```
- When a task is STOPPED or DIED, `.pid` and `.pipe` are removed; log/result remain.
- LOG format matches current `codex-task` output (timestamps, headings, reasoning blocks, etc.).

## 6. Worker lifecycle
1. **Spawn**: parent CLI forks a worker, writes initial files, and returns `task_id`.
2. **Initialization**: worker launches `codex proto` with piped stdin/stdout, replicating existing `codex-task` logic, honoring any custom config file and working directory derived from the CLI flags.
3. **Main loop**:
   - Keep `<task_id>.pipe` open for reading; translate each UTF-8 segment into a prompt. The worker tolerates writers connecting and disconnecting repeatedly, only reacting to the explicit `/quit` sentinel.
   - Forward Codex events to the renderer, appending to `<task_id>.log` and updating `<task_id>.result` when appropriate.
4. **Shutdown**:
   - `stop` writes `/quit` into the pipe. The worker interprets this sentinel, sends `{"id":"sub-…","op":{"type":"shutdown"}}`, and begins the shutdown sequence.
   - Wait for `ShutdownComplete`; close pipes; kill process on timeout (5 s) with logging.
5. **Termination**: remove `.pid`/`.pipe`. If exit was graceful → `STOPPED`; if worker dies unexpectedly → `DIED`.

## 7. State transitions
```
(start) → IDLE or RUNNING (if prompt) ↔ RUNNING (during codex call)
RUNNING → IDLE (on successful completion)
stop → STOPPED (after graceful shutdown)
worker crash / missing PID → DIED
archive → ARCHIVED (files moved to archive/…)
```

## 8. Error handling & logging
- All Codex interaction logs go to `<task_id>.log` (exact renderer from `codex-task`).
- `status` detects:
  - Missing `task.pid` but existing log ⇒ `DIED`.
  - Archived tasks based on directory location.
- CLI commands exit non-zero with a clear message if:
  - Task ID not found.
  - Pipe missing (STOPPED/ARCHIVED tasks) unless command is `status` or `ls`.
  - Worker fails to start (`start` surfaces error).

## 9. Reuse of existing code
- Copy or refactor the current `codex-task` communication layer (spawning `codex proto`, streaming events, UTF-8 line handling, shutdown procedure) into a reusable module consumed by each worker.
- Reuse the transcript renderer to ensure output parity.

## 10. Future enhancements (out of scope for initial version)
- Optional metadata cache (`meta.json`) to avoid scanning directories on `ls`.
- Structured JSON output for `status` / `ls`.
- Additional configuration customization templates for workers (beyond current CLI flags), such as containerized execution, scripted dependency installation, or secrets management.
- Integration hooks for external orchestrators.
