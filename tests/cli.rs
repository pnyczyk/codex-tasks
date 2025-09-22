use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command as StdCommand;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{from_str, json, Value};
use tempfile::{tempdir, TempDir};
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
fn send_rejects_stopped_tasks() {
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
fn send_rejects_archived_tasks() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "archived-task";
    write_metadata(&tasks_dir, task_id, "ARCHIVED");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "prompt"]);
    cmd.assert().failure().stderr(predicates::str::contains(
        "task archived-task is ARCHIVED and cannot receive prompts",
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
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains(
            "prompt pipe for task missing-pipe-task is missing; the worker may have STOPPED, DIED, or been ARCHIVED",
        ));
}

#[test]
fn send_errors_when_pipe_has_no_reader() {
    let tmp = tempdir().expect("tempdir");
    let tasks_dir = tmp.path().join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    let task_id = "no-reader-task";
    write_metadata(&tasks_dir, task_id, "IDLE");
    let pipe_path = tasks_dir.join(format!("{task_id}.pipe"));
    create_pipe(&pipe_path);

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path())
        .args(["send", task_id, "prompt"]);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains(
            "task no-reader-task is not accepting prompts; the worker may have STOPPED, DIED, or been ARCHIVED",
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

#[test]
fn status_reports_idle_task_in_json() {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path();
    let task_id = "task-123";
    let created_at = "2024-05-01T12:34:56Z";
    let store_root = home.join(".codex").join("tasks");
    fs::create_dir_all(&store_root).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "title": "Example task",
        "state": "IDLE",
        "created_at": created_at,
        "updated_at": created_at
    });
    fs::write(
        store_root.join(format!("{task_id}.json")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home).args(["status", "--json", task_id]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["id"], task_id);
    assert_eq!(value["title"], "Example task");
    assert_eq!(value["state"], "IDLE");
    assert_eq!(value["location"], "active");
    assert_eq!(value["pid"], Value::Null);
}

#[test]
fn status_flags_missing_pid_as_died() {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path();
    let task_id = "task-456";
    let timestamp = "2024-05-02T00:00:00Z";
    let store_root = home.join(".codex").join("tasks");
    fs::create_dir_all(&store_root).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "state": "RUNNING",
        "created_at": timestamp,
        "updated_at": timestamp
    });
    fs::write(
        store_root.join(format!("{task_id}.json")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home).args(["status", "--json", task_id]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["state"], "DIED");
    assert_eq!(value["location"], "active");
    assert_eq!(value["pid"], Value::Null);
}

#[test]
fn status_reports_running_task_when_pid_alive() {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path();
    let task_id = "task-789";
    let timestamp = "2024-05-03T06:07:08Z";
    let store_root = home.join(".codex").join("tasks");
    fs::create_dir_all(&store_root).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "state": "RUNNING",
        "created_at": timestamp,
        "updated_at": timestamp
    });
    fs::write(
        store_root.join(format!("{task_id}.json")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");

    let mut child = StdCommand::new("sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let pid = i32::try_from(child.id()).expect("pid fits in i32");
    fs::write(store_root.join(format!("{task_id}.pid")), pid.to_string()).expect("pid file");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home).args(["status", "--json", task_id]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["state"], "RUNNING");
    assert_eq!(value["pid"], pid);
    assert_eq!(value["location"], "active");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn status_detects_archived_tasks() {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path();
    let task_id = "task-archived";
    let timestamp = "2024-05-04T09:10:11Z";
    let store_root = home.join(".codex").join("tasks");
    let archive_dir = store_root
        .join("done")
        .join("2024")
        .join("05")
        .join("04")
        .join(task_id);
    fs::create_dir_all(&archive_dir).expect("archive dir");
    let metadata = serde_json::json!({
        "id": task_id,
        "state": "ARCHIVED",
        "created_at": timestamp,
        "updated_at": timestamp
    });
    fs::write(
        archive_dir.join(format!("{task_id}.json")),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");
    fs::write(
        archive_dir.join(format!("{task_id}.result")),
        "final outcome",
    )
    .expect("result");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home).args(["status", "--json", task_id]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["state"], "ARCHIVED");
    assert_eq!(value["location"], "archived");
    assert_eq!(value["pid"], Value::Null);
    assert_eq!(value["last_result"], "final outcome");
}
