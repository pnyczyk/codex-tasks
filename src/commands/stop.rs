use anyhow::Result;

use crate::cli::StopArgs;
use crate::services::tasks::{StopOutcome, TaskService};

pub fn handle_stop(args: StopArgs) -> Result<()> {
    let service = TaskService::with_default_store(false)?;

    if args.all {
        let reports = service.stop_all_running()?;
        if reports.is_empty() {
            println!("No running tasks to stop.");
            return Ok(());
        }

        let mut stopped = 0usize;
        let mut already = 0usize;

        for report in reports {
            print_stop_outcome(&report.task_id, report.outcome);
            match report.outcome {
                StopOutcome::Stopped => stopped += 1,
                StopOutcome::AlreadyStopped => already += 1,
            }
        }

        println!(
            "Stopped {stopped} running task(s); {already} already stopped.",
            stopped = stopped,
            already = already
        );

        Ok(())
    } else {
        let task_id = args
            .task_id
            .expect("task id is required when --all is not specified");
        let outcome = service.stop_task(&task_id)?;
        print_stop_outcome(&task_id, outcome);
        Ok(())
    }
}

fn print_stop_outcome(task_id: &str, outcome: StopOutcome) {
    match outcome {
        StopOutcome::AlreadyStopped => {
            println!("Task {} is not running; nothing to stop.", task_id);
        }
        StopOutcome::Stopped => {
            println!("Task {} stopped.", task_id);
        }
    }
}
