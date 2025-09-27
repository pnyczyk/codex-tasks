# codex-tasks

`codex-tasks` is a standalone command-line tool for launching and managing background Codex sessions. Each session (called a "task") spawns a helper process that talks to `codex proto`, records transcripts, and keeps lightweight metadata on disk so you can reconnect at any time.

## Features
- **Start tasks** that continue running after the CLI exits and optionally send an initial prompt.
- **Send follow-up prompts** to a running task without attaching to its TTY.
- **Inspect task status** (state, timestamps, last prompt, last result) in human-readable or JSON form.
- **Stream logs** to review Codex output in real time or tail past sessions.
- **Stop or archive tasks** to clean up resources and keep historical transcripts organized.

Task data is stored under `~/.codex/tasks/` with per-task directories and a dated archive hierarchy for completed sessions.

## Installation
1. Install the Rust toolchain (Rust 1.78 or newer) using [rustup](https://rustup.rs/).
2. Fetch this repository and its git submodules:
   ```bash
   git clone --recurse-submodules https://github.com/pnyczyk/codex-tasks.git
   cd codex-tasks
   ```
3. Build the CLI:
   ```bash
   cargo build --release
   ```
4. Add the binary to your `PATH` (optional):
   ```bash
   cp target/release/codex-tasks ~/.local/bin/
   ```

## Usage
The CLI exposes several subcommands; run `codex-tasks <command> --help` for full details.

| Command | Description |
| --- | --- |
| `codex-tasks start [-t <title>] [prompt]` | Create a new task, optionally sending an initial prompt. |
| `codex-tasks send <task_id> <prompt>` | Send another prompt to an existing task. |
| `codex-tasks status [--json] <task_id>` | Show live status, metadata, and the last prompt/result. |
| `codex-tasks log [-f\|--follow] [--forever] [-n <lines>] <task_id>` | Stream or tail the transcript for a task. |
| `codex-tasks stop <task_id>` | Gracefully shut down the worker process. |
| `codex-tasks ls [-a\|--all] [--state <STATE> ...]` | List active tasks, optionally including archived ones and filtering by state. |
| `codex-tasks archive [-a\|--all] [<task_id>]` | Archive a specific task or bulk archive all STOPPED/DIED tasks. |

The `start` subcommand accepts additional flags for tailoring the worker environment:
- `--config-file PATH` loads a custom `config.toml` for `codex proto` (the file must be named `config.toml`).
- `--working-dir DIR` runs `codex proto` inside the specified directory, creating it when needed.
- `--repo URL` clones a Git repository into the working directory before launching the worker (requires `--working-dir`).
- `--repo-ref REF` checks out the given branch, tag, or commit after cloning the repository.

The `log -f/--follow` flag now exits automatically once the worker returns to `IDLE`, `STOPPED`, or `DIED`. Use `--forever` (or `-F`) to retain the original "follow until interrupted" behavior. The `archive -a/--all` flag bulk-archives every task currently in `STOPPED` or `DIED` state.

### Typical workflow
```bash
# Start a task with an initial question and capture the generated task ID
TASK_ID=$(codex-tasks start -t "Investigate bug" "Why is request latency spiking?")

# Send a follow-up prompt later
codex-tasks send "$TASK_ID" "Summarize the repro steps as bullet points."

# Inspect progress
codex-tasks status "$TASK_ID"

# Tail the transcript
codex-tasks log -f "$TASK_ID"

# Stop and archive when finished
codex-tasks stop "$TASK_ID"
codex-tasks archive "$TASK_ID"
```

## Additional documentation
- [Product requirements](docs/prd.md)
- [Implementation plan](docs/plan.md)
- [Codex protocol reference](docs/proto.md)

These documents describe the broader architecture, development roadmap, and wire protocol used by the worker process.

## Contributing
Run the test suite before submitting changes:
```bash
cargo test
```

Please follow the existing coding style and update relevant documentation when introducing new commands or behavior.
