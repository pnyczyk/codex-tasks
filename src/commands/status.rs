use anyhow::Result;

use crate::cli::StatusArgs;
use crate::status::{StatusCommandOptions, StatusFormat};

pub fn handle_status(args: StatusArgs) -> Result<()> {
    let format = if args.json {
        StatusFormat::Json
    } else {
        StatusFormat::Human
    };
    crate::status::run(StatusCommandOptions {
        task_id: args.task_id,
        format,
        time_format: args.time_format,
    })
}
