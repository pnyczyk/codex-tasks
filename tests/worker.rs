use std::fs;
use std::io::BufRead;

use assert_cmd::cargo::cargo_bin;
use tempfile::tempdir;

const BIN: &str = "codex-tasks";

#[path = "util.rs"]
mod util;

#[test]
fn worker_subcommand_writes_pid_file() {
    let tmp = tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    util::write_fake_codex(&bin_dir);

    let mut path_value = std::ffi::OsString::new();
    path_value.push(bin_dir.as_os_str());
    path_value.push(":");
    path_value.push(std::env::var_os("PATH").unwrap_or_else(|| std::ffi::OsString::from("")));

    let store_root = tmp.path().join(".codex").join("tasks");
    let binary = cargo_bin(BIN);

    let mut child = std::process::Command::new(&binary)
        .arg("worker")
        .arg("--store-root")
        .arg(&store_root)
        .arg("--prompt")
        .arg("Integration Prompt")
        .arg("--title")
        .arg("Integration Title")
        .env("PATH", &path_value)
        .env("HOME", tmp.path())
        .env("FAKE_CODEX_ROOT", tmp.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn worker");

    let stdout = child
        .stdout
        .take()
        .expect("worker should expose stdout for handshake");
    let mut reader = std::io::BufReader::new(stdout);
    let mut thread_id = String::new();
    reader
        .read_line(&mut thread_id)
        .expect("read thread id from worker stdout");
    let thread_id = thread_id.trim().to_string();
    assert!(!thread_id.is_empty(), "worker must output thread id");

    let task_dir = store_root.join(&thread_id);
    let pid_path = task_dir.join("task.pid");
    let start = std::time::Instant::now();
    while !pid_path.exists() {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            panic!("pid file was not created at {:?}", pid_path);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let contents = fs::read_to_string(&pid_path).expect("pid file should exist");
    let value = contents
        .trim()
        .parse::<i32>()
        .expect("pid file should contain an integer");
    assert!(value > 0, "pid should be positive");

    let status = child.wait().expect("wait for worker to exit");
    assert!(status.success(), "worker exit status {status:?}");

    let start = std::time::Instant::now();
    while pid_path.exists() {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            panic!("pid file should be removed after worker exits");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
