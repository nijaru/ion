use super::*;
use crate::{Host, Resources};

fn write_skill(path: &Path, name: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: {body}\n---\n{body}\n"),
    )
    .unwrap();
}

#[test]
fn explicit_files_and_directories_keep_default_precedence_and_can_isolate_skills() {
    let root = std::env::temp_dir().join(format!("ion-selected-skills-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("project");
    let config = root.join("config");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(cwd.join(".ion/prompts")).unwrap();
    fs::write(cwd.join("AGENTS.md"), "PROJECT_RULE").unwrap();
    fs::write(cwd.join(".ion/prompts/check.md"), "CHECK_TEMPLATE").unwrap();
    write_skill(
        &config.join("skills/shared/SKILL.md"),
        "shared",
        "USER_COPY",
    );
    write_skill(
        &cwd.join(".agents/skills/automatic/SKILL.md"),
        "automatic",
        "AUTOMATIC_COPY",
    );
    write_skill(&cwd.join("picked.md"), "shared", "EXPLICIT_COPY");
    write_skill(&cwd.join("bundle/only/SKILL.md"), "only", "BUNDLE_COPY");
    write_skill(
        &cwd.join("bundle/only/examples/nested/SKILL.md"),
        "nested",
        "NOT_A_SEPARATE_SKILL",
    );
    std::os::unix::fs::symlink(".", cwd.join("bundle/only/examples/cycle")).unwrap();
    write_skill(&cwd.join("bundle/loose.md"), "loose", "ROOT_MARKDOWN");
    let paths = vec![
        PathBuf::from("picked.md"),
        PathBuf::from("bundle"),
        PathBuf::from("bundle/only"),
    ];
    for include_defaults in [true, false] {
        let selection = SkillSelection::new(&cwd, include_defaults, paths.clone()).unwrap();
        let resources = Resources::load_selected(&cwd, &config, None, &selection).unwrap();
        let names: Vec<_> = resources.skills().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            if include_defaults {
                vec!["automatic", "loose", "only", "shared"]
            } else {
                vec!["loose", "only", "shared"]
            }
        );
        let expanded = resources
            .expand_command("/skill:shared inspect", 4096)
            .unwrap()
            .unwrap();
        assert!(expanded.contains(if include_defaults {
            "USER_COPY"
        } else {
            "EXPLICIT_COPY"
        }));
        assert!(expanded.contains("User request: inspect"));
        assert!(resources.instructions().contains("PROJECT_RULE"));
        assert_eq!(resources.templates().count(), 1);
        assert_eq!(resources.diagnostics().len(), usize::from(include_defaults));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn aliases_are_deduplicated_but_distinct_names_and_invalid_paths_are_diagnosed() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("ion-skill-aliases-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    write_skill(&root.join("bundle/check/SKILL.md"), "check", "FIRST");
    write_skill(&root.join("other.md"), "check", "SECOND");
    fs::write(root.join("not-markdown.txt"), "not a skill").unwrap();
    symlink(root.join("bundle/check/SKILL.md"), root.join("alias.md")).unwrap();
    symlink(root.join("bundle/check"), root.join("wrong-name")).unwrap();
    let selection = SkillSelection::new(
        &root,
        false,
        [
            "wrong-name",
            "bundle",
            "alias.md",
            "bundle/check",
            "other.md",
            "missing.md",
            "not-markdown.txt",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
    )
    .unwrap();
    let resources =
        Resources::load_selected(&root, &root.join("config"), None, &selection).unwrap();
    assert_eq!(resources.skills().count(), 1);
    let expanded = resources
        .expand_command("/skill:check", 4096)
        .unwrap()
        .unwrap();
    assert!(expanded.contains("FIRST") && !expanded.contains("SECOND"));
    let diagnostics: Vec<_> = resources
        .diagnostics()
        .iter()
        .map(|d| d.path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(
        diagnostics,
        ["SKILL.md", "other.md", "missing.md", "not-markdown.txt"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn host_selection_keeps_anchored_paths_across_workspaces_and_refreshes_content() {
    let root = std::env::temp_dir().join(format!("ion-skill-capture-{}", uuid::Uuid::now_v7()));
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir_all(&second).unwrap();
    write_skill(&first.join("chosen.md"), "chosen", "CAPTURED");
    write_skill(&second.join("chosen.md"), "chosen", "WRONG_WORKSPACE");
    let selection = SkillSelection::new(&first, false, vec![PathBuf::from("chosen.md")]).unwrap();
    let host = Host::new(root.join("config"), root.join("state")).with_skills(selection);
    for cwd in [&first, &second] {
        let resources = host.resources(cwd).unwrap();
        assert_eq!(
            resources.skills().next().unwrap().path,
            first.canonicalize().unwrap().join("chosen.md")
        );
        assert!(
            resources
                .expand_command("/skill:chosen", 4096)
                .unwrap()
                .unwrap()
                .contains("CAPTURED")
        );
    }
    write_skill(&first.join("chosen.md"), "chosen", "RELOADED");
    assert!(
        host.resources(&second)
            .unwrap()
            .expand_command("/skill:chosen", 4096)
            .unwrap()
            .unwrap()
            .contains("RELOADED")
    );
    fs::remove_dir_all(root).unwrap();
}
