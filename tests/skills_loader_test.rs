use open_agent_sdk::skills::load_all_global;
use open_agent_sdk::types::SkillSource;

fn write_skill(dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {}\ndescription: Test skill\n---\n\n# {}\n",
            name, name
        ),
    )
    .unwrap();
}

#[test]
fn load_all_global_includes_codex_skills() {
    let home = tempfile::tempdir().unwrap();
    write_skill(&home.path().join(".codex/skills/demo"), "demo");

    let skills = load_all_global(home.path());

    let skill = skills.iter().find(|s| s.name == "demo").unwrap();
    assert_eq!(skill.source, SkillSource::Codex);
    assert_eq!(skill.source.as_str(), "codex");
}

#[test]
fn load_all_global_skips_hidden_codex_skill_dirs() {
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &home.path().join(".codex/skills/.system/internal"),
        "internal",
    );
    write_skill(&home.path().join(".codex/skills/visible"), "visible");

    let names = load_all_global(home.path())
        .into_iter()
        .map(|s| s.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"visible".to_string()));
    assert!(!names.contains(&"internal".to_string()));
}
