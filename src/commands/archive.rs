use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::cli::ArchiveArgs;
use crate::commands::common::is_process_running;
use crate::commands::tasks::collect_active_tasks;
use crate::status::derive_active_state;
use crate::storage::TaskStore;
use crate::task::TaskState;

pub fn handle_archive(args: ArchiveArgs) -> Result<()> {
    let store = TaskStore::default()?;
    store.ensure_layout()?;

    if args.all {
        archive_all_tasks(&store)
    } else {
        let task_id = args
            .task_id
            .expect("clap ensures task id is present when --all is absent");
        match archive_task(&store, &task_id)? {
            ArchiveTaskOutcome::Archived { id, destination } => {
                println!("Task {} archived to {}.", id, destination.display());
            }
            ArchiveTaskOutcome::AlreadyArchived { id } => {
                println!("Task {} is already archived.", id);
            }
        }
        Ok(())
    }
}

enum ArchiveTaskOutcome {
    Archived {
        id: String,
        destination: std::path::PathBuf,
    },
    AlreadyArchived {
        id: String,
    },
}

fn archive_task(store: &TaskStore, task_id: &str) -> Result<ArchiveTaskOutcome> {
    if let Some((_, metadata)) = store.find_archived_task(task_id)? {
        return Ok(ArchiveTaskOutcome::AlreadyArchived { id: metadata.id });
    }

    let paths = store.task(task_id.to_string());
    let mut metadata = match paths.read_metadata() {
        Ok(metadata) => metadata,
        Err(err) => {
            let not_found = err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound);
            if not_found {
                bail!("task {task_id} was not found");
            }
            return Err(err);
        }
    };

    let pid = paths.read_pid()?;
    let derived_state = derive_active_state(&metadata.state, pid);
    if metadata.state != derived_state {
        metadata.set_state(derived_state.clone());
        paths.write_metadata(&metadata)?;
    }

    if derived_state == TaskState::Running {
        bail!("task {} is RUNNING; stop it before archiving", metadata.id);
    }

    if let Some(pid) = pid {
        if is_process_running(pid)? {
            bail!("task {} is RUNNING; stop it before archiving", metadata.id);
        }
    }

    paths.remove_pid()?;
    paths.remove_pipe()?;

    let now = Utc::now();
    metadata.state = TaskState::Archived;
    metadata.updated_at = now;
    paths.write_metadata(&metadata)?;

    let bucket = store.ensure_archive_bucket(now)?;
    let destination = bucket.join(&metadata.id);
    if destination.exists() {
        bail!(
            "archive destination {} already exists for task {}",
            destination.display(),
            metadata.id
        );
    }

    std::fs::rename(paths.directory(), &destination).with_context(|| {
        format!(
            "failed to move task {} into archive at {}",
            metadata.id,
            destination.display()
        )
    })?;

    Ok(ArchiveTaskOutcome::Archived {
        id: metadata.id,
        destination,
    })
}

fn archive_all_tasks(store: &TaskStore) -> Result<()> {
    let tasks = collect_active_tasks(store)?;

    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    for task in tasks {
        match task.metadata.state {
            TaskState::Stopped | TaskState::Died => {
                candidates.push(task.metadata.id.clone());
            }
            TaskState::Running => {
                skipped.push((task.metadata.id.clone(), task.metadata.state));
            }
            TaskState::Archived => {}
        }
    }

    if candidates.is_empty() {
        for (id, state) in skipped {
            println!("Skipping task {} ({}).", id, state.as_str());
        }
        println!("No STOPPED or DIED tasks were found to archive.");
        return Ok(());
    }

    for (id, state) in skipped {
        println!("Skipping task {} ({}).", id, state.as_str());
    }

    let mut archived = Vec::new();
    let mut already = Vec::new();
    let mut failures = Vec::new();

    for task_id in candidates {
        match archive_task(store, &task_id) {
            Ok(ArchiveTaskOutcome::Archived { id, destination }) => {
                println!("Task {} archived to {}.", id, destination.display());
                archived.push(id);
            }
            Ok(ArchiveTaskOutcome::AlreadyArchived { id }) => {
                println!("Task {} is already archived.", id);
                already.push(id);
            }
            Err(err) => {
                eprintln!("Failed to archive task {}: {err:#}", task_id);
                failures.push(task_id);
            }
        }
    }

    if !failures.is_empty() {
        bail!("failed to archive {} task(s)", failures.len());
    }

    if archived.is_empty() && already.is_empty() {
        println!("No STOPPED or DIED tasks were archived.");
    }

    Ok(())
}
