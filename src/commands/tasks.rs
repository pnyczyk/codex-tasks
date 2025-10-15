use std::collections::VecDeque;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::tasks::{METADATA_FILE_NAME, TaskMetadata, TaskStore, derive_active_state};

#[derive(Debug)]
pub(crate) struct ListedTask {
    pub(crate) metadata: TaskMetadata,
}

pub(crate) fn collect_active_tasks(store: &TaskStore) -> Result<Vec<ListedTask>> {
    let mut tasks = Vec::new();
    let root = store.root().to_path_buf();
    if !root.exists() {
        return Ok(tasks);
    }

    for entry in fs::read_dir(&root)
        .with_context(|| format!("failed to read task directory {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let metadata_path = path.join(METADATA_FILE_NAME);
        if !metadata_path.exists() {
            continue;
        }

        let mut metadata = read_metadata_file(&metadata_path)?;
        let task_paths = store.task(metadata.id.clone());
        let pid = task_paths.read_pid()?;
        metadata.state = derive_active_state(&metadata.state, pid);
        if metadata.last_result.is_none() {
            metadata.last_result = task_paths.read_last_result()?;
        }
        tasks.push(ListedTask { metadata });
    }

    Ok(tasks)
}

pub(crate) fn collect_archived_tasks(store: &TaskStore) -> Result<Vec<ListedTask>> {
    let mut tasks = Vec::new();
    let archive_root = store.archive_root();
    if !archive_root.exists() {
        return Ok(tasks);
    }

    let mut queue = VecDeque::from([archive_root]);
    while let Some(dir) = queue.pop_front() {
        let metadata_path = dir.join(METADATA_FILE_NAME);
        if metadata_path.exists() {
            let metadata = read_metadata_file(&metadata_path)?;
            tasks.push(ListedTask { metadata });
            continue;
        }

        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read archive directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read archive entry in {}", dir.display()))?;
            if entry.file_type()?.is_dir() {
                queue.push_back(entry.path());
            }
        }
    }

    Ok(tasks)
}

pub(crate) fn read_metadata_file(path: &Path) -> Result<TaskMetadata> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read metadata file {}", path.display()))?;
    let metadata: TaskMetadata = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse metadata file {}", path.display()))?;

    if let Some(parent) = path
        .parent()
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
    {
        ensure!(
            metadata.id == parent,
            "metadata id {} does not match directory {}",
            metadata.id,
            parent
        );
    }

    Ok(metadata)
}
