use std::path::{Path, PathBuf};

use crate::types::skill::{LoadedSkill, SkillMetadata, SkillSource};

fn is_hidden_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

/// Parse a SKILL.md file into (metadata, markdown_body).
/// Frontmatter is delimited by `---` lines at the start of the file.
pub fn parse_skill_file(content: &str) -> Option<(SkillMetadata, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the closing `---`
    let after_first = &trimmed[3..];
    let rest = after_first.trim_start_matches(['\r', '\n']);
    let end_idx = rest.find("\n---")?;
    let yaml_str = &rest[..end_idx];
    let after_closing = &rest[end_idx + 4..]; // skip "\n---"
    let body = after_closing.trim_start_matches(['\r', '\n']).to_string();

    let metadata: SkillMetadata = serde_yaml::from_str(yaml_str).ok()?;
    Some((metadata, body))
}

/// Load a single skill from a directory containing SKILL.md.
pub fn load_skill_from_dir(dir: &Path, source: SkillSource) -> Option<LoadedSkill> {
    let skill_file = dir.join("SKILL.md");
    let content = std::fs::read_to_string(&skill_file).ok()?;
    let (metadata, body) = parse_skill_file(&content)?;

    let name = metadata.name.clone();
    let name = if name.is_empty() {
        dir.file_name()?.to_string_lossy().to_string()
    } else {
        name
    };

    Some(LoadedSkill {
        name,
        path: skill_file,
        metadata,
        content: body,
        source,
        group: None,
    })
}

/// Scan all subdirectories of `parent_dir` for SKILL.md files.
pub fn load_from_dir(parent_dir: &Path, source: SkillSource) -> Vec<LoadedSkill> {
    let entries = match std::fs::read_dir(parent_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_hidden_dir(&path) {
            if let Some(skill) = load_skill_from_dir(&path, source.clone()) {
                skills.push(skill);
            } else {
                // No SKILL.md — treat as a group container and recurse
                let group_name = match path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => continue,
                };
                let mut group_skills = Vec::new();
                collect_skills_recursive(&path, source.clone(), &mut group_skills, 0, 3);
                for mut skill in group_skills {
                    skill.group = Some(group_name.clone());
                    skills.push(skill);
                }
            }
        }
    }
    skills
}

/// Recursively collect skills from nested directories up to `max_depth`.
fn collect_skills_recursive(
    dir: &Path,
    source: SkillSource,
    skills: &mut Vec<LoadedSkill>,
    depth: u32,
    max_depth: u32,
) {
    if depth >= max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_hidden_dir(&path) {
            if let Some(skill) = load_skill_from_dir(&path, source.clone()) {
                skills.push(skill);
            } else {
                collect_skills_recursive(&path, source.clone(), skills, depth + 1, max_depth);
            }
        }
    }
}

fn global_skill_dirs(home_dir: &Path) -> Vec<(PathBuf, SkillSource)> {
    vec![
        (home_dir.join(".aqbot").join("skills"), SkillSource::AQBot),
        (home_dir.join(".claude").join("skills"), SkillSource::Claude),
        (home_dir.join(".agents").join("skills"), SkillSource::Agents),
        (home_dir.join(".codex").join("skills"), SkillSource::Codex),
    ]
}

/// Load skills from all global directories in priority order.
/// Deduplicates by name — first found wins.
pub fn load_all_global(home_dir: &Path) -> Vec<LoadedSkill> {
    let dirs = global_skill_dirs(home_dir);

    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (dir, source) in dirs {
        for skill in load_from_dir(&dir, source) {
            if seen.insert(skill.name.clone()) {
                result.push(skill);
            }
        }
    }
    result
}

/// Load all global skills without deduplication for management UIs.
pub fn load_all_global_for_management(home_dir: &Path) -> Vec<LoadedSkill> {
    global_skill_dirs(home_dir)
        .into_iter()
        .flat_map(|(dir, source)| load_from_dir(&dir, source))
        .collect()
}

/// Walk up from `cwd` looking for `.git/` or `.aqbot/` to detect the project root.
pub fn detect_project_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").is_dir() || current.join(".aqbot").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Load skills from the project's `.aqbot/skills/` directory.
pub fn load_project_skills(cwd: &Path) -> Vec<LoadedSkill> {
    let root = match detect_project_root(cwd) {
        Some(r) => r,
        None => return Vec::new(),
    };
    load_from_dir(&root.join(".aqbot").join("skills"), SkillSource::Project)
}

/// Substitute template variables in skill content.
/// Replaces `$ARGUMENTS`, `${SKILL_DIR}`, and `${CLAUDE_SKILL_DIR}` (compat).
pub fn substitute_variables(content: &str, arguments: &str, skill_dir: &str) -> String {
    content
        .replace("$ARGUMENTS", arguments)
        .replace("${SKILL_DIR}", skill_dir)
        .replace("${CLAUDE_SKILL_DIR}", skill_dir)
}
