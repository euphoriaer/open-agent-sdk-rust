use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::skills::loader::{load_skill_from_dir, parse_skill_file};
use crate::skills::SkillRegistry;
use crate::types::skill::SkillSource;
use crate::types::{Tool, ToolError, ToolInputSchema, ToolResult, ToolUseContext};

pub struct SkillManager {
    home_dir: PathBuf,
    registry: Arc<RwLock<SkillRegistry>>,
}

impl SkillManager {
    pub fn new(home_dir: PathBuf, registry: Arc<RwLock<SkillRegistry>>) -> Self {
        Self { home_dir, registry }
    }

    fn aqbot_skills_dir(&self) -> PathBuf {
        self.home_dir.join(".aqbot").join("skills")
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.aqbot_skills_dir().join(name)
    }
}

#[async_trait]
impl Tool for SkillManager {
    fn name(&self) -> &str {
        "SkillManager"
    }

    fn description(&self) -> &str {
        "Manage installed skills. Actions: list (list all installed skills with their status), install (install a new skill from SKILL.md content), remove (uninstall a skill by name). Installed skills persist across sessions."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: HashMap::from([
                (
                    "action".to_string(),
                    json!({
                        "type": "string",
                        "enum": ["list", "install", "remove"],
                        "description": "Action to perform"
                    }),
                ),
                (
                    "name".to_string(),
                    json!({
                        "type": "string",
                        "description": "Skill name (required for install and remove)"
                    }),
                ),
                (
                    "content".to_string(),
                    json!({
                        "type": "string",
                        "description": "Full SKILL.md content including YAML frontmatter (required for install)"
                    }),
                ),
            ]),
            required: vec!["action".to_string()],
            additional_properties: Some(false),
        }
    }

    fn is_read_only(&self, input: &Value) -> bool {
        input
            .get("action")
            .and_then(|v| v.as_str())
            .map(|a| a == "list")
            .unwrap_or(false)
    }

    async fn call(
        &self,
        input: Value,
        _context: &ToolUseContext,
    ) -> Result<ToolResult, ToolError> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing 'action' field".to_string()))?;

        match action {
            "list" => self.list_skills().await,
            "install" => self.install_skill(&input).await,
            "remove" => self.remove_skill(&input).await,
            _ => Err(ToolError::InvalidInput(format!(
                "Unknown action '{}'. Valid actions: list, install, remove",
                action
            ))),
        }
    }
}

impl SkillManager {
    async fn list_skills(&self) -> Result<ToolResult, ToolError> {
        let registry = self.registry.read().await;
        let all = registry.all();
        if all.is_empty() {
            return Ok(ToolResult::text(
                "No skills installed. Use 'install' action to add a skill.",
            ));
        }

        let enabled_names: std::collections::HashSet<String> = registry
            .all_enabled()
            .iter()
            .map(|s| s.name.clone())
            .collect();

        let mut lines = vec![format!("Found {} installed skills:\n", all.len())];
        for skill in all {
            let status = if enabled_names.contains(&skill.name) {
                "enabled"
            } else {
                "disabled"
            };
            let desc = skill
                .metadata
                .description
                .as_deref()
                .unwrap_or("(no description)");
            let source = skill.source.as_str();
            lines.push(format!(
                "- **{}** [{}] ({}) — {}",
                skill.name, status, source, desc
            ));
            if let Some(when) = &skill.metadata.when_to_use {
                lines.push(format!("  TRIGGER when: {}", when));
            }
            if let Some(hint) = &skill.metadata.argument_hint {
                lines.push(format!("  Usage: /{}", hint));
            }
            lines.push(String::new());
        }

        Ok(ToolResult::text(lines.join("\n")))
    }

    async fn install_skill(&self, input: &Value) -> Result<ToolResult, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidInput("Missing or empty 'name' field for install".to_string())
            })?;

        let content = input.get("content").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidInput(
                "Missing 'content' field for install (expected full SKILL.md content)"
                    .to_string(),
            )
        })?;

        // Validate content has proper YAML frontmatter
        if parse_skill_file(content).is_none() {
            return Err(ToolError::InvalidInput(
                "Skill content must start with '---' YAML frontmatter".to_string(),
            ));
        }

        // Create skill directory
        let dir = self.skill_dir(name);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| ToolError::ExecutionError(format!("Failed to create skill directory: {}", e)))?;

        // Write SKILL.md
        let skill_path = dir.join("SKILL.md");
        tokio::fs::write(&skill_path, content)
            .await
            .map_err(|e| ToolError::ExecutionError(format!("Failed to write SKILL.md: {}", e)))?;

        // Load and register the skill in-memory
        let skill = load_skill_from_dir(&dir, SkillSource::AQBot).ok_or_else(|| {
            // Clean up on failure
            let _ = std::fs::remove_dir_all(&dir);
            ToolError::ExecutionError("Failed to load installed skill".to_string())
        })?;

        {
            let mut registry = self.registry.write().await;
            registry.register(skill);
        }

        Ok(ToolResult::text(format!(
            "Skill '{}' installed successfully.\n\
             Path: {}\n\
             The skill is now available via the Skill tool.",
            name,
            skill_path.display()
        )))
    }

    async fn remove_skill(&self, input: &Value) -> Result<ToolResult, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidInput("Missing or empty 'name' field for remove".to_string())
            })?;

        let dir = self.skill_dir(name);

        // Check if the skill directory exists
        if !tokio::fs::metadata(&dir).await.is_ok() {
            return Err(ToolError::InvalidInput(format!(
                "Skill '{}' is not installed (directory not found at {})",
                name,
                dir.display()
            )));
        }

        // Remove from filesystem
        tokio::fs::remove_dir_all(&dir)
            .await
            .map_err(|e| ToolError::ExecutionError(format!("Failed to remove skill directory: {}", e)))?;

        // Remove from in-memory registry
        {
            let mut registry = self.registry.write().await;
            registry.remove(name);
        }

        Ok(ToolResult::text(format!(
            "Skill '{}' removed successfully.",
            name
        )))
    }
}
