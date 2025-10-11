use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const FAKE_CODEX_SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import os
import sys
import time
import uuid

ROOT = os.path.abspath(os.environ.get("FAKE_CODEX_ROOT", "."))
DELAY_MS = int(os.environ.get("FAKE_CODEX_DELAY_MS", "0"))


def parse_args(args):
    output_path = None
    remaining = []
    idx = 0
    while idx < len(args):
        arg = args[idx]
        if arg == "--json":
            idx += 1
        elif arg == "--output-last-message":
            if idx + 1 >= len(args):
                raise SystemExit("missing value for --output-last-message")
            output_path = args[idx + 1]
            idx += 2
        elif arg == "--cd":
            if idx + 1 >= len(args):
                raise SystemExit("missing value for --cd")
            os.makedirs(args[idx + 1], exist_ok=True)
            os.chdir(args[idx + 1])
            idx += 2
        elif arg in {"--config", "-c", "--profile", "-p", "--model", "-m"}:
            idx += 2
        else:
            remaining.append(arg)
            idx += 1
    if remaining and remaining[0] == "exec":
        remaining = remaining[1:]
    return output_path, remaining


def session_counter_path(thread_id):
    return os.path.join(ROOT, f"{thread_id}.json")


def load_counter(thread_id):
    path = session_counter_path(thread_id)
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as handle:
            try:
                data = json.load(handle)
                return int(data.get("count", 0))
            except Exception:
                return 0
    return 0


def store_counter(thread_id, count):
    path = session_counter_path(thread_id)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump({"count": count}, handle)


def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()


def main():
    output_path, remaining = parse_args(sys.argv[1:])
    if not remaining:
        sys.stderr.write("missing prompt\n")
        return 1

    if remaining[0] == "resume":
        if len(remaining) < 3:
            sys.stderr.write("resume requires thread id and prompt\n")
            return 1
        thread_id = remaining[1]
        prompt = remaining[2]
    else:
        thread_id = str(uuid.uuid4())
        prompt = remaining[0]

    count = load_counter(thread_id) + 1
    store_counter(thread_id, count)
    message = f"response {count}: {prompt}"

    emit({"type": "thread.started", "thread_id": thread_id})
    emit({"type": "turn.started"})

    if DELAY_MS > 0:
        time.sleep(DELAY_MS / 1000.0)

    emit(
        {
            "type": "item.completed",
            "item": {
                "id": f"reasoning_{count}",
                "type": "reasoning",
                "text": f"thinking about {prompt}",
            },
        }
    )
    emit(
        {
            "type": "item.completed",
            "item": {
                "id": f"message_{count}",
                "type": "agent_message",
                "text": message,
            },
        }
    )
    emit({"type": "turn.completed", "usage": {"output_tokens": len(message)}})

    if output_path:
        with open(output_path, "w", encoding="utf-8") as handle:
            handle.write(message)

    return 0


if __name__ == "__main__":
    sys.exit(main())
"#;

pub fn write_fake_codex(bin_dir: &Path) {
    fs::create_dir_all(bin_dir).expect("create fake codex bin dir");
    let script_path = bin_dir.join("codex");
    fs::write(&script_path, FAKE_CODEX_SCRIPT).expect("write fake codex script");
    let mut permissions = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("set script permissions");
}
