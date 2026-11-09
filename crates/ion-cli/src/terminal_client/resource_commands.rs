//! Native discovery and reload feedback over the current frozen catalogs.
use std::path::Path;

use ion_app::{CommandCatalog, Resources};

use crate::terminal_commands::Builtin;

fn source(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

fn warnings(resources: &Resources, cwd: &Path) -> Vec<String> {
    let mut lines = resources
        .diagnostics()
        .iter()
        .map(|diagnostic| format!("{}: {}", source(&diagnostic.path, cwd), diagnostic.message))
        .collect::<Vec<_>>();
    for template in resources.templates() {
        let command = format!("/{}", template.name);
        if Builtin::parse(&command).is_some() {
            lines.push(format!(
                "{}: {command} is a built-in command in terminal chat; rename this prompt template to invoke it here",
                source(&template.path, cwd)
            ));
        }
    }
    lines
}

fn with_warnings(mut lines: Vec<String>, warnings: Vec<String>) -> String {
    if !warnings.is_empty() {
        lines.push("Warnings:".into());
        lines.extend(warnings);
        lines.push("Review the files, fix the reported issues, then /reload.".into());
    }
    lines.join("\n")
}

pub(super) fn skills(resources: &Resources, cwd: &Path) -> String {
    let mut lines = vec!["Skills · invoke /skill:NAME [request]".into()];
    for skill in resources.skills() {
        lines.push(format!("/skill:{} — {}", skill.name, skill.description));
        lines.push(format!("  {}", source(&skill.path, cwd)));
    }
    if resources.skills().next().is_none() {
        lines.push("No skills loaded. With default discovery, add .agents/skills/NAME/SKILL.md and /reload. Select explicit sources at startup with --skill PATH.".into());
    }
    with_warnings(lines, warnings(resources, cwd))
}

pub(super) fn prompts(resources: &Resources, cwd: &Path) -> String {
    let mut lines = vec!["Prompt templates · invoke /NAME [arguments]".into()];
    for template in resources.templates() {
        let mut command = format!("/{}", template.name);
        let unavailable = if Builtin::parse(&command).is_some() {
            " [built-in conflict]"
        } else {
            ""
        };
        if let Some(hint) = template
            .argument_hint
            .as_deref()
            .filter(|hint| !hint.is_empty())
        {
            command.push(' ');
            command.push_str(hint);
        }
        lines.push(format!("{command}{unavailable} — {}", template.description));
        lines.push(format!("  {}", source(&template.path, cwd)));
    }
    if resources.templates().next().is_none() {
        lines.push("No prompt templates loaded. With default discovery, add .ion/prompts/NAME.md and /reload. Select explicit sources at startup with --prompt-template PATH.".into());
    }
    with_warnings(lines, warnings(resources, cwd))
}

pub(super) fn reloaded(resources: &Resources, commands: &CommandCatalog, cwd: &Path) -> String {
    let mut warnings = warnings(resources, cwd);
    warnings.extend(commands.diagnostics().iter().cloned());
    with_warnings(
        vec![
            format!(
                "Reloaded keybindings, theme, resources and commands ({} diagnostic(s))",
                warnings.len()
            ),
            format!(
                "Loaded {} skill(s), {} prompt(s), {} command(s)",
                resources.skills().count(),
                resources.templates().count(),
                commands.iter().count()
            ),
        ],
        warnings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_app::{ResourcePaths, ResourceSelection};
    use std::{fs, path::PathBuf};

    fn catalog() -> (PathBuf, Resources) {
        let root = std::env::temp_dir().join(format!("ion-resource-ui-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("skills/audit")).unwrap();
        fs::create_dir_all(root.join("prompts")).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(
            root.join("skills/audit/SKILL.md"),
            "---\nname: audit\ndescription: Review café changes\n---\nPRIVATE_SKILL_BODY",
        )
        .unwrap();
        fs::write(root.join("prompts/check.md"), "---\ndescription: Check a concern\nargument-hint: CONCERN\n---\nPRIVATE_TEMPLATE_BODY $1").unwrap();
        fs::write(
            root.join("prompts/broken.md"),
            "---\ndescription: [unclosed\n---\n",
        )
        .unwrap();
        fs::write(
            root.join("prompts/help.md"),
            "---\ndescription: A conflicting name\n---\nPRIVATE_CONFLICT_BODY",
        )
        .unwrap();
        let selection = ResourceSelection {
            skills: ResourcePaths::new(&root, false, vec![root.join("skills")]).unwrap(),
            prompt_templates: ResourcePaths::new(&root, false, vec![root.join("prompts")]).unwrap(),
        };
        let resources = Resources::load(&root, &root.join("config"), &selection).unwrap();
        fs::remove_dir_all(&root).unwrap();
        (root, resources)
    }

    #[test]
    fn discovery_shows_invocations_sources_and_unavailable_prompts_without_bodies() {
        let (cwd, resources) = catalog();
        let skill = skills(&resources, &cwd);
        assert!(
            skill.contains("/skill:audit — Review café changes"),
            "{skill}"
        );
        assert!(skill.contains("  skills/audit/SKILL.md"), "{skill}");
        let prompt = prompts(&resources, &cwd);
        assert!(
            prompt.contains("/check CONCERN — Check a concern"),
            "{prompt}"
        );
        assert!(prompt.contains("  prompts/check.md"), "{prompt}");
        assert!(prompt.contains("/help [built-in conflict]"), "{prompt}");
        for listing in [skill, prompt] {
            assert!(!listing.contains("PRIVATE_"), "{listing}");
            assert!(
                listing.contains("prompts/broken.md: invalid YAML frontmatter"),
                "{listing}"
            );
            assert!(listing.contains("rename this prompt template"), "{listing}");
        }
    }

    #[test]
    fn reload_surfaces_file_reasons_and_current_counts_not_just_a_warning_total() {
        let (cwd, resources) = catalog();
        let notice = reloaded(&resources, &CommandCatalog::default(), &cwd);
        assert!(notice.contains("(2 diagnostic(s))"), "{notice}");
        assert!(
            notice.contains("Loaded 1 skill(s), 2 prompt(s), 0 command(s)"),
            "{notice}"
        );
        assert!(
            notice.contains("prompts/broken.md: invalid YAML frontmatter"),
            "{notice}"
        );
        assert!(
            notice.contains("prompts/help.md: /help is a built-in command"),
            "{notice}"
        );
        assert!(notice.contains("then /reload"), "{notice}");
    }

    #[test]
    fn empty_catalogs_explain_how_to_add_enabled_sources() {
        let resources = Resources::new(String::new());
        let cwd = Path::new("/workspace");
        assert!(skills(&resources, cwd).contains(".agents/skills/NAME/SKILL.md"));
        assert!(prompts(&resources, cwd).contains(".ion/prompts/NAME.md"));
        assert!(skills(&resources, cwd).contains("--skill PATH"));
        assert!(prompts(&resources, cwd).contains("--prompt-template PATH"));
    }
}
