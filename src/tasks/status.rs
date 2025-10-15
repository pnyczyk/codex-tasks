use crate::tasks::TaskState;

/// Derives the effective task state by combining stored metadata with the worker PID (if any).
pub fn derive_active_state(metadata_state: &TaskState, pid: Option<i32>) -> TaskState {
    if let Some(pid) = pid {
        if is_process_running(pid) {
            return match metadata_state {
                TaskState::Running => TaskState::Running,
                TaskState::Stopped => TaskState::Stopped,
                TaskState::Archived => TaskState::Archived,
                TaskState::Died => TaskState::Running,
            };
        }
    }
    derive_state_without_pid(metadata_state.clone())
}

fn derive_state_without_pid(metadata_state: TaskState) -> TaskState {
    match metadata_state {
        TaskState::Running => TaskState::Died,
        other => other,
    }
}

fn is_process_running(pid: i32) -> bool {
    // SAFETY: libc::kill is called with signal 0 which performs error checking without
    // delivering a signal to the target process.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }

    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => true,
        Some(libc::ESRCH) | None => false,
        _ => false,
    }
}
