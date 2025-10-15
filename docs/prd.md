# Product Requirements – `codex-tasks`

## 1. Overview
`codex-tasks` is a standalone CLI for launching and managing multiple Codex sessions in the background. Each session ("task") is anchored by a Codex `thread_id` and advances through successive `codex exec --json` or `codex exec resume` invocations while the CLI records transcripts and lightweight metadata under `~/.codex/tasks`.

## 2. Goals
- Allow users to start Codex jobs that continue running after the CLI exits.
- Provide lightweight commands to send additional prompts, inspect results, and stop/archive tasks.
- Preserve the existing transcript formatting, shutdown semantics, and UTF-8 handling from `codex-task`.
- Avoid a central daemon; each task is self-contained in its own process with a small set of files.

## 3. CLI surface
```
codex-tasks start [-t <title>] [--config-file <PATH>] [--working-dir <DIR>] [--repo <URL>] [--repo-ref <REF>] <prompt>
codex-tasks send <task_id> <prompt>
codex-tasks status <task_id>
codex-tasks log [--json] [-f] [-n <lines>] <task_id>
codex-tasks stop <task_id>
codex-tasks ls [--state <STATE> ...]
codex-tasks archive <task_id>
```
### 3.1 `start`
- Requires an initial prompt (supplied as an argument or via `-`/stdin).
- Forks a helper process (the “task worker”) that immediately launches `codex exec --json` using the selected working directory and config overrides.
- Buffers JSON events until a `thread.started` event arrives, then persists task files in `~/.codex/tasks/<thread_id>/` (`task.json`, `task.log`, `task.result`, `task.pid`) and prints the Codex `thread_id` (the canonical task identifier) to stdout.
- Optional inputs that tailor worker launch:
  - `--config-file PATH`: load a custom `config.toml` (must be named `config.toml`). The worker sets `CODEX_HOME` to that file’s parent before invoking `codex exec`.
  - `--working-dir DIR`: run `codex exec` from this directory (created if missing, unless repo cloning is requested). If omitted, the worker records the CLI’s current directory for reuse on future prompts.
  - `--repo URL`: clone the Git repository into the parent of `DIR` and use the directory name as the clone target (requires `--working-dir`).
  - `--repo-ref REF`: checkout the named branch/tag/commit after cloning.

### 3.2 `send`
- Resolve the task directory, validate that the task is neither `ARCHIVED` nor `DIED`, and ensure no worker process is still running.
- Spawn a new worker that resumes the existing `thread_id` by calling `codex exec --json resume <thread_id> "<prompt>"`.
- Record the updated `last_prompt` metadata before returning.

### 3.3 `status`
- Inspect the task files and process state.
- Determine status (`RUNNING`, `STOPPED`, `ARCHIVED`, `DIED`) based on metadata and the presence of a live worker PID.
- Output JSON or table with status, title (if any), timestamps, and last result (if available).

### 3.4 `log`
- Stream the transcript from `<task_id>.log`. By default the CLI renders human-friendly output matching `codex exec`; `--json` streams raw JSONL events.
- `-f/--follow` streams until the current invocation completes and the task transitions to `STOPPED` or `DIED`, exiting automatically afterwards. Combine with `--forever`/`-F` to continue streaming indefinitely.
- `--forever`/`-F` implies `--follow` and retains the original "tail indefinitely" behavior for users who want to keep the stream open.
- `-n/--lines <N>` restricts the initial dump to the last *N* lines before optionally following.
- Intended as the primary observability tool without attaching a TTY to the worker.

### 3.5 `stop`
- Inspect the task’s recorded PID and, if the worker is still running, send `SIGTERM` followed by a timed wait for the process to exit cleanly.
- After graceful exit, status becomes `STOPPED` and the PID file is removed.
- `-a/--all` stops every task that currently has a live worker process, printing per-task outcomes.

### 3.6 `ls`
- List active tasks in `~/.codex/tasks/`.
- Use `-a/--all` to include archived entries from `archive/YYYY/MM/DD/`.
- Optional `--state` filters (multiple allowed via repeated flags or comma-delimited values).

### 3.7 `archive`
- `archive <task_id>` moves the specified task into `archive/<YYYY>/<MM>/<DD>/<task_id>/` and marks it `ARCHIVED`.
- `archive -a/--all` iterates over all tasks, archiving those whose state is `STOPPED` or `DIED` and skipping others.
- Status becomes `ARCHIVED` for each archived task.

## 4. Task data model
- `task_id`: Codex `thread_id` returned by the service (opaque string).
- `title`: optional string provided at `start`.
- `state`: one of {`RUNNING`, `STOPPED`, `ARCHIVED`, `DIED`}.
- `created_at`, `updated_at` timestamps (recorded by worker).
- `last_result`: UTF-8 text of the most recent Codex answer (available in `STOPPED` or `ARCHIVED`).
- `last_prompt`: UTF-8 text of the most recent user prompt (updated on `start` and each `send`).
- `working_dir`: absolute path used when launching Codex; defaults to the CLI's current directory if `--working-dir` was not provided.

## 5. Filesystem layout (`~/.codex/tasks/`)
- Active tasks live under `~/.codex/tasks/<task_id>/`, keeping related files grouped together.
- Each task directory stores `task.pid`, `task.log`, `task.result`, and `task.json`. Legacy tasks created before the `codex exec` migration may also include `task.pipe`.
- Archived tasks move to `~/.codex/tasks/archive/<YYYY>/<MM>/<DD>/<task_id>/` with the same filenames.

```
~/.codex/tasks/
  <task_id>/
    task.pid
    task.log
    task.result
    task.json
  archive/<YYYY>/<MM>/<DD>/<task_id>/...
```
- When a task is STOPPED or DIED, the worker removes the `.pid` file (and any legacy `.pipe` file); log/result remain.
- `task.log` contains a JSON Lines stream of Codex events (`thread.started`, `turn.completed`, `item.completed`, etc.) including synthetic `user_message` entries for prompts and `stderr` entries for diagnostics. CLI rendering converts this stream into the familiar human transcript.

## 6. Worker lifecycle
1. **Spawn**: the CLI forks a worker with the prompt, optional title, and configuration overrides. For follow-up prompts, the worker receives the existing `thread_id`.
2. **Initialization**: the worker prepares the task store layout and loads any existing metadata so it can reuse stored config paths or working directories.
3. **Invocation**:
   - For a new task, invoke `codex exec --json "<prompt>"`. Buffer events until `thread.started` arrives, then write metadata/log files under the emitted `thread_id` and flush buffered lines.
   - For follow-up prompts, invoke `codex exec --json resume <thread_id> "<prompt>"` immediately, appending `user_message` and streamed events to the existing log.
   - Record the worker PID and update metadata (state → `RUNNING`, refresh `last_prompt`).
4. **Completion**:
   - Wait for the process to exit. On success, promote the `--output-last-message` artifact into `task.result` and set state → `STOPPED`. On failure, mark the task `DIED`.
5. **Finalization**: flush buffered log output, remove the PID file, and exit. Any follow-up prompts will spawn a fresh worker repeating the flow above.

## 7. State transitions
```
(start) → RUNNING (initial codex exec invocation)
RUNNING → STOPPED (invocation exits successfully)
RUNNING → DIED (exec process fails or crashes)
stop → STOPPED (signal worker and wait for exit)
archive → ARCHIVED (files moved to archive/…)
```

## 8. Error handling & logging
- All Codex interaction logs go to `<task_id>.log` (same renderer as `codex exec`).
- `status` detects:
  - Missing `task.pid` but existing log ⇒ `DIED`.
  - Archived tasks based on directory location.
- CLI commands exit non-zero with a clear message if:
  - Task ID not found.
  - Task is `ARCHIVED` or `DIED` when attempting to send a prompt.
  - Worker fails to start or `codex exec` exits with an error code.

## 9. Reuse of existing code
- Reuse the `codex-task` transcript renderer to convert JSON events into human-readable logs.
- Share the existing storage helpers for metadata, log/result persistence, and archiving.

## 10. Future enhancements (out of scope for initial version)
- Optional metadata cache (`meta.json`) to avoid scanning directories on `ls`.
- Structured JSON output for `status` / `ls`.
- Additional configuration customization templates for workers (beyond current CLI flags), such as containerized execution, scripted dependency installation, or secrets management.
- Integration hooks for external orchestrators.
