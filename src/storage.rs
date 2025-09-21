use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, Utc};
use dirs::home_dir;
use uuid::Uuid;

use crate::task::{TaskId, TaskMetadata};

const METADATA_EXTENSION: &str = "json";

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
        self.root.join("done")
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

    /// Generates a new random identifier for a task.
    pub fn generate_task_id(&self) -> TaskId {
        Uuid::new_v4().to_string()
    }

    /// Returns helpers for interacting with an active task's files.
    pub fn task(&self, task_id: impl Into<TaskId>) -> TaskPaths {
        TaskPaths::new(self.root.clone(), task_id.into())
    }

    /// Returns helpers for interacting with an archived task's files.
    pub fn archived_task(&self, timestamp: DateTime<Utc>, task_id: impl Into<TaskId>) -> TaskPaths {
        let id = task_id.into();
        let dir = self.archive_bucket(timestamp).join(&id);
        TaskPaths::new(dir, id)
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

    /// Returns the identifier associated with these paths.
    pub fn id(&self) -> &str {
        &self.task_id
    }

    /// Returns the directory that contains the task's files.
    pub fn directory(&self) -> &Path {
        &self.base
    }

    fn file_path(&self, extension: &str) -> PathBuf {
        self.base.join(format!("{}.{}", self.task_id, extension))
    }

    /// Location of the PID file for the task.
    pub fn pid_path(&self) -> PathBuf {
        self.file_path("pid")
    }

    /// Location of the FIFO used for sending prompts to the worker.
    pub fn pipe_path(&self) -> PathBuf {
        self.file_path("pipe")
    }

    /// Location where the worker writes the transcript log.
    pub fn log_path(&self) -> PathBuf {
        self.file_path("log")
    }

    /// Location that stores the most recent Codex result.
    pub fn result_path(&self) -> PathBuf {
        self.file_path("result")
    }

    /// Location of the structured metadata file.
    pub fn metadata_path(&self) -> PathBuf {
        self.file_path(METADATA_EXTENSION)
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
        let payload = serde_json::to_string_pretty(metadata)
            .with_context(|| format!("failed to serialize metadata for task {}", self.task_id))?;
        fs::write(&path, payload)
            .with_context(|| format!("failed to write metadata for task {}", self.task_id))?;
        Ok(())
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
            crate::task::TaskState::Idle,
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
        assert_eq!(paths.log_path(), expected_dir.join("task-abc.log"));
    }
}
