#![cfg(unix)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use std::os::unix::fs::PermissionsExt;

const STUB_SCRIPT: &str = r"#!/usr/bin/env python3
import json
import sys

def emit(event):
    sys.stdout.write(json.dumps(event) + '\n')
    sys.stdout.flush()

emit({
    'id': 'bootstrap',
    'msg': {
        'type': 'session_configured',
        'session_id': '00000000-0000-0000-0000-000000000000',
        'model': 'stub-model',
        'history_log_id': 0,
        'history_entry_count': 0,
        'rollout_path': '/tmp/stub-rollout'
    }
})

for raw_line in sys.stdin:
    raw_line = raw_line.strip()
    if not raw_line:
        continue
    submission = json.loads(raw_line)
    op = submission.get('op', {})
    op_type = op.get('type')
    if op_type == 'user_input':
        prompt_parts = []
        for item in op.get('items', []):
            if item.get('type') == 'text':
                prompt_parts.append(item.get('text', ''))
        prompt = ' '.join(prompt_parts).strip()
        response = f'Echo: {prompt}' if prompt else 'Echo: (empty)'
        emit({'id': submission.get('id'), 'msg': {'type': 'task_started', 'model_context_window': None}})
        emit({'id': submission.get('id'), 'msg': {'type': 'agent_message', 'message': response}})
        emit({'id': submission.get('id'), 'msg': {'type': 'task_complete', 'last_agent_message': response}})
    elif op_type == 'shutdown':
        emit({'id': submission.get('id'), 'msg': {'type': 'shutdown_complete'}})
        break
sys.stdout.flush()
";

struct TestEnv {
    _temp: TempDir,
    codex_home: PathBuf,
    path: String,
}

impl TestEnv {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let stub_dir = temp.path().join("bin");
        fs::create_dir(&stub_dir).expect("create stub dir");

        let codex_home = temp.path().join("codex-home");
        fs::create_dir(&codex_home).expect("create codex home");

        let stub_path = stub_dir.join("codex");
        fs::write(&stub_path, STUB_SCRIPT).expect("write stub codex");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&stub_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&stub_path, perms).expect("chmod");
        }

        let original_path = env::var("PATH").unwrap_or_default();
        let path = if original_path.is_empty() {
            stub_dir.display().to_string()
        } else {
            format!("{}:{}", stub_dir.display(), original_path)
        };

        Self {
            _temp: temp,
            codex_home,
            path,
        }
    }
}

fn spawn_codex_task(env: &TestEnv) -> Child {
    let binary = assert_cmd::cargo::cargo_bin("codex-task");
    std::process::Command::new(binary)
        .env("PATH", &env.path)
        .env("CODEX_HOME", &env.codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn codex-task")
}

fn assert_success(child: std::process::Child) -> std::process::Output {
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn exits_gracefully_after_eof() {
    let env = TestEnv::new();
    let mut child = spawn_codex_task(&env);
    drop(child.stdin.take());
    assert_success(child);
}

#[test]
fn exits_gracefully_after_quit() {
    let env = TestEnv::new();
    let mut child = spawn_codex_task(&env);
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"/quit\n").expect("write quit");
    }
    assert_success(child);
}

#[test]
fn responds_to_prompt_then_eof() {
    let env = TestEnv::new();
    let mut child = spawn_codex_task(&env);
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"Hi there!\n").expect("write");
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = assert_success(child);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Echo: Hi there!"));
}

#[test]
fn responds_to_prompt_then_quit() {
    let env = TestEnv::new();
    let mut child = spawn_codex_task(&env);
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"Hi there!\n").expect("write");
        std::thread::sleep(Duration::from_millis(100));
        stdin.write_all(b"/quit\n").expect("write quit");
    }
    let output = assert_success(child);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Echo: Hi there!"));
}
