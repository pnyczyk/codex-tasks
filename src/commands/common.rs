use std::io;

use anyhow::{Context, Result};

pub(crate) fn is_process_running(pid: i32) -> Result<bool> {
    if pid <= 0 {
        return Ok(false);
    }

    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return Ok(true);
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(false),
        Some(code) if code == libc::EPERM => Ok(true),
        _ => Err(err).with_context(|| format!("failed to query status of process {pid}")),
    }
}
