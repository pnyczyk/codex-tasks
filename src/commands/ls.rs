use std::io::Write;

use anyhow::Result;
use tabwriter::TabWriter;

use crate::cli::LsArgs;
use crate::services::tasks::{ListTasksOptions, TaskService};
use crate::timefmt::format_time;

pub fn handle_ls(args: LsArgs) -> Result<()> {
    let service = TaskService::with_default_store(false)?;
    let tasks = service.list_tasks(ListTasksOptions {
        include_archived: args.include_archived,
        states: args.states.clone(),
    })?;

    if tasks.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }

    let time_format = args.time_format;

    let mut buffer = Vec::new();
    {
        let mut writer = TabWriter::new(&mut buffer).padding(2);
        writeln!(
            &mut writer,
            "ID\tTitle\tState\tCreated At\tUpdated At\tWorking Dir"
        )?;
        for entry in tasks {
            let title = entry.metadata.title.as_deref().unwrap_or("-");
            let created = format_time(entry.metadata.created_at, time_format);
            let updated = format_time(entry.metadata.updated_at, time_format);
            let working_dir = entry.metadata.working_dir.as_deref().unwrap_or("-");
            writeln!(
                &mut writer,
                "{}\t{}\t{}\t{}\t{}\t{}",
                entry.metadata.id, title, entry.metadata.state, created, updated, working_dir
            )?;
        }
        writer.flush()?;
    }

    print!("{}", String::from_utf8(buffer)?);

    Ok(())
}
