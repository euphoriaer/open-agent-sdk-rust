use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

/// SKILL.md YAML frontmatter metadata.
/// Uses kebab-case in YAML, mapped to snake_case in Rust via serde rename_all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SkillMetadata {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub argument_hint: Option<String>,
    #[serde(default)]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub disable_model_invocation: bool,
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    #[serde(default, deserialize_with = "deserialize_allowed_tools")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Custom deserializer for allowed-tools field.
/// Supports YAML array or space-separated string (Claude Code compat).
fn deserialize_allowed_tools<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum AllowedToolsFormat {
        Array(Vec<String>),
        SpaceSeparated(String),
    }

    let opt = Option::<AllowedToolsFormat>::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(AllowedToolsFormat::Array(v)) => Ok(Some(v)),
        Some(AllowedToolsFormat::SpaceSeparated(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.split_whitespace().map(String::from).collect()))
            }
        }
    }
}

/// Where a skill was loaded from (priority order, highest first).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkillSource {
    AQBot,
    Codex,
    Claude,
    Agents,
    Project,
}

impl SkillSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillSource::AQBot => "aqbot",
            SkillSource::Codex => "codex",
            SkillSource::Claude => "claude",
            SkillSource::Agents => "agents",
            SkillSource::Project => "project",
        }
    }
}

/// A fully loaded skill with parsed metadata and content.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub path: PathBuf,
    pub metadata: SkillMetadata,
    /// Markdown body (after frontmatter)
    pub content: String,
    pub source: SkillSource,
    /// If this skill is part of a group/collection, the group name
    pub group: Option<String>,
}
