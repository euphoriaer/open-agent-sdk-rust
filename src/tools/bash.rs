use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tokio::process::Command;

use crate::tools::command_runner;
use crate::types::{Tool, ToolError, ToolInputSchema, ToolResult, ToolUseContext};

const MAX_OUTPUT_SIZE: usize = 100_000;

/// Destructive command patterns that should be flagged.
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "rm -rf .",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "chmod 777",
    "chmod -R 777",
    "> /dev/sda",
    "mkfs.",
    "dd if=",
    ":(){ :|:& };:",
];

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Executes a given bash command and returns its output. Use for system commands and terminal operations that require shell execution. Long-running commands will periodically report partial output."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: HashMap::from([
                (
                    "command".to_string(),
                    json!({
                        "type": "string",
                        "description": "The command to execute"
                    }),
                ),
                (
                    "description".to_string(),
                    json!({
                        "type": "string",
                        "description": "Clear description of what this command does"
                    }),
                ),
            ]),
            required: vec!["command".to_string()],
            additional_properties: Some(false),
        }
    }

    fn is_read_only(&self, input: &Value) -> bool {
        let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");

        // Check if command starts with a read-only command
        let cmd_trimmed = command.trim();
        let first_cmd = cmd_trimmed.split_whitespace().next().unwrap_or("");
        let single_word_reads = [
            "ls", "cat", "head", "tail", "find", "grep", "rg", "wc", "pwd", "echo", "which",
            "type", "file", "stat", "du", "df",
        ];
        let prefix_reads = [
            "git status",
            "git log",
            "git diff",
            "git show",
            "git branch",
            "cargo check",
            "cargo test --no-run",
            "rustc --version",
        ];

        single_word_reads.contains(&first_cmd)
            || prefix_reads.iter().any(|p| cmd_trimmed.starts_with(p))
    }

    async fn call(&self, input: Value, context: &ToolUseContext) -> Result<ToolResult, ToolError> {
        let command = input
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing 'command' field".to_string()))?;

        // Security check
        if let Some(warning) = check_destructive(command) {
            return Ok(ToolResult::error(format!(
                "Potentially destructive command detected: {}. Proceed with caution.",
                warning
            )));
        }

        // Build shell command
        let shell = build_shell_runner(command, context.shell_binary.as_deref());
        let mut cmd = Command::new(&shell.program);
        cmd.args(&shell.args)
            .current_dir(&context.working_dir);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let description = input
            .get("description")
            .and_then(|d| d.as_str());

        let output = command_runner::run_command(
            &mut cmd,
            &context.abort_signal,
            None, // no hard timeout — use heartbeat + cancel instead
            context.event_sender.as_ref(),
            "Bash",
            description,
            context.tool_use_id.as_deref(),
        )
        .await;

        match output {
            Ok(out) => {
                let mut result = String::new();

                if !out.stdout.is_empty() {
                    result.push_str(&out.stdout);
                }
                if !out.stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("STDERR:\n");
                    result.push_str(&out.stderr);
                }

                if result.len() > MAX_OUTPUT_SIZE {
                    result.truncate(MAX_OUTPUT_SIZE);
                    result.push_str("\n... (output truncated)");
                }

                if out.exit_code != 0 {
                    result.push_str(&format!("\n\nExit code: {}", out.exit_code));
                }

                if result.is_empty() {
                    result = "(no output)".to_string();
                }

                Ok(if out.exit_code != 0 {
                    ToolResult::error(result)
                } else {
                    ToolResult::text(result)
                })
            }
            Err(e) => Ok(ToolResult::error(format!("Command failed: {}", e))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellRunner {
    program: String,
    args: Vec<String>,
}

fn build_shell_runner(command: &str, shell_override: Option<&str>) -> ShellRunner {
    if let Some(shell) = shell_override {
        if !shell.is_empty() && std::path::Path::new(shell).is_file() {
            return ShellRunner {
                program: shell.to_string(),
                args: vec!["-c".to_string(), command.to_string()],
            };
        }
    }
    select_shell_runner(
        command,
        cfg!(windows),
        std::env::var("ComSpec").ok(),
        program_exists,
    )
}

fn select_shell_runner<F>(
    command: &str,
    is_windows: bool,
    comspec: Option<String>,
    mut has_program: F,
) -> ShellRunner
where
    F: FnMut(&str) -> bool,
{
    if is_windows {
        for candidate in ["bash.exe", "bash"] {
            if has_program(candidate) {
                return ShellRunner {
                    program: candidate.to_string(),
                    args: vec!["-c".to_string(), command.to_string()],
                };
            }
        }

        for candidate in ["pwsh.exe", "pwsh", "powershell.exe", "powershell"] {
            if has_program(candidate) {
                return ShellRunner {
                    program: candidate.to_string(),
                    args: vec![
                        "-NoLogo".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-Command".to_string(),
                        command.to_string(),
                    ],
                };
            }
        }

        return ShellRunner {
            program: comspec
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "cmd.exe".to_string()),
            args: vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                command.to_string(),
            ],
        };
    }

    for candidate in ["/bin/bash", "bash", "/bin/sh", "sh"] {
        if has_program(candidate) {
            return ShellRunner {
                program: candidate.to_string(),
                args: vec!["-c".to_string(), command.to_string()],
            };
        }
    }

    ShellRunner {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
    }
}

fn program_exists(candidate: &str) -> bool {
    if candidate.contains(std::path::MAIN_SEPARATOR)
        || candidate.contains('/')
        || candidate.contains('\\')
    {
        return Path::new(candidate).is_file();
    }

    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let full_path = dir.join(candidate);
                if full_path.is_file() {
                    return true;
                }

                #[cfg(windows)]
                {
                    let exe_path = dir.join(format!("{candidate}.exe"));
                    return exe_path.is_file();
                }

                #[cfg(not(windows))]
                {
                    false
                }
            })
        })
        .unwrap_or(false)
}

fn check_destructive(command: &str) -> Option<&'static str> {
    for pattern in DESTRUCTIVE_PATTERNS {
        if command.contains(pattern) {
            return Some(pattern);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{select_shell_runner, ShellRunner};

    #[test]
    fn select_shell_runner_falls_back_to_powershell_on_windows() {
        let runner = select_shell_runner("node -v", true, None, |candidate| {
            matches!(candidate, "pwsh.exe")
        });

        assert_eq!(
            runner,
            ShellRunner {
                program: "pwsh.exe".to_string(),
                args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "node -v".to_string(),
                ],
            }
        );
    }

    #[test]
    fn select_shell_runner_falls_back_to_cmd_on_windows() {
        let runner = select_shell_runner(
            "node -v",
            true,
            Some("C:\\Windows\\System32\\cmd.exe".to_string()),
            |_| false,
        );

        assert_eq!(
            runner,
            ShellRunner {
                program: "C:\\Windows\\System32\\cmd.exe".to_string(),
                args: vec![
                    "/d".to_string(),
                    "/s".to_string(),
                    "/c".to_string(),
                    "node -v".to_string(),
                ],
            }
        );
    }

    #[test]
    fn select_shell_runner_prefers_bash_on_unix() {
        let runner =
            select_shell_runner("node -v", false, None, |candidate| candidate == "/bin/bash");

        assert_eq!(
            runner,
            ShellRunner {
                program: "/bin/bash".to_string(),
                args: vec!["-c".to_string(), "node -v".to_string()],
            }
        );
    }
}
