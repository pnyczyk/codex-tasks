use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{from_str, json};
use tempfile::tempdir;
use uuid::Uuid;

const BIN: &str = "codex-tasks";

#[test]
fn help_lists_supported_subcommands() {
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("start"))
        .stdout(predicates::str::contains("send"))
        .stdout(predicates::str::contains("status"))
        .stdout(predicates::str::contains("log"))
        .stdout(predicates::str::contains("stop"))
        .stdout(predicates::str::contains("ls"))
        .stdout(predicates::str::contains("archive"));
}

#[test]
fn unfinished_subcommands_return_not_implemented_errors() {
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.args(["ls", "--state", "RUNNING"]);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("`ls` is not implemented yet"));

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.args(["status", "fake-task"]);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("`status` is not implemented yet"));
}

#[test]
fn start_command_creates_task_and_launches_worker() {
    let tmp = tempdir().expect("tempdir");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .arg("--title")
        .arg("Integration Title")
        .arg("Initial prompt")
        .env("HOME", tmp.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    let task_id = output.trim();
    assert!(!task_id.is_empty(), "start should print the task id");
    Uuid::parse_str(task_id).expect("task id should be a valid uuid");

    let store_root = tmp.path().join(".codex").join("tasks");
    let metadata_path = store_root.join(format!("{task_id}.json"));
    assert!(
        metadata_path.exists(),
        "metadata file should exist at {:?}",
        metadata_path
    );
    let metadata_contents = fs::read_to_string(&metadata_path).expect("metadata readable");
    #[derive(Debug, Deserialize)]
    struct Metadata {
        id: String,
        #[serde(default)]
        title: Option<String>,
        state: String,
        #[serde(default)]
        initial_prompt: Option<String>,
    }
    let metadata: Metadata = from_str(&metadata_contents).expect("metadata valid json");
    assert_eq!(metadata.id, task_id);
    assert_eq!(metadata.title.as_deref(), Some("Integration Title"));
    assert_eq!(metadata.state, "RUNNING");
    assert_eq!(metadata.initial_prompt.as_deref(), Some("Initial prompt"));

    let pid_path = store_root.join(format!("{task_id}.pid"));
    let mut attempts = 0;
    while !pid_path.exists() && attempts < 20 {
        attempts += 1;
        thread::sleep(Duration::from_millis(50));
    }
    assert!(pid_path.exists(), "worker should record its pid");
    let pid_contents = fs::read_to_string(pid_path).expect("pid readable");
    assert!(!pid_contents.trim().is_empty(), "pid should not be empty");
}

#[test]
fn send_returns_error_for_missing_task() {
    let tmp = tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", "missing-task", "prompt"]);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("task missing-task was not found"));
}

#[test]
fn send_rejects_inactive_states() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "stopped-task";
    write_metadata(&tasks_dir, task_id, "STOPPED");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "prompt"]);
    cmd.assert().failure().stderr(predicates::str::contains(
        "task stopped-task is STOPPED and cannot receive prompts",
    ));
}

#[test]
fn send_rejects_died_tasks() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "died-task";
    write_metadata(&tasks_dir, task_id, "DIED");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "prompt"]);
    cmd.assert().failure().stderr(predicates::str::contains(
        "task died-task has DIED and cannot receive prompts",
    ));
}

#[test]
fn send_writes_prompt_to_pipe() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "active-task";
    write_metadata(&tasks_dir, task_id, "IDLE");
    let pipe_path = tasks_dir.join(format!("{task_id}.pipe"));
    create_pipe(&pipe_path);

    let (tx, rx) = mpsc::channel();
    let reader_path = pipe_path.clone();
    let reader = thread::spawn(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .open(&reader_path)
            .expect("open pipe for read");
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read line");
        tx.send(line).expect("send line");
    });

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "hello world"]);
    cmd.assert().success();

    let line = rx.recv().expect("prompt from pipe");
    reader.join().expect("reader thread");
    assert_eq!(line, "hello world\n");
}

#[test]
fn send_errors_when_pipe_missing() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "missing-pipe-task";
    write_metadata(&tasks_dir, task_id, "IDLE");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "prompt"]);
    cmd.assert().failure().stderr(predicates::str::contains(
        "prompt pipe for task missing-pipe-task is missing; the worker may have stopped or exited",
    ));
}

fn write_metadata(tasks_dir: &Path, task_id: &str, state: &str) {
    let metadata_path = tasks_dir.join(format!("{task_id}.json"));
    let timestamp = Utc::now().to_rfc3339();
    let payload = json!({
        "id": task_id,
        "state": state,
        "created_at": timestamp,
        "updated_at": timestamp,
    });
    fs::write(
        metadata_path,
        serde_json::to_string_pretty(&payload).expect("serialize metadata"),
    )
    .expect("write metadata");
}

fn create_pipe(path: &Path) {
    if path.exists() {
        fs::remove_file(path).expect("remove existing pipe");
    }
    let c_path = CString::new(path.as_os_str().as_bytes()).expect("pipe path");
    let mode = 0o600;
    let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
    if result != 0 {
        panic!("failed to create pipe: {}", std::io::Error::last_os_error());
    }
}
