use std::collections::VecDeque;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, Utc};
use dirs::home_dir;
use tempfile::NamedTempFile;

use crate::tasks::{TaskId, TaskMetadata};

const ARCHIVE_DIR_NAME: &str = "archive";

/// Canonical filenames for task artifacts stored on disk.
pub const METADATA_FILE_NAME: &str = "task.json";
pub const PID_FILE_NAME: &str = "task.pid";
pub const PIPE_FILE_NAME: &str = "task.pipe";
pub const LOG_FILE_NAME: &str = "task.log";
pub const RESULT_FILE_NAME: &str = "task.result";

/// Rooted view into the filesystem layout backing Codex tasks.
#[derive(Clone, Debug)]
pub struct TaskStore {
    root: PathBuf,
}

impl TaskStore {
    /// Creates a new store rooted at the provided path.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Returns a store rooted at the default `~/.codex/tasks` directory.
    pub fn default() -> Result<Self> {
        let home = home_dir().context("failed to locate home directory")?;
        Ok(Self::new(home.join(".codex").join("tasks")))
    }

    /// Location on disk where active task files are stored.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory containing archived tasks.
    pub fn archive_root(&self) -> PathBuf {
        self.root.join(ARCHIVE_DIR_NAME)
    }

    /// Ensures the primary directories required by the store exist.
    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(self.root())
            .with_context(|| format!("failed to create task root at {}", self.root.display()))?;
        let archive_root = self.archive_root();
        fs::create_dir_all(&archive_root).with_context(|| {
            format!(
                "failed to create archive root at {}",
                archive_root.display()
            )
        })?;
        Ok(())
    }

    /// Ensures the archive bucket for the provided timestamp exists.
    pub fn ensure_archive_bucket(&self, timestamp: DateTime<Utc>) -> Result<PathBuf> {
        let bucket = self.archive_bucket(timestamp);
        fs::create_dir_all(&bucket)
            .with_context(|| format!("failed to create archive bucket at {}", bucket.display()))?;
        Ok(bucket)
    }

    fn archive_bucket(&self, timestamp: DateTime<Utc>) -> PathBuf {
        let date = timestamp.date_naive();
        self.archive_root()
            .join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()))
    }

    /// Ensures the archive directory for a specific task exists and returns it.
    pub fn ensure_archive_task_dir(
        &self,
        timestamp: DateTime<Utc>,
        task_id: &TaskId,
    ) -> Result<PathBuf> {
        let dir = self.archive_bucket(timestamp).join(task_id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create archive directory for task {}", task_id))?;
        Ok(dir)
    }

    /// Returns helpers for interacting with an active task's files.
    pub fn task(&self, task_id: impl Into<TaskId>) -> TaskPaths {
        let id = task_id.into();
        let directory = self.root.join(&id);
        TaskPaths::from_directory(directory, id)
    }

    /// Returns helpers for interacting with an archived task's files.
    pub fn archived_task(&self, timestamp: DateTime<Utc>, task_id: impl Into<TaskId>) -> TaskPaths {
        let id = task_id.into();
        let dir = self.archive_bucket(timestamp).join(&id);
        TaskPaths::from_directory(dir, id)
    }

    /// Writes metadata to disk using the standard layout.
    pub fn save_metadata(&self, metadata: &TaskMetadata) -> Result<()> {
        self.task(metadata.id.clone()).write_metadata(metadata)
    }

    /// Loads metadata for the provided task identifier.
    pub fn load_metadata(&self, task_id: impl Into<TaskId>) -> Result<TaskMetadata> {
        let id = task_id.into();
        self.task(id).read_metadata()
    }

    /// Attempts to locate an archived task by identifier, returning its paths and metadata.
    pub fn find_archived_task(&self, task_id: &str) -> Result<Option<(TaskPaths, TaskMetadata)>> {
        let archive_root = self.archive_root();
        if !archive_root.exists() {
            return Ok(None);
        }

        let mut queue = VecDeque::from([archive_root]);
        while let Some(dir) = queue.pop_front() {
            if dir
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name == task_id)
            {
                let paths = TaskPaths::from_directory(dir.clone(), task_id.to_string());
                let metadata = paths.read_metadata()?;
                return Ok(Some((paths, metadata)));
            }

            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("failed to read archive directory {}", dir.display())
                    });
                }
            };

            for entry in entries {
                let entry = entry.with_context(|| {
                    format!("failed to inspect archive entry in {}", dir.display())
                })?;
                if entry.file_type()?.is_dir() {
                    queue.push_back(entry.path());
                }
            }
        }

        Ok(None)
    }
}

/// Helper for working with the files associated with a particular task.
#[derive(Clone, Debug)]
pub struct TaskPaths {
    base: PathBuf,
    task_id: TaskId,
}

impl TaskPaths {
    fn new(base: PathBuf, task_id: TaskId) -> Self {
        Self { base, task_id }
    }

    /// Creates a helper for an existing task directory and identifier.
    pub fn from_directory(directory: PathBuf, task_id: TaskId) -> Self {
        Self::new(directory, task_id)
    }

    /// Returns the identifier associated with these paths.
    pub fn id(&self) -> &str {
        &self.task_id
    }

    /// Returns the directory that contains the task's files.
    pub fn directory(&self) -> &Path {
        &self.base
    }

    fn file_path(&self, file_name: &str) -> PathBuf {
        self.base.join(file_name)
    }

    /// Location of the PID file for the task.
    pub fn pid_path(&self) -> PathBuf {
        self.file_path(PID_FILE_NAME)
    }

    /// Location of the FIFO used for sending prompts to the worker.
    pub fn pipe_path(&self) -> PathBuf {
        self.file_path(PIPE_FILE_NAME)
    }

    /// Location where the worker writes the transcript log.
    pub fn log_path(&self) -> PathBuf {
        self.file_path(LOG_FILE_NAME)
    }

    /// Location that stores the most recent Codex result.
    pub fn result_path(&self) -> PathBuf {
        self.file_path(RESULT_FILE_NAME)
    }

    /// Location of the structured metadata file.
    pub fn metadata_path(&self) -> PathBuf {
        self.file_path(METADATA_FILE_NAME)
    }

    fn ensure_parent(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to prepare directory {}", parent.display()))?;
        }
        Ok(())
    }

    /// Persists structured metadata for the task to disk.
    pub fn write_metadata(&self, metadata: &TaskMetadata) -> Result<()> {
        ensure!(
            metadata.id == self.task_id,
            "metadata id {} does not match path {}",
            metadata.id,
            self.task_id
        );
        let path = self.metadata_path();
        self.ensure_parent(&path)?;
        let payload = serde_json::to_vec_pretty(metadata)
            .with_context(|| format!("failed to serialize metadata for task {}", self.task_id))?;
        let parent = path
            .parent()
            .context("metadata path missing parent directory")?;
        let mut temp = NamedTempFile::new_in(parent)
            .with_context(|| format!("failed to create temp file for task {}", self.task_id))?;
        temp.write_all(&payload)
            .with_context(|| format!("failed to write metadata for task {}", self.task_id))?;
        temp.as_file()
            .sync_all()
            .with_context(|| format!("failed to sync metadata for task {}", self.task_id))?;
        temp.persist(&path)
            .map_err(|err| err.error)
            .with_context(|| format!("failed to persist metadata for task {}", self.task_id))?;
        Ok(())
    }

    /// Loads metadata, applies a mutation, persists it, and returns the updated record.
    pub fn update_metadata<F>(&self, mutate: F) -> Result<TaskMetadata>
    where
        F: FnOnce(&mut TaskMetadata),
    {
        let mut metadata = self.read_metadata()?;
        mutate(&mut metadata);
        self.write_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Loads structured metadata for the task from disk.
    pub fn read_metadata(&self) -> Result<TaskMetadata> {
        let path = self.metadata_path();
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read metadata for task {}", self.task_id))?;
        let metadata: TaskMetadata = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse metadata for task {}", self.task_id))?;
        ensure!(
            metadata.id == self.task_id,
            "metadata id {} does not match path {}",
            metadata.id,
            self.task_id
        );
        Ok(metadata)
    }

    /// Writes the PID of the associated worker to disk.
    pub fn write_pid(&self, pid: i32) -> Result<()> {
        let path = self.pid_path();
        self.ensure_parent(&path)?;
        fs::write(&path, pid.to_string())
            .with_context(|| format!("failed to write pid for task {}", self.task_id))?;
        Ok(())
    }

    /// Reads the PID of the associated worker. Returns `None` if the PID file is missing.
    pub fn read_pid(&self) -> Result<Option<i32>> {
        let path = self.pid_path();
        match fs::read_to_string(&path) {
            Ok(raw) => {
                let value = raw
                    .trim()
                    .parse::<i32>()
                    .with_context(|| format!("failed to parse pid for task {}", self.task_id))?;
                Ok(Some(value))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("failed to read pid for task {}", self.task_id))
            }
        }
    }

    /// Removes the PID file, ignoring missing files.
    pub fn remove_pid(&self) -> Result<()> {
        let path = self.pid_path();
        match fs::remove_file(&path) {
            Ok(_) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err)
                .with_context(|| format!("failed to remove pid file for task {}", self.task_id)),
        }
    }

    /// Removes the pipe file, ignoring missing files.
    pub fn remove_pipe(&self) -> Result<()> {
        let path = self.pipe_path();
        match fs::remove_file(&path) {
            Ok(_) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => {
                Err(err).with_context(|| format!("failed to remove pipe for task {}", self.task_id))
            }
        }
    }

    /// Writes the last Codex result for the task to disk.
    pub fn write_last_result(&self, contents: &str) -> Result<()> {
        let path = self.result_path();
        self.ensure_parent(&path)?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write result for task {}", self.task_id))?;
        Ok(())
    }

    /// Reads the last Codex result for the task, if present.
    pub fn read_last_result(&self) -> Result<Option<String>> {
        let path = self.result_path();
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("failed to read result for task {}", self.task_id))
            }
        }
    }

    /// Ensures the directory holding task files exists.
    pub fn ensure_directory(&self) -> Result<()> {
        fs::create_dir_all(self.directory()).with_context(|| {
            format!(
                "failed to create task directory {}",
                self.directory().display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    #[test]
    fn ensure_layout_creates_directories() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join(".codex").join("tasks"));
        store.ensure_layout().expect("layout should be created");
        assert!(store.root().exists());
        assert!(store.archive_root().exists());
    }

    #[test]
    fn metadata_round_trip() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("store"));
        store.ensure_layout().expect("layout");
        let id = "abc-123".to_string();
        let files = store.task(id.clone());
        let metadata = TaskMetadata::new(
            id.clone(),
            Some("Example".into()),
            crate::tasks::TaskState::Stopped,
        );
        files.write_metadata(&metadata).expect("write metadata");
        let loaded = files.read_metadata().expect("read metadata");
        assert_eq!(metadata, loaded);
    }

    #[test]
    fn pid_read_write_and_remove() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("root"));
        store.ensure_layout().expect("layout");
        let files = store.task("task-1".to_string());
        assert_eq!(files.read_pid().expect("read pid"), None);
        files.write_pid(4242).expect("write pid");
        assert_eq!(files.read_pid().expect("read pid"), Some(4242));
        files.remove_pid().expect("remove pid");
        assert_eq!(files.read_pid().expect("read pid"), None);
    }

    #[test]
    fn last_result_round_trip() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("root"));
        store.ensure_layout().expect("layout");
        let files = store.task("task-42".to_string());
        assert_eq!(files.read_last_result().expect("read result"), None);
        files
            .write_last_result("some result")
            .expect("write result");
        assert_eq!(
            files.read_last_result().expect("read result"),
            Some("some result".to_string())
        );
    }

    #[test]
    fn ensure_archive_bucket_creates_hierarchy() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("root"));
        store.ensure_layout().expect("layout");
        let timestamp = Utc
            .with_ymd_and_hms(2024, 3, 14, 15, 9, 26)
            .single()
            .expect("valid timestamp");
        let bucket = store
            .ensure_archive_bucket(timestamp)
            .expect("create bucket");
        assert!(bucket.exists());
        assert!(bucket.ends_with("14"));
        let dir = store
            .ensure_archive_task_dir(timestamp, &"task-xyz".to_string())
            .expect("archive dir");
        assert!(dir.exists());
        assert!(dir.ends_with("task-xyz"));
    }

    #[test]
    fn archived_task_paths_include_task_directory() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("root"));
        store.ensure_layout().expect("layout");
        let timestamp = Utc
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .single()
            .expect("timestamp");
        let paths = store.archived_task(timestamp, "task-abc".to_string());
        let expected_dir = store
            .archive_root()
            .join("2024")
            .join("01")
            .join("02")
            .join("task-abc");
        assert_eq!(paths.directory(), expected_dir.as_path());
        assert_eq!(paths.log_path(), expected_dir.join(LOG_FILE_NAME));
    }

    #[test]
    fn find_archived_task_returns_metadata_and_paths() {
        let tmp = tempdir().expect("tempdir");
        let store = TaskStore::new(tmp.path().join("root"));
        store.ensure_layout().expect("layout");
        let timestamp = Utc
            .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
            .single()
            .expect("timestamp");
        let task_id = "task-find".to_string();
        let archive_dir = store
            .ensure_archive_task_dir(timestamp, &task_id)
            .expect("archive dir");
        let paths = TaskPaths::from_directory(archive_dir, task_id.clone());
        let metadata = TaskMetadata::new(task_id.clone(), None, crate::tasks::TaskState::Stopped);
        paths
            .write_metadata(&metadata)
            .expect("write archived metadata");

        let found = store
            .find_archived_task(&task_id)
            .expect("find archived task")
            .expect("task present");
        assert_eq!(found.0.directory(), paths.directory());
        assert_eq!(found.1, metadata);
    }
}
