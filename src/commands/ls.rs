use anyhow::Result;

use crate::cli::LsArgs;
use crate::commands::tasks::{collect_active_tasks, collect_archived_tasks};
use crate::timefmt::format_unix_style;

pub fn handle_ls(args: LsArgs) -> Result<()> {
    let store = crate::storage::TaskStore::default()?;
    store.ensure_layout()?;

    let include_archived = args.include_archived;
    let mut tasks = Vec::new();
    tasks.extend(collect_active_tasks(&store)?);
    if include_archived {
        tasks.extend(collect_archived_tasks(&store)?);
    }

    let states = args.states;
    if !states.is_empty() {
        tasks.retain(|task| states.contains(&task.metadata.state));
    }

    tasks.sort_by(|a, b| b.metadata.updated_at.cmp(&a.metadata.updated_at));

    if tasks.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }

    println!(
        "{:<36}  {:<20}  {:<10}  {:<25}  {:<25}  {}",
        "ID", "Title", "State", "Created At", "Updated At", "Working Dir"
    );
    for entry in tasks {
        let title = entry.metadata.title.as_deref().unwrap_or("-");
        let created = format_unix_style(entry.metadata.created_at);
        let updated = format_unix_style(entry.metadata.updated_at);
        let working_dir = entry.metadata.working_dir.as_deref().unwrap_or("-");
        println!(
            "{:<36}  {:<20}  {:<10}  {:<25}  {:<25}  {}",
            entry.metadata.id, title, entry.metadata.state, created, updated, working_dir
        );
    }

    Ok(())
}
