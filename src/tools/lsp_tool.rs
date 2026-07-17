use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tokio::process::Command;

use crate::tools::command_runner;
use crate::types::{Tool, ToolError, ToolInputSchema, ToolResult, ToolUseContext};

/// LSPTool - Language Server Protocol integration.
///
/// Provides code intelligence: go-to-definition, find-references,
/// hover, document symbols. Uses ripgrep/grep as fallback for symbol lookup.
pub struct LSPTool;

#[async_trait]
impl Tool for LSPTool {
    fn name(&self) -> &str {
        "LSP"
    }

    fn description(&self) -> &str {
        "Language Server Protocol operations for code intelligence. Supports go-to-definition, find-references, hover, and symbol lookup."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: HashMap::from([
                (
                    "operation".to_string(),
                    json!({
                        "type": "string",
                        "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol"],
                        "description": "LSP operation to perform"
                    }),
                ),
                (
                    "file_path".to_string(),
                    json!({ "type": "string", "description": "File path for the operation" }),
                ),
                (
                    "line".to_string(),
                    json!({ "type": "number", "description": "Line number (0-based)" }),
                ),
                (
                    "character".to_string(),
                    json!({ "type": "number", "description": "Character position (0-based)" }),
                ),
                (
                    "query".to_string(),
                    json!({ "type": "string", "description": "Symbol name (for workspace symbol search)" }),
                ),
            ]),
            required: vec!["operation".to_string()],
            additional_properties: Some(false),
        }
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    async fn call(&self, input: Value, context: &ToolUseContext) -> Result<ToolResult, ToolError> {
        let operation = input
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing 'operation'".to_string()))?;

        let file_path = input.get("file_path").and_then(|v| v.as_str());
        let line = input.get("line").and_then(|v| v.as_u64());
        let character = input.get("character").and_then(|v| v.as_u64()).unwrap_or(0);

        let cwd = &context.working_dir;

        match operation {
            "goToDefinition" => {
                let file_path = file_path
                    .ok_or_else(|| ToolError::InvalidInput("file_path required".to_string()))?;
                let line = line
                    .ok_or_else(|| ToolError::InvalidInput("line required".to_string()))?;

                let symbol = get_symbol_at_position(file_path, line as usize, character as usize, cwd);
                match symbol {
                    Some(sym) => {
                        let pattern = format!(
                            r"(?:fn |struct |enum |trait |type |const |let |pub fn |pub struct |pub enum |pub trait |pub type |pub const |impl |mod |use )\s*{}",
                            regex::escape(&sym)
                        );
                        let result = run_rg_or_grep(&pattern, cwd, &context.abort_signal)
                            .await
                            .map_err(ToolError::ExecutionError)?;
                        if result.is_empty() {
                            Ok(ToolResult::text(format!("No definition found for \"{}\"", sym)))
                        } else {
                            Ok(ToolResult::text(result))
                        }
                    }
                    None => Ok(ToolResult::text("Could not identify symbol at position")),
                }
            }

            "findReferences" => {
                let file_path = file_path
                    .ok_or_else(|| ToolError::InvalidInput("file_path required".to_string()))?;
                let line = line
                    .ok_or_else(|| ToolError::InvalidInput("line required".to_string()))?;

                let symbol = get_symbol_at_position(file_path, line as usize, character as usize, cwd);
                match symbol {
                    Some(sym) => {
                        let result = run_rg_or_grep(
                            &regex::escape(&sym),
                            cwd,
                            &context.abort_signal,
                        )
                        .await
                        .map_err(ToolError::ExecutionError)?;
                        if result.is_empty() {
                            Ok(ToolResult::text(format!("No references found for \"{}\"", sym)))
                        } else {
                            Ok(ToolResult::text(result))
                        }
                    }
                    None => Ok(ToolResult::text("Could not identify symbol at position")),
                }
            }

            "hover" => Ok(ToolResult::text(
                "Hover information requires a running language server. Use FileRead tool to examine the file content.",
            )),

            "documentSymbol" => {
                let file_path = file_path
                    .ok_or_else(|| ToolError::InvalidInput("file_path required".to_string()))?;

                let pattern = r"^\s*(pub\s+)?(fn |struct |enum |trait |type |const |let |impl |mod |use )";
                let result = run_rg_on_file(pattern, file_path, cwd, &context.abort_signal)
                    .await
                    .map_err(ToolError::ExecutionError)?;
                if result.is_empty() {
                    Ok(ToolResult::text("No symbols found"))
                } else {
                    Ok(ToolResult::text(result))
                }
            }

            _ => Ok(ToolResult::text(format!(
                "LSP operation \"{}\" requires a running language server.",
                operation
            ))),
        }
    }
}

/// Extract the symbol (word) at a given line and character position in a file.
fn get_symbol_at_position(
    file_path: &str,
    line: usize,
    character: usize,
    cwd: &str,
) -> Option<String> {
    let full_path = Path::new(cwd).join(file_path);
    let content = std::fs::read_to_string(full_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    if line >= lines.len() {
        return None;
    }

    let line_text = lines[line];
    if character >= line_text.len() {
        return None;
    }

    // Find the word boundary around the character position
    let bytes = line_text.as_bytes();
    let mut start = character;
    let mut end = character;

    while start > 0 && is_word_char(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_word_char(bytes[end]) {
        end += 1;
    }

    if start == end {
        return None;
    }

    Some(line_text[start..end].to_string())
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Run ripgrep on the working directory, falling back to grep.
async fn run_rg_or_grep(
    pattern: &str,
    cwd: &str,
    abort_signal: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    // Try ripgrep first
    let mut cmd = Command::new("rg");
    cmd.args([
        "-n",
        pattern,
        "--type-add",
        "src:*.{ts,tsx,js,jsx,py,go,rs,java}",
        "-t",
        "src",
    ])
    .arg(cwd);

    let rg_error = match run_search_command(&mut cmd, abort_signal, "rg").await {
        Ok(Some(output)) => {
            let lines: Vec<&str> = output.lines().take(50).collect();
            return Ok(lines.join("\n"));
        }
        Ok(None) => return Ok(String::new()),
        Err(error) => error,
    };

    // Fall back only when ripgrep could not execute successfully.
    let mut cmd = Command::new("grep");
    cmd.args([
        "-rn",
        pattern,
        cwd,
        "--include=*.rs",
        "--include=*.ts",
        "--include=*.py",
        "--include=*.go",
        "--include=*.java",
    ]);

    match run_search_command(&mut cmd, abort_signal, "grep").await {
        Ok(Some(output)) => {
            let lines: Vec<&str> = output.lines().take(50).collect();
            Ok(lines.join("\n"))
        }
        Ok(None) => Ok(String::new()),
        Err(grep_error) => Err(format!("rg: {}; grep: {}", rg_error, grep_error)),
    }
}

/// Run ripgrep on a single file, falling back to grep.
async fn run_rg_on_file(
    pattern: &str,
    file_path: &str,
    cwd: &str,
    abort_signal: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    let full_path = Path::new(cwd).join(file_path);
    let path_str = full_path.to_string_lossy().to_string();

    let mut cmd = Command::new("rg");
    cmd.args(["-n", pattern]).arg(&path_str);

    let rg_error = match run_search_command(&mut cmd, abort_signal, "rg").await {
        Ok(Some(output)) => return Ok(output.trim().to_string()),
        Ok(None) => return Ok(String::new()),
        Err(error) => error,
    };

    let mut cmd = Command::new("grep");
    cmd.args(["-n", pattern]).arg(&path_str);

    match run_search_command(&mut cmd, abort_signal, "grep").await {
        Ok(Some(output)) => Ok(output.trim().to_string()),
        Ok(None) => Ok(String::new()),
        Err(grep_error) => Err(format!("rg: {}; grep: {}", rg_error, grep_error)),
    }
}

async fn run_search_command(
    cmd: &mut Command,
    abort_signal: &tokio_util::sync::CancellationToken,
    tool_name: &str,
) -> Result<Option<String>, String> {
    let output = command_runner::run_command(
        cmd,
        abort_signal,
        command_runner::CommandRunOptions {
            timeout: Some(std::time::Duration::from_secs(10)),
            event_sender: None,
            tool_name,
            description: None,
            tool_use_id: None,
        },
    )
    .await?;
    match output.exit_code {
        0 => Ok(Some(output.stdout)),
        1 => Ok(None),
        _ => Err(output.stderr),
    }
}
