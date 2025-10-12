use anyhow::Context;

use crate::cli::WorkerArgs;

pub fn handle_worker(args: WorkerArgs) -> anyhow::Result<()> {
    let config = crate::worker::child::WorkerConfig::new(
        args.store_root,
        args.task_id,
        args.title,
        args.prompt,
        args.config_path,
        args.working_dir,
    )?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize async runtime for worker")?
        .block_on(crate::worker::child::run_worker(config))
}
