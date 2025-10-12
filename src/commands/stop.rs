use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::cli::StopArgs;
use crate::commands::common::is_process_running;
use crate::commands::tasks::collect_active_tasks;
use crate::storage::TaskPaths;
use crate::storage::TaskStore;
use crate::task::TaskState;

pub fn handle_stop(args: StopArgs) -> Result<()> {
    let store = TaskStore::default()?;
    store.ensure_layout()?;
    if args.all {
        stop_all_idle_tasks(&store)
    } else {
        let task_id = args
            .task_id
            .expect("task id is required when --all is not specified");
        let paths = store.task(task_id.clone());
        let outcome = stop_task(&paths)?;
        print_stop_outcome(&task_id, &outcome);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopOutcome {
    AlreadyStopped,
    Stopped,
}

const SHUTDOWN_TIMEOUT_SECS: u64 = 10;
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 100;

pub fn stop_task(paths: &TaskPaths) -> Result<StopOutcome> {
    let pid = match paths.read_pid()? {
        Some(pid) => pid,
        None => return Ok(StopOutcome::AlreadyStopped),
    };

    if !is_process_running(pid)? {
        let _ = paths.remove_pid();
        return Ok(StopOutcome::AlreadyStopped);
    }

    send_signal(pid, libc::SIGTERM)?;
    wait_for_worker_shutdown(pid)?;
    let _ = paths.remove_pid();
    mark_task_state(paths, TaskState::Stopped)?;

    Ok(StopOutcome::Stopped)
}

fn stop_all_idle_tasks(store: &TaskStore) -> Result<()> {
    let mut running = Vec::new();
    for task in collect_active_tasks(store)? {
        let paths = store.task(task.metadata.id.clone());
        let pid = paths.read_pid()?;
        if let Some(pid) = pid {
            if is_process_running(pid)? {
                running.push(task.metadata.id.clone());
            }
        }
    }

    if running.is_empty() {
        println!("No running tasks to stop.");
        return Ok(());
    }

    let mut stopped = 0usize;
    let mut already = 0usize;

    for task_id in running {
        let paths = store.task(task_id.clone());
        let outcome = stop_task(&paths)?;
        print_stop_outcome(&task_id, &outcome);
        match outcome {
            StopOutcome::Stopped => stopped += 1,
            StopOutcome::AlreadyStopped => already += 1,
        }
    }

    println!(
        "Stopped {stopped} running task(s); {already} already stopped.",
        stopped = stopped,
        already = already
    );

    Ok(())
}

fn print_stop_outcome(task_id: &str, outcome: &StopOutcome) {
    match outcome {
        StopOutcome::AlreadyStopped => {
            println!("Task {} is not running; nothing to stop.", task_id);
        }
        StopOutcome::Stopped => {
            println!("Task {} stopped.", task_id);
        }
    }
}

fn wait_for_worker_shutdown(pid: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    loop {
        let mut status: libc::c_int = 0;
        let wait_result =
            unsafe { libc::waitpid(pid, &mut status as *mut libc::c_int, libc::WNOHANG) };
        if wait_result == pid {
            break;
        } else if wait_result == 0 {
            // child still running
        } else if wait_result == -1 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ECHILD) {
                if !is_process_running(pid)? {
                    break;
                }
            } else {
                return Err(err).with_context(|| format!("failed to wait for process {pid}"));
            }
        }

        if Instant::now() >= deadline {
            send_signal(pid, libc::SIGKILL)?;
            thread::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS));
            if !is_process_running(pid)? {
                break;
            }
            bail!("timed out waiting for worker {pid} to stop");
        }

        if !is_process_running(pid)? {
            break;
        }

        thread::sleep(Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS));
    }
    Ok(())
}

fn send_signal(pid: i32, signal: libc::c_int) -> Result<()> {
    if pid <= 0 {
        return Ok(());
    }

    let result = unsafe { libc::kill(pid, signal) };
    if result == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err).with_context(|| format!("failed to signal process {pid}"))
        }
    } else {
        Ok(())
    }
}

fn mark_task_state(paths: &TaskPaths, state: TaskState) -> Result<()> {
    match paths.update_metadata(|metadata| metadata.set_state(state)) {
        Ok(_) => Ok(()),
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound);
            if not_found { Ok(()) } else { Err(err) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskMetadata;
    use tempfile::tempdir;

    #[test]
    fn not_implemented_returns_err() {
        let err = crate::commands::not_implemented("start").unwrap_err();
        assert_eq!(
            "`start` is not implemented yet. Track progress in future issues.",
            err.to_string()
        );
    }

    #[test]
    fn stop_task_reports_already_stopped_when_pid_missing() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout().expect("layout");
        let paths = store.task("task-1".to_string());
        paths.ensure_directory().expect("directory");

        let outcome = stop_task(&paths).expect("stop task");
        assert_eq!(outcome, StopOutcome::AlreadyStopped);
    }

    #[test]
    fn stop_task_terminates_running_process() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout().expect("layout");
        let paths = store.task("task-2".to_string());
        paths.ensure_directory().expect("directory");

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let pid = i32::try_from(child.id()).expect("pid fits in i32");
        paths.write_pid(pid).expect("write pid");
        paths
            .write_metadata(&TaskMetadata::new(
                "task-2".into(),
                None,
                TaskState::Running,
            ))
            .expect("write metadata");

        let outcome = stop_task(&paths).expect("stop task");
        assert_eq!(outcome, StopOutcome::Stopped);

        let _ = child.wait();
        assert!(!paths.pid_path().exists());
    }
}
