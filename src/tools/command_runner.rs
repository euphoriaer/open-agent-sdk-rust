use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::types::SDKMessage;

/// Result of a completed command execution.
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const USER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const STALL_THRESHOLD: Duration = Duration::from_secs(90);
const LAST_OUTPUT_CHARS: usize = 500;
const OUTPUT_THROTTLE_MS: u64 = 200;

fn format_elapsed(secs: u64) -> String {
    if secs >= 86400 {
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        let mins = (secs % 3600) / 60;
        let secs = secs % 60;
        format!("运行{}天{}小时{}分{}秒", days, hours, mins, secs)
    } else if secs >= 3600 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let secs = secs % 60;
        format!("运行{}小时{}分{}秒", hours, mins, secs)
    } else if secs >= 60 {
        let mins = secs / 60;
        let secs = secs % 60;
        format!("运行{}分{}秒", mins, secs)
    } else {
        format!("运行{}秒", secs)
    }
}

/// Run a command with safe pipe handling, cancellation, and optional heartbeat.
///
/// Replaces all `.output().await` patterns to prevent Windows pipe-inheritance hangs.
/// For long-running commands (e.g. bash install), provide `event_sender` + `description`
/// so the frontend receives periodic status updates with partial output.
pub async fn run_command(
    cmd: &mut Command,
    abort_signal: &CancellationToken,
    timeout: Option<Duration>,
    event_sender: Option<&mpsc::Sender<SDKMessage>>,
    tool_name: &str,
    description: Option<&str>,
    tool_use_id: Option<&str>,
) -> Result<CmdOutput, String> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", tool_name, e))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    // Channel for streaming pipe data: (is_stdout, data_chunk)
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<(bool, Vec<u8>)>(64);

    // Read stdout in a background task
    let tx = chunk_tx.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        let mut reader = tokio::io::BufReader::new(stdout);
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send((true, buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Read stderr in a background task
    let stderr_tx = chunk_tx.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        let mut reader = tokio::io::BufReader::new(stderr);
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stderr_tx.send((false, buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    drop(chunk_tx);

    let mut stdout_bytes: Vec<u8> = Vec::new();
    let mut stderr_bytes: Vec<u8> = Vec::new();
    let start = Instant::now();
    let mut last_data_time = Instant::now();
    let mut last_output_time = Instant::now();
    let tool_use_id = tool_use_id.map(|s| s.to_string());

    let deadline = timeout.map(|t| start + t);
    let mut ai_heartbeat_ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    // Skip the immediate first tick
    ai_heartbeat_ticker.tick().await;
    let mut user_heartbeat_ticker = tokio::time::interval(USER_HEARTBEAT_INTERVAL);
    user_heartbeat_ticker.tick().await;

    let status = loop {
        tokio::select! {
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some((is_stdout, data)) => {
                        if is_stdout {
                            stdout_bytes.extend_from_slice(&data);
                        } else {
                            stderr_bytes.extend_from_slice(&data);
                        }
                        last_data_time = Instant::now();

                        // Throttled real-time output streaming
                        if let Some(ref sender) = event_sender {
                            if let Some(ref tuid) = tool_use_id {
                                let now = Instant::now();
                                if now.duration_since(last_output_time).as_millis() as u64 >= OUTPUT_THROTTLE_MS {
                                    last_output_time = now;
                                    let partial = String::from_utf8_lossy(&stdout_bytes)
                                        .replace("\r\n", "\n")
                                        .replace("\r", "\n");
                                    let _ = sender.send(SDKMessage::ToolOutput {
                                        tool_use_id: tuid.clone(),
                                        tool_name: tool_name.to_string(),
                                        content: partial,
                                    }).await;
                                }
                            }
                        }
                    }
                    None => {
                        // Both pipe readers finished; wait for process exit
                        break child.wait().await.map_err(|e| e.to_string())?;
                    }
                }
            }
            _ = abort_signal.cancelled() => {
                kill_child(&mut child).await;
                return Err(format!("{} aborted", tool_name));
            }
            _ = user_heartbeat_ticker.tick() => {
                // Fast user-facing status bar update (1s)
                if let Some(sender) = event_sender {
                    let elapsed = start.elapsed().as_secs();
                    let combined = String::from_utf8_lossy(&stdout_bytes)
                        .replace("\r\n", "\n")
                        .replace("\r", "\n");
                    let last_line = combined.lines()
                        .filter(|l| !l.is_empty())
                        .last()
                        .unwrap_or("")
                        .to_string();
                    let desc = description.unwrap_or(tool_name);
                    let display_line = if last_line.len() > 50 {
                        format!("{}...", &last_line[..47])
                    } else {
                        last_line.clone()
                    };
                    let msg = if !last_line.is_empty() {
                        format!("{}({})--{}", desc, format_elapsed(elapsed), display_line)
                    } else {
                        format!("{}({})--运行中...", desc, format_elapsed(elapsed))
                    };
                    let _ = sender.send(SDKMessage::Status { message: msg }).await;
                }
            }
            _ = ai_heartbeat_ticker.tick() => {
                // Slow AI-awareness heartbeat (30s)
                if let Some(sender) = event_sender {
                    let elapsed = start.elapsed().as_secs();
                    let total_bytes = stdout_bytes.len() + stderr_bytes.len();

                    let combined = String::from_utf8_lossy(&stdout_bytes);
                    let preview = if combined.len() > LAST_OUTPUT_CHARS {
                        let start_idx = combined.len() - LAST_OUTPUT_CHARS;
                        format!("...{}", &combined[start_idx..])
                    } else {
                        combined.to_string()
                    };

                    let desc = description.map(|d| format!(" ({})", d)).unwrap_or_default();
                    let stalled = last_data_time.elapsed() >= STALL_THRESHOLD;
                    let stalled_warn = if stalled {
                        format!("\n\n[⚠️ 输出已 {} 秒无变化，可能已卡住]", last_data_time.elapsed().as_secs())
                    } else {
                        String::new()
                    };

                    let msg = format!(
                        "[{}]{} 运行中 ({}s) — 累积 {}KB\n{}{}",
                        tool_name, desc, elapsed, total_bytes / 1024, preview, stalled_warn,
                    );
                    let _ = sender.send(SDKMessage::Progress { message: msg }).await;
                }
            }
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                kill_child(&mut child).await;
                let elapsed = start.elapsed().as_secs();
                return Err(format!("{} timed out after {}s", tool_name, elapsed));
            }
        }
    };

    // Final flush: send complete output after process exits
    if let Some(ref sender) = event_sender {
        if let Some(ref tuid) = tool_use_id {
            let full = String::from_utf8_lossy(&stdout_bytes)
                .replace("\r\n", "\n")
                .replace("\r", "\n");
            let _ = sender
                .send(SDKMessage::ToolOutput {
                    tool_use_id: tuid.clone(),
                    tool_name: tool_name.to_string(),
                    content: full,
                })
                .await;
        }
        // Clear status bar — command finished
        let _ = sender
            .send(SDKMessage::Status {
                message: String::new(),
            })
            .await;
    }

    let exit_code = status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&stdout_bytes)
        .replace("\r\n", "\n")
        .replace("\r", "\n");
    let stderr = String::from_utf8_lossy(&stderr_bytes)
        .replace("\r\n", "\n")
        .replace("\r", "\n");

    Ok(CmdOutput { stdout, stderr, exit_code })
}

async fn kill_child(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        // On Windows, child.kill() only kills the immediate shell (e.g. cmd.exe),
        // not grandchild processes (e.g. git.exe). Use taskkill /T to kill the
        // entire process tree.
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("taskkill")
                .args(&["/F", "/T", "/PID", &pid.to_string()])
                .output()
                .await;
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}
