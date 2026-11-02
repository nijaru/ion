use super::*;
use crate::{ResourceSelection, Resources};

#[test]
fn substitution_preserves_semantics_under_raw_argument_and_output_byte_caps() {
    // Output may be smaller than source; budgets count bytes, not characters.
    let cases = [
        ("$1", "x", 1, Some("x")),
        ("$9$9$9$9", "", 0, Some("")),
        ("$1", "🦀", 4, Some("🦀")),
        ("$1", "🦀", 3, None),
        ("[$1]", "é", 4, Some("[é]")),
        ("[$1]", "é", 3, None),
        ("$1$1", "ab", 4, Some("abab")),
        ("$1$1", "ab", 3, None),
        ("$@", "one two", 7, Some("one two")),
        ("$@", "one two", 6, None),
        ("${@:2:1}", "a bb", 4, Some("bb")),
        ("${@:0:0}", "a bb", 4, Some("")),
        ("${2:-fallback}", "", 8, Some("fallback")),
        ("${2:-fallback}", "", 7, None),
        ("${@:-fallback}", "''", 8, Some("fallback")),
        ("${@:-fallback}", "'' ''", 5, Some(" ")),
        ("${ARGUMENTS:-x}", "", 1, Some("x")),
        ("$ARGUMENTS/$@/$1", "a", 5, Some("a/a/a")),
        ("$0:$999999999999999999999", "", 1, Some(":")),
        ("${bad}", "", 6, Some("${bad}")),
        ("$1", r"a\ b", 4, Some("a b")),
        ("$1", "'$@'", 4, Some("$@")),
        ("$1", "'unterminated", 64, None),
        ("$1", "unfinished\\", 64, None),
    ];
    for (body, args, budget, expected) in cases {
        let template = PromptTemplate {
            name: "bounded".into(),
            description: String::new(),
            argument_hint: None,
            path: PathBuf::new(),
            body: body.into(),
        };
        let actual = template.expand(args, budget);
        match expected {
            Some(value) => assert_eq!(actual.unwrap(), value, "{body:?}, {args:?}, {budget}"),
            None => assert!(actual.is_err(), "{body:?}, {args:?}, {budget}: {actual:?}"),
        }
    }
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn explicit_templates_are_shallow_ordered_and_independent_of_skills_and_instructions() {
    let root =
        std::env::temp_dir().join(format!("ion-selected-templates-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("project");
    let config = root.join("config");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    write(&cwd.join("AGENTS.md"), "PROJECT_RULE");
    write(
        &config.join("skills/retained/SKILL.md"),
        "---\nname: retained\ndescription: Retained skill\n---\nSKILL_BODY",
    );
    write(&config.join("prompts/check.md"), "USER $1");
    write(&cwd.join(".ion/prompts/check.md"), "PROJECT $1");
    write(&cwd.join(".ion/prompts/automatic.md"), "AUTOMATIC");
    write(&cwd.join("picked/check.md"), "EXPLICIT $1");
    write(
        &cwd.join("bundle/chosen.md"),
        "---\ndescription: Selected command\nargument-hint: TARGET\n---\nCHOSEN $1",
    );
    write(&cwd.join("bundle/nested/ignored.md"), "NOT_RECURSIVE");
    let paths = vec![PathBuf::from("picked/check.md"), PathBuf::from("bundle")];
    for include_defaults in [true, false] {
        let selection = ResourceSelection {
            prompt_templates: ResourcePaths::new(&cwd, include_defaults, paths.clone()).unwrap(),
            ..ResourceSelection::default()
        };
        let resources = Resources::load_selected(&cwd, &config, None, &selection).unwrap();
        assert_eq!(
            resources
                .templates()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            if include_defaults {
                vec!["automatic", "check", "chosen"]
            } else {
                vec!["check", "chosen"]
            }
        );
        assert_eq!(
            resources
                .expand_command("/check target", 4096)
                .unwrap()
                .unwrap(),
            if include_defaults {
                "USER target"
            } else {
                "EXPLICIT target"
            }
        );
        let chosen = resources.templates().find(|t| t.name == "chosen").unwrap();
        assert_eq!(chosen.description, "Selected command");
        assert_eq!(chosen.argument_hint.as_deref(), Some("TARGET"));
        assert_eq!(resources.skills().count(), 1);
        assert!(resources.instructions().contains("PROJECT_RULE"));
        assert!(!resources.instructions().contains("CHOSEN"));
        assert_eq!(
            resources.diagnostics().len(),
            if include_defaults { 2 } else { 0 }
        );
    }
    let selection = ResourceSelection {
        prompt_templates: ResourcePaths::new(&cwd, false, vec![]).unwrap(),
        ..ResourceSelection::default()
    };
    let empty = Resources::load_selected(&cwd, &config, None, &selection).unwrap();
    assert_eq!(empty.templates().count(), 0);
    assert_eq!(empty.skills().count(), 1);
    assert!(empty.instructions().contains("PROJECT_RULE"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn template_aliases_keep_filename_commands_without_duplicate_or_invalid_sources() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("ion-template-aliases-{}", uuid::Uuid::now_v7()));
    write(&root.join("bundle/review.md"), "FIRST $1");
    write(&root.join("other/review.md"), "SECOND $1");
    symlink(root.join("bundle"), root.join("bundle-link")).unwrap();
    symlink(root.join("bundle/review.md"), root.join("inspect.md")).unwrap();
    symlink(root.join("bundle/review.md"), root.join("BAD.md")).unwrap();
    fs::write(root.join("wrong.txt"), "not a template").unwrap();
    let paths = [
        "BAD.md",
        "bundle",
        "bundle-link",
        "bundle/review.md",
        "inspect.md",
        "other/review.md",
        "missing.md",
        "wrong.txt",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let selection = ResourceSelection {
        prompt_templates: ResourcePaths::new(&root, false, paths).unwrap(),
        ..ResourceSelection::default()
    };
    let resources =
        Resources::load_selected(&root, &root.join("config"), None, &selection).unwrap();
    assert_eq!(
        resources
            .templates()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["inspect", "review"]
    );
    for name in ["inspect", "review"] {
        assert_eq!(
            resources
                .expand_command(&format!("/{name} target"), 4096)
                .unwrap()
                .unwrap(),
            "FIRST target"
        );
    }
    assert_eq!(
        resources
            .diagnostics()
            .iter()
            .map(|d| d.path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        ["BAD.md", "review.md", "missing.md", "wrong.txt"]
    );
    fs::remove_dir_all(root).unwrap();
}
