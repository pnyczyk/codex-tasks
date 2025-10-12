use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::cli::StartArgs;
use crate::storage::TaskStore;
use crate::worker::launcher::{WorkerLaunchRequest, spawn_worker};

pub fn handle_start(args: StartArgs) -> Result<()> {
    let StartArgs {
        title,
        prompt,
        config_file,
        working_dir,
        repo,
        repo_ref,
    } = args;

    let prompt = resolve_start_prompt(prompt)?;
    let config_file = resolve_config_file(config_file)?;
    let working_dir = prepare_working_directory(working_dir, repo.as_deref(), repo_ref.as_deref())?;
    let working_dir = match working_dir {
        Some(path) => Some(make_absolute(path)?),
        None => {
            let cwd = env::current_dir()
                .context("failed to determine current working directory for worker")?;
            Some(make_absolute(cwd)?)
        }
    };

    let store = TaskStore::default().context("failed to locate task store")?;
    store
        .ensure_layout()
        .context("failed to prepare task store layout")?;

    let mut request = WorkerLaunchRequest::new(store.root().to_path_buf(), prompt);
    request.title = title;
    request.config_path = config_file;
    request.working_directory = working_dir.clone();

    let mut child = spawn_worker(request).context("failed to launch worker process")?;
    let thread_id = receive_thread_id(&mut child)?;

    drop(child);

    println!("{thread_id}");

    Ok(())
}

fn resolve_start_prompt(raw_prompt: String) -> Result<String> {
    if raw_prompt == "-" {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read prompt from stdin")?;
        if buffer.trim().is_empty() {
            bail!("no prompt provided via stdin");
        }
        Ok(buffer)
    } else if raw_prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    } else {
        Ok(raw_prompt)
    }
}

fn receive_thread_id(child: &mut std::process::Child) -> Result<String> {
    let stdout = child
        .stdout
        .take()
        .context("worker did not expose stdout for handshake")?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let result = reader
            .read_line(&mut line)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                if bytes == 0 {
                    Err(anyhow!("worker exited before publishing thread id"))
                } else {
                    Ok(line.trim().to_string())
                }
            });
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(id)) if !id.is_empty() => Ok(id),
        Ok(Ok(_)) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("worker returned empty thread identifier");
        }
        Ok(Err(err)) => {
            let _ = child.kill();
            if let Ok(status) = child.wait() {
                bail!("failed to start worker: {err:#}. worker exited with {status}");
            } else {
                bail!("failed to start worker: {err:#}");
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out waiting for worker to publish thread id");
        }
    }
}

fn resolve_config_file(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };

    let absolute = make_absolute(path)?;
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve config file at {}", absolute.display()))?;
    ensure!(
        canonical.is_file(),
        "config file {} does not exist or is not a file",
        canonical.display()
    );
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!(
                "config file path {} is missing file name",
                canonical.display()
            )
        })?;
    ensure!(
        name == "config.toml",
        "custom config file must be named `config.toml` (got {name})",
        name = name
    );
    Ok(Some(canonical))
}

fn prepare_working_directory(
    working_dir: Option<PathBuf>,
    repo: Option<&str>,
    repo_ref: Option<&str>,
) -> Result<Option<PathBuf>> {
    let resolved = match working_dir {
        Some(path) => Some(make_absolute(path)?),
        None => None,
    };

    if repo.is_some() {
        let repo_url = repo.unwrap();
        let repo_spec_storage = if Path::new(repo_url).exists() {
            Some(make_absolute(PathBuf::from(repo_url))?.into_os_string())
        } else {
            None
        };
        let repo_spec: &OsStr = repo_spec_storage
            .as_ref()
            .map(|value| value.as_os_str())
            .unwrap_or_else(|| OsStr::new(repo_url));
        let target = resolved
            .as_ref()
            .ok_or_else(|| anyhow!("`--working-dir` is required when `--repo` is provided"))?;
        clone_repository(repo_spec, repo_ref, target)?;
    } else if let Some(path) = resolved.as_ref() {
        if !path.exists() {
            fs::create_dir_all(path).with_context(|| {
                format!("failed to create working directory {}", path.display())
            })?;
        }
    }

    match resolved {
        Some(path) => {
            let canonical = path.canonicalize().with_context(|| {
                format!("failed to resolve working directory {}", path.display())
            })?;
            Ok(Some(canonical))
        }
        None => Ok(None),
    }
}

fn clone_repository(repo_spec: &OsStr, repo_ref: Option<&str>, target_dir: &Path) -> Result<()> {
    let parent = target_dir.parent().ok_or_else(|| {
        anyhow!(
            "working directory {} is missing a parent directory",
            target_dir.display()
        )
    })?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent directory {}", parent.display()))?;

    if target_dir.exists() {
        bail!(
            "working directory {} already exists; remove it or choose a different directory before cloning",
            target_dir.display()
        );
    }

    let name = target_dir.file_name().ok_or_else(|| {
        anyhow!(
            "working directory {} is missing a final path component",
            target_dir.display()
        )
    })?;

    let status = StdCommand::new("git")
        .current_dir(parent)
        .arg("clone")
        .arg(repo_spec)
        .arg(name)
        .status()
        .with_context(|| {
            format!(
                "failed to run `git clone` for {}",
                repo_spec.to_string_lossy()
            )
        })?;
    ensure!(
        status.success(),
        "`git clone` for {} exited with status {status}",
        repo_spec.to_string_lossy(),
        status = status
    );

    if let Some(reference) = repo_ref {
        let mut checkout_status = StdCommand::new("git")
            .current_dir(target_dir)
            .args(["checkout", reference])
            .status()
            .with_context(|| format!("failed to checkout {reference} in cloned repository"))?;

        if !checkout_status.success() {
            let fetch_status = StdCommand::new("git")
                .current_dir(target_dir)
                .args(["fetch", "origin", reference])
                .status()
                .with_context(|| {
                    format!(
                        "failed to fetch {reference} from {}",
                        repo_spec.to_string_lossy()
                    )
                })?;
            ensure!(
                fetch_status.success(),
                "`git fetch origin {reference}` exited with status {fetch_status}",
                reference = reference,
                fetch_status = fetch_status
            );

            checkout_status = StdCommand::new("git")
                .current_dir(target_dir)
                .args(["checkout", reference])
                .status()
                .with_context(|| format!("failed to checkout {reference} after fetch"))?;
        }

        ensure!(
            checkout_status.success(),
            "`git checkout {reference}` exited with status {checkout_status}",
            reference = reference,
            checkout_status = checkout_status
        );
    }

    Ok(())
}

fn make_absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = env::current_dir().context("failed to resolve current working directory")?;
        Ok(cwd.join(path))
    }
}
