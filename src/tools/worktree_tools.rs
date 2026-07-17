use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;

use crate::tools::command_runner;
use crate::types::{Tool, ToolError, ToolInputSchema, ToolResult, ToolUseContext};

/// Info about an active worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub original_cwd: String,
}

/// Shared worktree tracker.
pub type WorktreeStore = Arc<RwLock<HashMap<String, WorktreeInfo>>>;

pub fn new_worktree_store() -> WorktreeStore {
    Arc::new(RwLock::new(HashMap::new()))
}

// ============================================================================
// EnterWorktreeTool
// ============================================================================

pub struct EnterWorktreeTool {
    store: WorktreeStore,
}

impl EnterWorktreeTool {
    pub fn new(store: WorktreeStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        "EnterWorktree"
    }

    fn description(&self) -> &str {
        "Create an isolated git worktree for parallel work. The agent will work in the worktree without affecting the main working tree."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: HashMap::from([
                (
                    "branch".to_string(),
                    json!({ "type": "string", "description": "Branch name for the worktree (auto-generated if not provided)" }),
                ),
                (
                    "path".to_string(),
                    json!({ "type": "string", "description": "Path for the worktree (auto-generated if not provided)" }),
                ),
            ]),
            required: Vec::new(),
            additional_properties: Some(false),
        }
    }

    async fn call(&self, input: Value, context: &ToolUseContext) -> Result<ToolResult, ToolError> {
        // Check if we're in a git repo
        let mut check_cmd = Command::new("git");
        check_cmd
            .args(["rev-parse", "--git-dir"])
            .current_dir(&context.working_dir);
        let check = command_runner::run_command(
            &mut check_cmd,
            &context.abort_signal,
            command_runner::CommandRunOptions {
                timeout: Some(std::time::Duration::from_secs(10)),
                event_sender: None,
                tool_name: "EnterWorktree",
                description: None,
                tool_use_id: context.tool_use_id.as_deref(),
            },
        )
        .await;

        if check.is_err() || check.as_ref().is_ok_and(|o| o.exit_code != 0) {
            return Ok(ToolResult::error("Not in a git repository"));
        }

        let now = chrono::Utc::now().timestamp_millis();
        let branch = input
            .get("branch")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("worktree-{}", now));

        let worktree_path = input
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                let parent = std::path::Path::new(&context.working_dir)
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("/tmp"));
                parent
                    .join(format!(".worktree-{}", branch))
                    .to_string_lossy()
                    .to_string()
            });

        // Create branch if it doesn't exist (ignore errors if it already exists)
        let mut branch_cmd = Command::new("git");
        branch_cmd
            .args(["branch", &branch])
            .current_dir(&context.working_dir);
        let _ = command_runner::run_command(
            &mut branch_cmd,
            &context.abort_signal,
            command_runner::CommandRunOptions {
                timeout: Some(std::time::Duration::from_secs(10)),
                event_sender: None,
                tool_name: "EnterWorktree",
                description: None,
                tool_use_id: context.tool_use_id.as_deref(),
            },
        )
        .await;

        // Create worktree
        let mut add_cmd = Command::new("git");
        add_cmd
            .args(["worktree", "add", &worktree_path, &branch])
            .current_dir(&context.working_dir);
        let result = command_runner::run_command(
            &mut add_cmd,
            &context.abort_signal,
            command_runner::CommandRunOptions {
                timeout: Some(std::time::Duration::from_secs(10)),
                event_sender: None,
                tool_name: "EnterWorktree",
                description: None,
                tool_use_id: context.tool_use_id.as_deref(),
            },
        )
        .await;

        match result {
            Ok(out) if out.exit_code == 0 => {
                let id = uuid::Uuid::new_v4().to_string();
                let info = WorktreeInfo {
                    path: worktree_path.clone(),
                    branch: branch.clone(),
                    original_cwd: context.working_dir.clone(),
                };

                let mut store = self.store.write().await;
                store.insert(id.clone(), info);

                Ok(ToolResult::text(format!(
                    "Worktree created:\n  ID: {}\n  Path: {}\n  Branch: {}\n\nYou are now working in the isolated worktree.",
                    id, worktree_path, branch
                )))
            }
            Ok(out) => {
                let stderr = out.stderr;
                Ok(ToolResult::error(format!(
                    "Error creating worktree: {}",
                    stderr
                )))
            }
            Err(e) => Ok(ToolResult::error(format!("Error creating worktree: {}", e))),
        }
    }
}

// ============================================================================
// ExitWorktreeTool
// ============================================================================

pub struct ExitWorktreeTool {
    store: WorktreeStore,
}

impl ExitWorktreeTool {
    pub fn new(store: WorktreeStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        "ExitWorktree"
    }

    fn description(&self) -> &str {
        "Exit and optionally remove a git worktree. Use \"keep\" to preserve changes or \"remove\" to clean up."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: HashMap::from([
                (
                    "id".to_string(),
                    json!({ "type": "string", "description": "Worktree ID" }),
                ),
                (
                    "action".to_string(),
                    json!({
                        "type": "string",
                        "enum": ["keep", "remove"],
                        "description": "Whether to keep or remove the worktree (default: remove)"
                    }),
                ),
            ]),
            required: vec!["id".to_string()],
            additional_properties: Some(false),
        }
    }

    async fn call(&self, input: Value, context: &ToolUseContext) -> Result<ToolResult, ToolError> {
        let id = input
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing 'id'".to_string()))?;

        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("remove");

        let mut store = self.store.write().await;
        let worktree = match store.get(id) {
            Some(w) => w.clone(),
            None => return Ok(ToolResult::error(format!("Worktree not found: {}", id))),
        };

        let mut branch_cleanup_warning = None;
        if action == "remove" {
            // Remove worktree
            let mut remove_cmd = Command::new("git");
            remove_cmd
                .args(["worktree", "remove", &worktree.path, "--force"])
                .current_dir(&worktree.original_cwd);
            let result = command_runner::run_command(
                &mut remove_cmd,
                &context.abort_signal,
                command_runner::CommandRunOptions {
                    timeout: Some(std::time::Duration::from_secs(10)),
                    event_sender: None,
                    tool_name: "ExitWorktree",
                    description: None,
                    tool_use_id: context.tool_use_id.as_deref(),
                },
            )
            .await;

            match result {
                Ok(out) if out.exit_code == 0 => {}
                Ok(out) => return Ok(ToolResult::error(format!("Error: {}", out.stderr))),
                Err(error) => return Ok(ToolResult::error(format!("Error: {}", error))),
            }

            // The worktree is already removed at this point; report branch cleanup separately.
            let mut branch_cmd = Command::new("git");
            branch_cmd
                .args(["branch", "-D", &worktree.branch])
                .current_dir(&worktree.original_cwd);
            let branch_result = command_runner::run_command(
                &mut branch_cmd,
                &context.abort_signal,
                command_runner::CommandRunOptions {
                    timeout: Some(std::time::Duration::from_secs(10)),
                    event_sender: None,
                    tool_name: "ExitWorktree",
                    description: None,
                    tool_use_id: context.tool_use_id.as_deref(),
                },
            )
            .await;
            match branch_result {
                Ok(out) if out.exit_code == 0 => {}
                Ok(out) => branch_cleanup_warning = Some(out.stderr),
                Err(error) => branch_cleanup_warning = Some(error),
            }
        }

        store.remove(id);

        let mut message = format!(
            "Worktree {}: {}",
            if action == "remove" {
                "removed"
            } else {
                "kept"
            },
            worktree.path
        );
        if let Some(warning) = branch_cleanup_warning {
            message.push_str(&format!("\nBranch cleanup warning: {}", warning));
        }
        Ok(ToolResult::text(message))
    }
}
