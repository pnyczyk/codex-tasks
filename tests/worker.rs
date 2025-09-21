use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

const BIN: &str = "codex-tasks";

#[test]
fn worker_subcommand_writes_pid_file() {
    let tmp = tempdir().expect("tempdir");
    let task_id = "integration-worker";
    let pid_path = tmp.path().join(format!("{task_id}.pid"));

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("worker")
        .arg("--task-id")
        .arg(task_id)
        .arg("--store-root")
        .arg(tmp.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1")
        .env("CODEX_TASK_TITLE", "Integration Title")
        .env("CODEX_TASK_PROMPT", "Integration Prompt");
    cmd.assert().success();

    let contents = fs::read_to_string(&pid_path).expect("pid file should exist");
    let value = contents
        .trim()
        .parse::<i32>()
        .expect("pid file should contain an integer");
    assert!(value > 0, "pid should be positive");
}
