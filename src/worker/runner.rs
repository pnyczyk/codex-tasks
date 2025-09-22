use std::io::IsTerminal;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use codex_core::config::{Config, ConfigOverrides};
use codex_core::protocol::{Event, InputItem, Op, Submission};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use super::event_processor::{CodexStatus, EventProcessor};
use super::event_processor_with_human_output::EventProcessorWithHumanOutput;

/// Convenience entry point that mirrors the original interactive binary.
#[allow(dead_code)]
pub(crate) async fn run_interactive_session() -> Result<()> {
    let config = Config::load_with_cli_overrides(Vec::new(), ConfigOverrides::default())
        .context("failed to load Codex configuration")?;

    let with_ansi = std::io::stdout().is_terminal();
    let mut event_processor =
        EventProcessorWithHumanOutput::create_with_ansi(with_ansi, &config, None);

    let ChildHandles {
        child,
        stdout,
        stdin,
    } = spawn_codex_proto().await?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(trimmed) {
                Ok(event) => {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("Failed to parse event from codex proto: {err}");
                }
            }
        }
    });

    run_event_loop(
        child,
        stdin,
        &mut event_processor,
        &config,
        &mut event_rx,
        tokio::io::stdin(),
        None,
    )
    .await
}

#[allow(dead_code)]
pub(crate) async fn send_submission(
    writer: &mut BufWriter<ChildStdin>,
    submission: Submission,
) -> Result<()> {
    let json = serde_json::to_vec(&submission)?;
    writer.write_all(&json).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn spawn_codex_proto() -> Result<ChildHandles> {
    let mut command = Command::new("codex");
    command.arg("proto");
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    if std::env::var_os("RUST_LOG").is_none() {
        command.env("RUST_LOG", "off");
    }

    let mut child = command.spawn().context("failed to spawn `codex proto`")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture stdout of `codex proto`")?;
    let stdin = child
        .stdin
        .take()
        .context("failed to capture stdin of `codex proto`")?;

    Ok(ChildHandles {
        child,
        stdout,
        stdin,
    })
}

#[allow(dead_code)]
pub(crate) struct ChildHandles {
    pub child: Child,
    pub stdout: tokio::process::ChildStdout,
    pub stdin: ChildStdin,
}

#[allow(dead_code)]
pub(crate) async fn run_event_loop<R, P>(
    mut child: Child,
    stdin: ChildStdin,
    event_processor: &mut P,
    config: &Config,
    event_rx: &mut mpsc::UnboundedReceiver<Event>,
    input: R,
    initial_prompt: Option<String>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    P: EventProcessor,
{
    let mut writer = Some(BufWriter::new(stdin));
    let mut next_submission_id: u64 = 1;
    let mut prompt: Option<String> = None;
    let mut printed_summary = false;
    let mut shutdown_sent = false;
    let mut shutdown_acknowledged = false;
    let child_pid = child.id().unwrap_or_default();
    let mut input_lines = BufReader::with_capacity(4096, input).split(b'\n');
    let mut input_open = true;

    if let Some(initial_line) = initial_prompt {
        process_user_line(
            initial_line,
            &mut prompt,
            &mut printed_summary,
            event_processor,
            config,
            &mut writer,
            &mut next_submission_id,
        )
        .await?;
    }

    'outer: loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        handle_event(
                            event_processor,
                            event,
                            &mut writer,
                            &mut shutdown_acknowledged,
                        ).await?;
                        if shutdown_acknowledged {
                            break 'outer;
                        }
                    }
                    None => {
                        break 'outer;
                    }
                }
            }
            line = input_lines.next_segment(), if input_open => {
                match line {
                    Ok(Some(line_bytes)) => {
                        let line = match String::from_utf8(line_bytes) {
                            Ok(line) => line,
                            Err(err) => {
                                eprintln!("Failed to decode prompt source as UTF-8: {err:#}");
                                continue;
                            }
                        };

                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if trimmed == "/quit" {
                            initiate_shutdown(
                                &mut writer,
                                &mut next_submission_id,
                                &mut shutdown_sent,
                                child_pid,
                            ).await?;
                            input_open = false;
                            continue;
                        }

                        process_user_line(
                            line,
                            &mut prompt,
                            &mut printed_summary,
                            event_processor,
                            config,
                            &mut writer,
                            &mut next_submission_id,
                        ).await?;
                    }
                    Ok(None) => {
                        input_open = false;
                        initiate_shutdown(
                            &mut writer,
                            &mut next_submission_id,
                            &mut shutdown_sent,
                            child_pid,
                        ).await?;
                    }
                    Err(err) => {
                        eprintln!("Failed to read prompt source: {err:#}");
                        input_open = false;
                        initiate_shutdown(
                            &mut writer,
                            &mut next_submission_id,
                            &mut shutdown_sent,
                            child_pid,
                        ).await?;
                    }
                }
            }
            else => {
                break 'outer;
            }
        }
    }

    if let Some(mut writer) = writer.take() {
        if let Err(err) = writer.shutdown().await {
            eprintln!("Failed to close Codex stdin: {err:#}");
        } else {
            eprintln!("closed stdin pipe to codex proto (pid {child_pid})");
        }
    }

    eprintln!("waiting for codex proto (pid {child_pid}) to exit");
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => {
            eprintln!("codex proto (pid {child_pid}) exited with status {status}");
            if !status.success() {
                eprintln!("Codex subprocess exited with non-zero status: {status}");
            }
        }
        Ok(Err(err)) => {
            eprintln!("Codex subprocess wait failed: {err:#}");
        }
        Err(_) => {
            eprintln!("codex proto (pid {child_pid}) did not exit after shutdown; sending kill");
            if let Err(err) = child.start_kill() {
                eprintln!("failed to kill codex proto (pid {child_pid}): {err:#}");
            }
            match child.wait().await {
                Ok(status) => {
                    eprintln!("codex proto (pid {child_pid}) killed; status {status}");
                }
                Err(err) => {
                    eprintln!("Codex subprocess wait after kill failed: {err:#}");
                }
            }
        }
    }

    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn handle_event<P: EventProcessor>(
    event_processor: &mut P,
    event: Event,
    writer: &mut Option<BufWriter<ChildStdin>>,
    shutdown_acknowledged: &mut bool,
) -> Result<()> {
    match event_processor.process_event(event) {
        CodexStatus::Running => {}
        CodexStatus::InitiateShutdown => {}
        CodexStatus::Shutdown => {
            *shutdown_acknowledged = true;
            if let Some(mut w) = writer.take() {
                if let Err(err) = w.shutdown().await {
                    eprintln!("Failed to close Codex stdin: {err:#}");
                } else {
                    eprintln!("closed stdin pipe to codex proto after shutdown ack");
                }
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn initiate_shutdown(
    writer: &mut Option<BufWriter<ChildStdin>>,
    next_submission_id: &mut u64,
    shutdown_sent: &mut bool,
    child_pid: u32,
) -> Result<()> {
    if !*shutdown_sent {
        eprintln!("sending shutdown to codex proto (pid {child_pid})");
        let submission = Submission {
            id: format!("sub-{next_submission_id:010}"),
            op: Op::Shutdown,
        };
        *next_submission_id += 1;
        if let Some(writer) = writer.as_mut() {
            send_submission(writer, submission).await?;
        }
        *shutdown_sent = true;
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn process_user_line<P: EventProcessor>(
    text: String,
    prompt: &mut Option<String>,
    printed_summary: &mut bool,
    event_processor: &mut P,
    config: &Config,
    writer: &mut Option<BufWriter<ChildStdin>>,
    next_submission_id: &mut u64,
) -> Result<()> {
    let first_prompt = prompt.is_none();
    if first_prompt {
        *prompt = Some(text.clone());
        if !*printed_summary {
            event_processor.print_config_summary(config, &text);
            *printed_summary = true;
        }
    } else {
        event_processor.print_user_prompt(&text);
    }

    if let Some(writer) = writer.as_mut() {
        let submission = Submission {
            id: format!("sub-{next_submission_id:010}"),
            op: Op::UserInput {
                items: vec![InputItem::Text { text }],
            },
        };
        *next_submission_id += 1;
        if let Err(err) = send_submission(writer, submission).await {
            eprintln!("Failed to send submission: {err:#}");
        }
    }

    Ok(())
}
