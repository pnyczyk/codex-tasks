use anyhow::{Result, bail};

use crate::cli::ArchiveArgs;
use crate::services::tasks::{ArchiveAllSummary, ArchiveTaskOutcome, TaskService};

pub fn handle_archive(args: ArchiveArgs) -> Result<()> {
    let service = TaskService::with_default_store(false)?;

    if args.all {
        handle_archive_all(service.archive_all()?)
    } else {
        let task_id = args
            .task_id
            .expect("clap ensures task id is present when --all is absent");
        match service.archive_task(&task_id)? {
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

fn handle_archive_all(summary: ArchiveAllSummary) -> Result<()> {
    if summary.skipped.is_empty() && summary.archived.is_empty() && summary.already.is_empty() {
        println!("No STOPPED or DIED tasks were found to archive.");
        return Ok(());
    }

    for (id, state) in &summary.skipped {
        println!("Skipping task {} ({}).", id, state.as_str());
    }

    for (id, destination) in &summary.archived {
        println!("Task {} archived to {}.", id, destination.display());
    }

    for id in &summary.already {
        println!("Task {} is already archived.", id);
    }

    if !summary.failures.is_empty() {
        for (id, err) in &summary.failures {
            eprintln!("Failed to archive task {}: {err:#}", id);
        }
        bail!("failed to archive {} task(s)", summary.failures.len());
    }

    if summary.archived.is_empty() && summary.already.is_empty() {
        println!("No STOPPED or DIED tasks were archived.");
    }

    Ok(())
}
