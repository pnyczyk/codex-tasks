use std::collections::VecDeque;
use std::convert::TryFrom;
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use chrono::{TimeZone, Utc};
use predicates::prelude::PredicateBooleanExt;
use serde::Deserialize;
use serde_json::{Value, from_str, json};
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

const BIN: &str = "codex-tasks";

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(
        status.success(),
        "git {:?} failed with status {:?}",
        args,
        status
    );
}

fn git_commit(dir: &Path, message: &str) {
    let status = StdCommand::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Codex Tests")
        .env("GIT_AUTHOR_EMAIL", "codex-tests@example.com")
        .env("GIT_COMMITTER_NAME", "Codex Tests")
        .env("GIT_COMMITTER_EMAIL", "codex-tests@example.com")
        .status()
        .expect("run git commit");
    assert!(
        status.success(),
        "git commit failed with status {:?}",
        status
    );
}

fn init_repo_with_feature_branch(path: &Path) {
    git(path, &["init"]);
    git(path, &["config", "user.email", "codex-tests@example.com"]);
    git(path, &["config", "user.name", "Codex Tests"]);

    fs::write(path.join("main.txt"), "main branch").expect("write main file");
    git(path, &["add", "main.txt"]);
    git_commit(path, "initial commit");

    git(path, &["checkout", "-b", "feature"]);
    fs::write(path.join("feature.txt"), "feature branch").expect("write feature file");
    git(path, &["add", "feature.txt"]);
    git_commit(path, "feature commit");
}

fn find_unused_pid() -> i32 {
    let mut candidate: i32 = 100_000;
    while candidate < 1_000_000 {
        let result = unsafe { libc::kill(candidate, 0) };
        if result == -1 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::ESRCH || code == libc::EINVAL => {
                    return candidate;
                }
                _ => {}
            }
        }
        candidate += 1;
    }
    panic!("failed to find unused pid for tests");
}

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
    let pid = i32::try_from(std::process::id()).expect("pid fits in i32");
    fs::write(active_dir.join("task.pid"), pid.to_string()).expect("write pid");

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
    cmd.env("HOME", home.path()).args(["ls", "--all"]);
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
fn ls_excludes_archived_by_default() {
    let home = tempdir().expect("tempdir");
    let task_root = home.path().join(".codex").join("tasks");
    let archive_root = task_root.join("archive");
    fs::create_dir_all(&archive_root).expect("layout");

    let active_id = "task-active";
    write_metadata(&task_root, active_id, "IDLE");

    let archived_id = "task-archived";
    let archived_dir = archive_root
        .join("2024")
        .join("03")
        .join("04")
        .join(archived_id);
    fs::create_dir_all(&archived_dir).expect("archive dir");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 3, 4, 5, 6, 7)
        .single()
        .expect("timestamp");
    let archived_metadata = json!({
        "id": archived_id,
        "state": "ARCHIVED",
        "created_at": timestamp.to_rfc3339(),
        "updated_at": timestamp.to_rfc3339(),
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
        .stdout(predicates::str::contains(active_id))
        .stdout(predicates::str::contains(archived_id).not())
        .stdout(predicates::str::contains("ARCHIVE").not());
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
    let pid = i32::try_from(std::process::id()).expect("pid fits in i32");
    fs::write(running_dir.join("task.pid"), pid.to_string()).expect("write pid");

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
        .args(["ls", "--all", "--state", "RUNNING"]);
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
fn ls_accepts_multiple_states() {
    let home = tempdir().expect("tempdir");
    let task_root = home.path().join(".codex").join("tasks");
    let archive_root = task_root.join("archive");
    fs::create_dir_all(&archive_root).expect("layout");

    write_metadata(&task_root, "task-idle", "IDLE");
    write_metadata(&task_root, "task-running", "RUNNING");
    let pid = i32::try_from(std::process::id()).expect("pid fits");
    fs::write(
        task_root.join("task-running").join("task.pid"),
        pid.to_string(),
    )
    .expect("write pid");

    let archived_dir = archive_root
        .join("2024")
        .join("05")
        .join("06")
        .join("task-archived");
    fs::create_dir_all(&archived_dir).expect("archive dir");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let metadata = json!({
        "id": "task-archived",
        "state": "ARCHIVED",
        "created_at": timestamp.to_rfc3339(),
        "updated_at": timestamp.to_rfc3339(),
    });
    fs::write(
        archived_dir.join("task.json"),
        serde_json::to_string_pretty(&metadata).expect("serialize"),
    )
    .expect("write archive metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path())
        .args(["ls", "--all", "--state", "RUNNING,ARCHIVED"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("task-running"))
        .stdout(predicates::str::contains("task-archived"))
        .stdout(predicates::str::contains("task-idle").not());
}

#[test]
fn ls_reports_idle_with_live_worker() {
    let home = tempdir().expect("tempdir");
    let task_root = home.path().join(".codex").join("tasks");
    fs::create_dir_all(&task_root).expect("layout");

    let task_id = "task-idle";
    write_metadata(&task_root, task_id, "IDLE");
    let pid = i32::try_from(std::process::id()).expect("pid fits in i32");
    fs::write(task_root.join(task_id).join("task.pid"), pid.to_string()).expect("write pid");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home.path()).args(["ls", "--state", "IDLE"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("task-idle"))
        .stdout(predicates::str::contains("IDLE"))
        .stdout(predicates::str::contains("RUNNING").not());
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
fn start_without_prompt_sets_idle_state() {
    let tmp = tempdir().expect("tempdir");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .env("HOME", tmp.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    let task_id = output.trim();
    assert!(!task_id.is_empty(), "start should print the task id");

    let store_root = tmp.path().join(".codex").join("tasks");
    let metadata_path = store_root.join(task_id).join("task.json");
    let metadata: Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("read metadata"))
            .expect("parse metadata");
    assert_eq!(metadata["state"], "IDLE");
}

#[test]
fn start_requires_working_dir_when_repo_specified() {
    let home = tempdir().expect("tempdir");
    let source_repo = tempdir().expect("tempdir");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .arg("--repo")
        .arg(source_repo.path())
        .env("HOME", home.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("--working-dir"));
}

#[test]
fn start_with_repo_ref_clones_branch_into_working_dir() {
    let home = tempdir().expect("tempdir");
    let repo_src = tempdir().expect("tempdir");
    init_repo_with_feature_branch(repo_src.path());

    let working_dir = home.path().join("workspace").join("cloned");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .arg("--title")
        .arg("Repo Task")
        .arg("--working-dir")
        .arg(&working_dir)
        .arg("--repo")
        .arg(repo_src.path())
        .arg("--repo-ref")
        .arg("feature")
        .env("HOME", home.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    let assert = cmd.assert().success();
    let task_id = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(!task_id.trim().is_empty(), "start should print a task id");

    assert!(
        working_dir.join("feature.txt").exists(),
        "feature file should exist after cloning"
    );
    let head_path = working_dir.join(".git").join("HEAD");
    let head = fs::read_to_string(&head_path).expect("read head");
    assert!(
        head.contains("feature"),
        "expected HEAD to reference feature branch, got {head}"
    );
}

#[test]
fn start_rejects_custom_config_with_wrong_filename() {
    let home = tempdir().expect("tempdir");
    let config_dir = home.path().join("config");
    fs::create_dir_all(&config_dir).expect("config dir");
    let config_path = config_dir.join("custom.toml");
    fs::write(&config_path, "model = \"o3\"").expect("write config");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .arg("--config-file")
        .arg(&config_path)
        .env("HOME", home.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    cmd.assert().failure().stderr(predicates::str::contains(
        "custom config file must be named",
    ));
}

#[test]
fn start_clones_local_repo_using_relative_path() {
    let home = tempdir().expect("tempdir");
    let repo_dir = home.path().join("local_repo");
    fs::create_dir_all(&repo_dir).expect("repo dir");
    init_repo_with_feature_branch(&repo_dir);

    let working_dir = home.path().join("workspace").join("clone");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.current_dir(home.path())
        .arg("start")
        .arg("--working-dir")
        .arg(&working_dir)
        .arg("--repo")
        .arg("./local_repo")
        .env("HOME", home.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    let assert = cmd.assert().success();
    let task_id = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(!task_id.trim().is_empty(), "start should print a task id");

    assert!(
        working_dir.join("feature.txt").exists(),
        "feature file should be present in cloned repo"
    );
}

#[test]
fn start_accepts_custom_config_named_config_toml() {
    let home = tempdir().expect("tempdir");
    let config_dir = home.path().join("config");
    fs::create_dir_all(&config_dir).expect("config dir");
    let config_path = config_dir.join("config.toml");
    fs::write(&config_path, "model = \"o3\"").expect("write config");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start")
        .arg("--config-file")
        .arg(&config_path)
        .env("HOME", home.path())
        .env("CODEX_TASKS_EXIT_AFTER_START", "1");
    cmd.assert().success();
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

    let metadata_path = tasks_dir.join(task_id).join("task.json");
    let metadata: Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).expect("read metadata"))
            .expect("parse metadata");
    assert_eq!(metadata["state"], "RUNNING");
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
fn stop_all_stops_idle_tasks() {
    let env = IntegrationTestEnv::new();
    let first = env.start_task("Idle One", "prompt one");
    let second = env.start_task("Idle Two", "prompt two");

    env.wait_for_condition(&first, |value| value["state"] == "IDLE");
    env.wait_for_condition(&second, |value| value["state"] == "IDLE");

    let mut cmd = env.command();
    let assert = cmd.args(["stop", "-a"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(stdout.contains(&format!("Task {} stopped.", first)));
    assert!(stdout.contains(&format!("Task {} stopped.", second)));
    assert!(stdout.contains("Stopped 2 idle task(s); 0 already stopped."));

    env.wait_for_condition(&first, |value| value["state"] == "STOPPED");
    env.wait_for_condition(&second, |value| value["state"] == "STOPPED");
}

#[test]
fn stop_all_reports_when_no_idle_tasks_found() {
    let tmp = tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", tmp.path()).args(["stop", "--all"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("No idle tasks to stop."));
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
    let pid = find_unused_pid();
    fs::write(task_dir.join("task.pid"), pid.to_string()).expect("pid");
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
    let pid = i32::try_from(std::process::id()).expect("pid fits");
    fs::write(task_dir.join("task.pid"), pid.to_string()).expect("write pid");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.env("HOME", home)
        .args(["archive", task_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "task task-running is RUNNING; stop it before archiving",
        ));
}

#[test]
fn archive_all_archives_stopped_and_died_tasks() {
    let tmp = tempdir().expect("tempdir");
    let home = tmp.path();
    let tasks_dir = home.join(".codex").join("tasks");
    fs::create_dir_all(&tasks_dir).expect("tasks dir");

    write_metadata(&tasks_dir, "task-stopped", "STOPPED");
    fs::write(
        tasks_dir.join("task-stopped").join("task.result"),
        "stopped result",
    )
    .expect("write result");

    write_metadata(&tasks_dir, "task-died", "DIED");

    write_metadata(&tasks_dir, "task-running", "RUNNING");
    let run_pid = i32::try_from(std::process::id()).expect("pid fits");
    fs::write(
        tasks_dir.join("task-running").join("task.pid"),
        run_pid.to_string(),
    )
    .expect("write pid");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    let assert = cmd
        .env("HOME", home)
        .args(["archive", "-a"])
        .assert()
        .success();

    let output = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(output.contains("Task task-stopped archived"));
    assert!(output.contains("Task task-died archived"));
    assert!(output.contains("Skipping task task-running"));

    let archive_root = tasks_dir.join("archive");
    let stopped_dir = find_task_directory(&archive_root, "task-stopped").expect("stopped dir");
    assert!(stopped_dir.join("task.json").exists());
    let died_dir = find_task_directory(&archive_root, "task-died").expect("died dir");
    assert!(died_dir.join("task.json").exists());

    assert!(tasks_dir.join("task-running").exists());
    assert!(!tasks_dir.join("task-stopped").exists());
    assert!(!tasks_dir.join("task-died").exists());
}

#[test]
fn end_to_end_task_lifecycle_flow() {
    let env = IntegrationTestEnv::new();

    let task_id = env.start_task("Lifecycle Task", "first prompt");

    let record = env.wait_for_condition(&task_id, |value| {
        value["state"] == "IDLE" && value["last_result"].as_str().is_some()
    });
    assert_eq!(record["last_result"], "response 1: first prompt");
    assert_eq!(record["last_prompt"], "first prompt");

    let mut send = env.command();
    send.args(["send", &task_id, "second prompt"]);
    send.assert().success();

    let record = env.wait_for_condition(&task_id, |value| {
        value["state"] == "IDLE" && value["last_result"] == "response 2: second prompt"
    });
    assert_eq!(record["last_prompt"], "second prompt");

    let mut status = env.command();
    status.args(["status", "--json", &task_id]);
    let output = status.assert().success().get_output().stdout.clone();
    let value: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(value["state"], "IDLE");
    assert_eq!(value["last_result"], "response 2: second prompt");
    assert_eq!(value["location"], "active");

    let mut log = env.command();
    log.args(["log", "-n", "20", &task_id]);
    let log_output =
        String::from_utf8(log.assert().success().get_output().stdout.clone()).expect("stdout utf8");
    assert!(log_output.contains("response 1: first prompt"));
    assert!(log_output.contains("response 2: second prompt"));

    let mut stop = env.command();
    stop.args(["stop", &task_id]);
    let stop_assert = stop.assert().success();
    let stop_output =
        String::from_utf8(stop_assert.get_output().stdout.clone()).expect("stdout utf8");
    assert!(stop_output.contains("stopped"));

    env.wait_for_condition(&task_id, |value| value["state"] == "STOPPED");

    let mut archive = env.command();
    archive.args(["archive", &task_id]);
    archive.assert().success();

    let archived = env.wait_for_condition(&task_id, |value| value["state"] == "ARCHIVED");
    assert_eq!(archived["location"], "archived");
    assert_eq!(archived["last_result"], "response 2: second prompt");
}

#[test]
fn status_reports_died_after_worker_killed() {
    let env = IntegrationTestEnv::with_delay(3000);

    let task_id = env.start_task("Fragile Task", "initial prompt");
    let pid = env.wait_for_pid(&task_id);

    let kill_result = unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(kill_result, 0, "failed to send SIGKILL to worker");

    let pid_path = env.tasks_root().join(&task_id).join("task.pid");
    fs::remove_file(&pid_path).expect("remove pid after crash");

    let died = env.wait_for_condition(&task_id, |value| value["state"] == "DIED");
    assert_eq!(died["location"], "active");
    assert_eq!(died["last_prompt"], "initial prompt");
}

const FAKE_CODEX_SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import os
import sys
import time

ROOT = os.path.abspath(os.environ.get("FAKE_CODEX_ROOT", "."))
DELAY_MS = int(os.environ.get("FAKE_CODEX_DELAY_MS", "0"))


def send(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()


send(
    {
        "id": "sub-0000000000",
        "msg": {
            "type": "session_configured",
            "session_id": "00000000-0000-0000-0000-000000000000",
            "model": "fake-model",
            "history_log_id": 0,
            "history_entry_count": 0,
            "rollout_path": os.path.join(ROOT, "rollout.json"),
        },
    }
)

turn = 0
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    data = json.loads(line)
    submission_id = data.get("id", "sub-unknown")
    op = data.get("op", {})
    op_type = op.get("type")
    if op_type == "user_input":
        parts = []
        for item in op.get("items", []):
            if item.get("type") == "text":
                parts.append(item.get("text", ""))
        prompt = " ".join(parts)
        turn += 1
        response = f"response {turn}: {prompt}"
        send({"id": submission_id, "msg": {"type": "task_started", "model_context_window": None}})
        if DELAY_MS > 0:
            time.sleep(DELAY_MS / 1000.0)
        send({"id": submission_id, "msg": {"type": "agent_message", "message": response}})
        send(
            {
                "id": submission_id,
                "msg": {"type": "task_complete", "last_agent_message": response},
            }
        )
    elif op_type == "shutdown":
        send({"id": submission_id, "msg": {"type": "shutdown_complete"}})
        break
    else:
        send(
            {
                "id": submission_id,
                "msg": {"type": "error", "message": f"unsupported op {op_type}"},
            }
        )
"#;

struct IntegrationTestEnv {
    home: TempDir,
    path: OsString,
    extra_envs: Vec<(String, String)>,
}

impl IntegrationTestEnv {
    fn new() -> Self {
        Self::with_optional_delay(None)
    }

    fn with_delay(delay_ms: u64) -> Self {
        Self::with_optional_delay(Some(delay_ms))
    }

    fn with_optional_delay(delay_ms: Option<u64>) -> Self {
        let home = tempdir().expect("tempdir");
        let bin_dir = home.path().join("bin");
        write_fake_codex(&bin_dir);

        let base_path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from(""));
        let mut path = OsString::new();
        path.push(bin_dir.as_os_str());
        path.push(":");
        path.push(&base_path);

        let mut extra_envs = vec![(
            "FAKE_CODEX_ROOT".to_string(),
            home.path().to_str().expect("home path utf8").to_string(),
        )];
        if let Some(delay) = delay_ms {
            extra_envs.push(("FAKE_CODEX_DELAY_MS".to_string(), delay.to_string()));
        }

        Self {
            home,
            path,
            extra_envs,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
        cmd.env("HOME", self.home.path());
        cmd.env("PATH", &self.path);
        for (key, value) in &self.extra_envs {
            cmd.env(key, value);
        }
        cmd
    }

    fn start_task(&self, title: &str, prompt: &str) -> String {
        let mut cmd = self.command();
        cmd.args(["start", "--title", title, prompt]);
        let assert = cmd.assert().success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
        stdout.trim().to_string()
    }

    fn wait_for_condition<F>(&self, task_id: &str, mut predicate: F) -> Value
    where
        F: FnMut(&Value) -> bool,
    {
        let start = Instant::now();
        loop {
            let value = self.status_json(task_id);
            if predicate(&value) {
                return value;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("timed out waiting for condition on task {}", task_id);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for_pid(&self, task_id: &str) -> i32 {
        let pid_path = self.tasks_root().join(task_id).join("task.pid");
        let start = Instant::now();
        loop {
            if let Ok(contents) = fs::read_to_string(&pid_path) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    return pid;
                }
            }
            if start.elapsed() > Duration::from_secs(5) {
                panic!("timed out waiting for pid file at {:?}", pid_path);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn status_json(&self, task_id: &str) -> Value {
        let mut cmd = self.command();
        cmd.args(["status", "--json", task_id]);
        let output = cmd.assert().success().get_output().stdout.clone();
        serde_json::from_slice(&output).expect("valid json")
    }

    fn tasks_root(&self) -> PathBuf {
        self.home.path().join(".codex").join("tasks")
    }
}

fn write_fake_codex(bin_dir: &Path) {
    fs::create_dir_all(bin_dir).expect("bin dir");
    let script_path = bin_dir.join("codex");
    fs::write(&script_path, FAKE_CODEX_SCRIPT).expect("write fake codex script");
    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set permissions");
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
    assert_eq!(value["last_prompt"], Value::Null);
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
    assert_eq!(value["last_prompt"], Value::Null);
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
    assert_eq!(value["last_prompt"], Value::Null);

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
    assert_eq!(value["last_prompt"], Value::Null);
}

#[test]
fn log_displays_entire_file() {
    let home = tempdir().expect("tempdir");
    let task_dir = home.path().join(".codex").join("tasks").join("task-123");
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
    let task_dir = home.path().join(".codex").join("tasks").join("task-abc");
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

#[test]
fn log_follow_exits_when_task_idle() {
    let home = tempdir().expect("tempdir");
    let task_dir = home.path().join(".codex").join("tasks").join("task-idle");
    fs::create_dir_all(&task_dir).expect("create dirs");
    let log_path = task_dir.join("task.log");
    fs::write(&log_path, b"first line\nsecond line\n").expect("write log");
    write_metadata(
        &home.path().join(".codex").join("tasks"),
        "task-idle",
        "IDLE",
    );

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    let assert = cmd
        .env("HOME", home.path())
        .args(["log", "-f", "task-idle"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert_eq!(stdout, "first line\nsecond line\n");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr utf8");
    assert!(
        stderr.contains("IDLE"),
        "expected stderr to mention idle completion, got: {stderr}"
    );
}

#[test]
fn log_follow_waits_for_log_file_creation() {
    let home = tempdir().expect("tempdir");
    let tasks_root = home.path().join(".codex").join("tasks");
    let task_id = "task-wait";
    let task_dir = tasks_root.join(task_id);
    fs::create_dir_all(&task_dir).expect("task dir");
    write_metadata(&tasks_root, task_id, "IDLE");

    let log_path = task_dir.join("task.log");
    let writer = thread::spawn({
        let log_path = log_path.clone();
        move || {
            thread::sleep(Duration::from_millis(200));
            fs::write(&log_path, b"line one\nline two\n").expect("write log contents");
        }
    });

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    let assert = cmd
        .env("HOME", home.path())
        .args(["log", "-f", task_id])
        .assert()
        .success();

    writer.join().expect("writer thread");

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert_eq!(stdout, "line one\nline two\n");
}

#[test]
fn log_follow_exits_for_archived_tasks() {
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
    let metadata_path = log_path.parent().expect("parent").join("task.json");
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&json!({
            "id": "task-archived",
            "state": "ARCHIVED",
            "created_at": "2024-01-02T03:04:05Z",
            "updated_at": "2024-01-02T03:04:05Z"
        }))
        .expect("metadata"),
    )
    .expect("write metadata");

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    let assert = cmd
        .env("HOME", home.path())
        .args(["log", "-f", "task-archived"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf8");
    assert_eq!(stdout, "archived\ncontent\n");
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr utf8");
    assert!(
        stderr.contains("ARCHIVED"),
        "expected stderr to mention archived completion, got: {stderr}"
    );
}

#[test]
fn log_forever_flag_waits_for_manual_interrupt() {
    let home = tempdir().expect("tempdir");
    let task_dir = home
        .path()
        .join(".codex")
        .join("tasks")
        .join("task-forever");
    fs::create_dir_all(&task_dir).expect("create dirs");
    let log_path = task_dir.join("task.log");
    fs::write(&log_path, b"initial\n").expect("write log");
    write_metadata(
        &home.path().join(".codex").join("tasks"),
        "task-forever",
        "IDLE",
    );

    let binary = cargo_bin(BIN);
    let mut child = StdCommand::new(binary)
        .env("HOME", home.path())
        .args(["log", "--forever", "task-forever"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn log --forever");

    thread::sleep(Duration::from_millis(300));
    let still_running = child.try_wait().expect("query status").is_none();
    assert!(
        still_running,
        "log --forever should continue running until interrupted"
    );

    child.kill().expect("kill log --forever");
    let _ = child.wait();
}
