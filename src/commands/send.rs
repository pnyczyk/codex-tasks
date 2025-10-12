use std::io;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::cli::SendArgs;
use crate::commands::common::is_process_running;
use crate::storage::TaskStore;
use crate::task::TaskState;
use crate::worker::launcher::{WorkerLaunchRequest, spawn_worker};

pub fn handle_send(args: SendArgs) -> Result<()> {
    let store = TaskStore::default().context("failed to locate task store")?;
    let task_id = args.task_id;
    let prompt = if args.prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    } else {
        args.prompt
    };

    let metadata = match store.load_metadata(task_id.clone()) {
        Ok(metadata) => metadata,
        Err(err) => {
            let not_found = err
                .downcast_ref::<io::Error>()
                .is_some_and(|io_err| io_err.kind() == ErrorKind::NotFound);
            if not_found {
                if let Some((_, archived_metadata)) = store.find_archived_task(&task_id)? {
                    bail!(
                        "task {} is ARCHIVED and cannot receive prompts",
                        archived_metadata.id
                    );
                }
                bail!("task {task_id} was not found");
            }
            return Err(err);
        }
    };

    match metadata.state {
        TaskState::Archived => {
            bail!(
                "task {} is ARCHIVED and cannot receive prompts",
                metadata.id
            )
        }
        TaskState::Died => {
            bail!("task {} has DIED and cannot receive prompts", metadata.id)
        }
        TaskState::Stopped | TaskState::Running => {}
    }

    let paths = store.task(metadata.id.clone());
    if let Some(pid) = paths.read_pid()? {
        if is_process_running(pid)? {
            bail!(
                "task {} is currently running; wait for completion or stop it first",
                metadata.id
            );
        }
        let _ = paths.remove_pid();
    }

    let store_root = store.root().to_path_buf();
    let mut request = WorkerLaunchRequest::new(store_root, prompt);
    request.task_id = Some(metadata.id.clone());
    request.title = metadata.title.clone();
    if let Some(path) = metadata.config_path.as_ref() {
        request.config_path = Some(PathBuf::from(path));
    }
    if let Some(dir) = metadata.working_dir.as_ref() {
        request.working_directory = Some(PathBuf::from(dir));
    }

    let mut child = spawn_worker(request).context("failed to launch worker process")?;
    if let Some(stdout) = child.stdout.take() {
        drop(stdout);
    }
    drop(child);

    Ok(())
}
