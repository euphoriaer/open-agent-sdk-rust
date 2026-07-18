use std::collections::{HashMap, HashSet};

use crate::types::skill::LoadedSkill;

pub struct SkillRegistry {
    skills: HashMap<String, LoadedSkill>,
    disabled: HashSet<String>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            disabled: HashSet::new(),
        }
    }

    /// Register a skill. If a skill with the same name exists, ignore (first wins).
    pub fn register(&mut self, skill: LoadedSkill) {
        self.skills.entry(skill.name.clone()).or_insert(skill);
    }

    /// Set the disabled skills set (from database).
    pub fn set_disabled(&mut self, disabled: HashSet<String>) {
        self.disabled = disabled;
    }

    /// Get all registered skills (including disabled).
    pub fn all(&self) -> Vec<&LoadedSkill> {
        self.skills.values().collect()
    }

    /// Get all enabled skills.
    pub fn all_enabled(&self) -> Vec<&LoadedSkill> {
        self.skills
            .values()
            .filter(|s| !self.disabled.contains(&s.name))
            .collect()
    }

    /// Get a skill by name (regardless of enabled/disabled).
    pub fn get(&self, name: &str) -> Option<&LoadedSkill> {
        self.skills.get(name)
    }

    /// Remove a skill by name from the registry.
    pub fn remove(&mut self, name: &str) {
        self.skills.remove(name);
        self.disabled.remove(name);
    }

    /// Get skills that the model can auto-invoke (enabled + not disable_model_invocation).
    pub fn auto_invocable(&self) -> Vec<&LoadedSkill> {
        self.skills
            .values()
            .filter(|s| !self.disabled.contains(&s.name) && !s.metadata.disable_model_invocation)
            .collect()
    }

    /// Get skills that the user can invoke (enabled + user_invocable).
    pub fn user_invocable(&self) -> Vec<&LoadedSkill> {
        self.skills
            .values()
            .filter(|s| !self.disabled.contains(&s.name) && s.metadata.user_invocable)
            .collect()
    }

    /// Generate a summary for system prompt injection (only enabled skills).
    pub fn generate_context_summary(&self) -> String {
        let skills = self.all_enabled();
        if skills.is_empty() {
            return String::new();
        }
        let mut summary = String::from(
            "# Available Skills\n\nYou have access to the following skills via the Skill tool:\n\n",
        );
        for skill in &skills {
            summary.push_str(&format!("- **{}**", skill.name));
            if let Some(desc) = &skill.metadata.description {
                let desc = if desc.len() > 250 {
                    format!("{}...", &desc[..250])
                } else {
                    desc.clone()
                };
                summary.push_str(&format!(": {}", desc));
            }
            if let Some(when) = &skill.metadata.when_to_use {
                summary.push_str(&format!(" TRIGGER when: {}", when));
            }
            if let Some(hint) = &skill.metadata.argument_hint {
                summary.push_str(&format!(" (usage: /{})", hint));
            }
            summary.push('\n');
        }
        summary.push_str(
            "\nWhen a skill matches the user's request, invoke it using the Skill tool. \
             When the Skill tool returns instructions, treat them as mandatory operational \
             procedures, not as optional reference material. Follow the instructions step by step.\n",
        );
        summary
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}
