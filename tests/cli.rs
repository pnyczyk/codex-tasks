use std::collections::VecDeque;
use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use chrono::{TimeZone, Utc};
use predicates::prelude::PredicateBooleanExt;
use serde::Deserialize;
use serde_json::{Value, from_str, json};
use tempfile::{TempDir, tempdir};
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
fn archive_reports_missing_task() {
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.args(["archive", "task-xyz"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("task task-xyz was not found"));
}

#[test]
fn ls_lists_active_and_archived_tasks() {
    let home = tempdir().expect("tempdir");
    let task_root = home.path().join(".codex").join("tasks");
    let archive_root = task_root.join("archive");
    fs::create_dir_all(&archive_root).expect("layout");

    let active_id = "task-active";
    let active_time = Utc
        .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
        .single()
        .expect("timestamp");
    let active_metadata = json!({
        "id": active_id,
        "title": "Active Task",
        "state": "RUNNING",
        "created_at": active_time.to_rfc3339(),
        "updated_at": active_time.to_rfc3339(),
    });
    let active_dir = task_root.join(active_id);
    fs::create_dir_all(&active_dir).expect("active dir");
    fs::write(
        active_dir.join("task.json"),
        serde_json::to_string_pretty(&active_metadata).expect("serialize"),
    )
    .expect("write active metadata");

    let archived_id = "task-archived";
    let archived_time = Utc
        .with_ymd_and_hms(2023, 12, 31, 23, 59, 59)
        .single()
        .expect("timestamp");
    let archived_dir = archive_root
        .join("2023")
        .join("12")
        .join("31")
        .join(archived_id);
    fs::create_dir_all(&archived_dir).expect("archive dir");
    let archived_metadata = json!({
        "id": archived_id,
        "title": "Archived Task",
        "state": "ARCHIVED",
        "created_at": archived_time.to_rfc3339(),
        "updated_at": archived_time.to_rfc3339(),
    });
    fs::write(
        archived_dir.join("task.json"),
        serde_json::to_string_pretty(&archived_metadata).expect("serialize"),
    )
    .expect("write archive metadata");
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path()).arg("ls");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("ID"))
        .stdout(predicates::str::contains("task-active"))
        .stdout(predicates::str::contains("task-archived"))
        .stdout(predicates::str::contains("ACTIVE"))
        .stdout(predicates::str::contains("ARCHIVE"))
        .stdout(predicates::str::contains("RUNNING"))
        .stdout(predicates::str::contains("ARCHIVED"));
}

#[test]
fn ls_supports_state_filtering() {
    let home = tempdir().expect("tempdir");
    let task_root = home.path().join(".codex").join("tasks");
    let archive_root = task_root.join("archive");
    fs::create_dir_all(&archive_root).expect("layout");

    let running_id = "task-running";
    let running_time = Utc
        .with_ymd_and_hms(2024, 2, 2, 2, 2, 2)
        .single()
        .expect("timestamp");
    let running_metadata = json!({
        "id": running_id,
        "title": "Running Task",
        "state": "RUNNING",
        "created_at": running_time.to_rfc3339(),
        "updated_at": running_time.to_rfc3339(),
    });
    let running_dir = task_root.join(running_id);
    fs::create_dir_all(&running_dir).expect("running dir");
    fs::write(
        running_dir.join("task.json"),
        serde_json::to_string_pretty(&running_metadata).expect("serialize"),
    )
    .expect("write running metadata");

    let archived_id = "task-archived";
    let archived_time = Utc
        .with_ymd_and_hms(2024, 2, 1, 1, 1, 1)
        .single()
        .expect("timestamp");
    let archived_dir = archive_root
        .join("2024")
        .join("02")
        .join("01")
        .join(archived_id);
    fs::create_dir_all(&archived_dir).expect("archive dir");
    let archived_metadata = json!({
        "id": archived_id,
        "title": "Archived Task",
        "state": "ARCHIVED",
        "created_at": archived_time.to_rfc3339(),
        "updated_at": archived_time.to_rfc3339(),
    });
    fs::write(
        archived_dir.join("task.json"),
        serde_json::to_string_pretty(&archived_metadata).expect("serialize"),
    )
    .expect("write archive metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path())
        .args(["ls", "--state", "RUNNING"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("task-running"))
        .stdout(predicates::str::contains("RUNNING"))
        .stdout(predicates::str::contains("ACTIVE"))
        .stdout(predicates::str::contains("ID"))
        .stdout(predicates::str::contains("ARCHIVE").not())
        .stdout(predicates::str::contains("task-archived").not());
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
    let task_dir = store_root.join(task_id);
    let metadata_path = task_dir.join("task.json");
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

    let pid_path = task_dir.join("task.pid");
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
    let task_id = "archived-task";
    let archived_dir = tasks_dir
        .join("archive")
        .join("2024")
        .join("01")
        .join("02")
        .join(task_id);
    fs::create_dir_all(&archived_dir).expect("archived dir");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let metadata = json!({
        "id": task_id,
        "state": "ARCHIVED",
        "created_at": timestamp.to_rfc3339(),
        "updated_at": timestamp.to_rfc3339(),
    });
    fs::write(
        archived_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).expect("serialize"),
    )
    .expect("write archived metadata");

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
    let pipe_path = tasks_dir.join(task_id).join("task.pipe");
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
    let pipe_path = tasks_dir.join(task_id).join("task.pipe");
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

#[test]
fn stop_handles_missing_task_gracefully() {
    let tmp = tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path()).args(["stop", "task-xyz"]);
    cmd.assert().success().stdout(predicates::str::contains(
        "Task task-xyz is not running; nothing to stop.",
    ));
}

#[test]
fn archive_moves_task_into_archive() {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path();
    let tasks_dir = home.join(".codex").join("tasks");
    let task_id = "task-archive";
    let task_dir = tasks_dir.join(task_id);
    fs::create_dir_all(&task_dir).expect("task dir");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("timestamp");
    let metadata = json!({
        "id": task_id,
        "state": "STOPPED",
        "created_at": timestamp.to_rfc3339(),
        "updated_at": timestamp.to_rfc3339(),
    });
    fs::write(
        task_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).expect("serialize"),
    )
    .expect("write metadata");
    fs::write(task_dir.join("task.log"), "log contents").expect("log");
    fs::write(task_dir.join("task.result"), "final result").expect("result");
    fs::write(task_dir.join("task.pid"), "1234").expect("pid");
    create_pipe(task_dir.join("task.pipe").as_path());

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    let assert = cmd
        .env("HOME", home)
        .args(["archive", task_id])
        .assert()
        .success();

    assert!(
        !task_dir.exists(),
        "active task directory should be moved into the archive"
    );

    let archive_root = tasks_dir.join("archive");
    let archived_dir = find_task_directory(&archive_root, task_id).expect("archived dir");
    let metadata_contents =
        fs::read_to_string(archived_dir.join("task.json")).expect("archived metadata");
    let value: Value = serde_json::from_str(&metadata_contents).expect("valid metadata");
    assert_eq!(value["id"], task_id);
    assert_eq!(value["state"], "ARCHIVED");
    assert_eq!(
        fs::read_to_string(archived_dir.join("task.log")).expect("archived log"),
        "log contents"
    );
    assert_eq!(
        fs::read_to_string(archived_dir.join("task.result")).expect("archived result"),
        "final result"
    );
    assert!(
        !archived_dir.join("task.pid").exists(),
        "pid file should be removed before archiving"
    );
    assert!(
        !archived_dir.join("task.pipe").exists(),
        "pipe should be removed before archiving"
    );
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(stdout.contains("archived to"));
}

#[test]
fn archive_rejects_running_task() {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path();
    let tasks_dir = home.join(".codex").join("tasks");
    let task_id = "task-running";
    let task_dir = tasks_dir.join(task_id);
    fs::create_dir_all(&task_dir).expect("task dir");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 6, 2, 1, 0, 0)
        .single()
        .expect("timestamp");
    let metadata = json!({
        "id": task_id,
        "state": "RUNNING",
        "created_at": timestamp.to_rfc3339(),
        "updated_at": timestamp.to_rfc3339(),
    });
    fs::write(
        task_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).expect("serialize"),
    )
    .expect("write metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home)
        .args(["archive", task_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "task task-running is RUNNING; stop it before archiving",
        ));
}

fn write_metadata(tasks_dir: &Path, task_id: &str, state: &str) {
    let task_dir = tasks_dir.join(task_id);
    fs::create_dir_all(&task_dir).expect("task directory");
    let metadata_path = task_dir.join("task.json");
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

fn find_task_directory(root: &Path, task_id: &str) -> Option<PathBuf> {
    if !root.exists() {
        return None;
    }
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        if dir
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name == task_id)
        {
            return Some(dir);
        }
        for entry in fs::read_dir(&dir).expect("read archive directory") {
            let entry = entry.expect("archive entry");
            if entry.file_type().expect("entry type").is_dir() {
                queue.push_back(entry.path());
            }
        }
    }
    None
}

#[test]
fn status_reports_idle_task_in_json() {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path();
    let task_id = "task-123";
    let created_at = "2024-05-01T12:34:56Z";
    let store_root = home.join(".codex").join("tasks");
    let task_dir = store_root.join(task_id);
    fs::create_dir_all(&task_dir).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "title": "Example task",
        "state": "IDLE",
        "created_at": created_at,
        "updated_at": created_at
    });
    fs::write(
        task_dir.join("task.json"),
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
    let task_dir = store_root.join(task_id);
    fs::create_dir_all(&task_dir).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "state": "RUNNING",
        "created_at": timestamp,
        "updated_at": timestamp
    });
    fs::write(
        task_dir.join("task.json"),
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
    let task_dir = store_root.join(task_id);
    fs::create_dir_all(&task_dir).expect("store root");
    let metadata = serde_json::json!({
        "id": task_id,
        "state": "RUNNING",
        "created_at": timestamp,
        "updated_at": timestamp
    });
    fs::write(
        task_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");

    let mut child = StdCommand::new("sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let pid = i32::try_from(child.id()).expect("pid fits in i32");
    fs::write(task_dir.join("task.pid"), pid.to_string()).expect("pid file");

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
        .join("archive")
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
        archive_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .expect("metadata");
    fs::write(archive_dir.join("task.result"), "final outcome").expect("result");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home).args(["status", "--json", task_id]);
    let output = cmd.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["state"], "ARCHIVED");
    assert_eq!(value["location"], "archived");
    assert_eq!(value["pid"], Value::Null);
    assert_eq!(value["last_result"], "final outcome");
}

#[test]
fn log_displays_entire_file() {
    let home = tempdir().expect("tempdir");
    let task_dir = home
        .path()
        .join(".codex")
        .join("tasks")
        .join("task-123");
    fs::create_dir_all(&task_dir).expect("create dirs");
    let log_path = task_dir.join("task.log");
    fs::write(&log_path, b"line one\nline two\n").expect("write log");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path());
    cmd.args(["log", "task-123"]);
    cmd.assert().success().stdout("line one\nline two\n");
}

#[test]
fn log_honors_tail_flag() {
    let home = tempdir().expect("tempdir");
    let task_dir = home
        .path()
        .join(".codex")
        .join("tasks")
        .join("task-abc");
    fs::create_dir_all(&task_dir).expect("create dirs");
    let log_path = task_dir.join("task.log");
    fs::write(&log_path, b"keep\nlast\nline\n").expect("write log");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path());
    cmd.args(["log", "-n", "1", "task-abc"]);
    cmd.assert().success().stdout("line\n");
}

#[test]
fn log_reads_archived_tasks() {
    let home = tempdir().expect("tempdir");
    let log_path = home
        .path()
        .join(".codex")
        .join("tasks")
        .join("archive")
        .join("2024")
        .join("01")
        .join("02")
        .join("task-archived")
        .join("task.log");
    fs::create_dir_all(log_path.parent().expect("parent exists")).expect("create dirs");
    fs::write(&log_path, b"archived\ncontent\n").expect("write log");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path());
    cmd.args(["log", "task-archived"]);
    cmd.assert().success().stdout("archived\ncontent\n");
}
