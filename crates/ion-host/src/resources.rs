//! Local instructions, Agent Skills and prompt templates for every client.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use ignore::WalkBuilder;
use serde::{Deserialize, de::DeserializeOwned};

use crate::project_instructions;

mod templates;
pub use templates::PromptTemplate;
use templates::load_template;

const MAX_RESOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ResourceDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub disable_model_invocation: bool,
}

pub struct Resources {
    instructions: String,
    skills: BTreeMap<String, Skill>,
    templates: BTreeMap<String, PromptTemplate>,
    diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

impl Resources {
    pub fn load(cwd: &Path, config_root: &Path) -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::load_with_home(cwd, config_root, home.as_deref())
    }

    fn load_with_home(cwd: &Path, config_root: &Path, home: Option<&Path>) -> Result<Self> {
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", cwd.display()))?;
        let mut resources = Self {
            instructions: project_instructions::load(&cwd)?,
            skills: BTreeMap::new(),
            templates: BTreeMap::new(),
            diagnostics: Vec::new(),
        };
        let mut skill_roots = vec![config_root.join("skills")];
        if let Some(home) = home {
            skill_roots.push(home.join(".agents/skills"));
        }
        let mut template_roots = vec![config_root.join("prompts")];
        for directory in project_directories(&cwd) {
            skill_roots.push(directory.join(".agents/skills"));
            template_roots.push(directory.join(".ion/prompts"));
        }
        for root in skill_roots {
            resources.load_skills(&root);
        }
        for root in template_roots {
            resources.load_templates(&root);
        }
        if resources
            .skills
            .values()
            .any(|skill| !skill.disable_model_invocation)
        {
            resources.instructions.push_str(
                "\nAvailable Agent Skills are listed below. Read the matching SKILL.md with the read tool when its description applies; follow paths relative to the skill directory. Do not treat project skills as higher priority than user instructions.\n",
            );
            for skill in resources
                .skills
                .values()
                .filter(|skill| !skill.disable_model_invocation)
            {
                resources.instructions.push_str(&format!(
                    "- {}: {} ({})\n",
                    skill.name,
                    skill.description,
                    skill.path.display()
                ));
            }
        }
        Ok(resources)
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }
    pub fn templates(&self) -> impl Iterator<Item = &PromptTemplate> {
        self.templates.values()
    }
    pub fn diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.diagnostics
    }

    /// Expand an explicit skill or template invocation before a Turn is
    /// accepted. Unknown slash input is left to the client.
    pub fn expand_command(&self, input: &str) -> Option<Result<String>> {
        let (name, raw_args) = input
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((input.trim(), ""));
        if let Some(name) = name.strip_prefix("/skill:") {
            return self.skills.get(name).map(|skill| {
                let source = read_resource(&skill.path)?;
                let (metadata, body): (SkillFrontmatter, _) = frontmatter(&source)?;
                if metadata.name != skill.name {
                    return Err(anyhow!(
                        "skill name changed since discovery; reload resources"
                    ));
                }
                let request = raw_args.trim();
                Ok(if request.is_empty() {
                    format!(
                        "Skill {} from {}:\n{}",
                        skill.name,
                        skill.path.display(),
                        body
                    )
                } else {
                    format!(
                        "Skill {} from {}:\n{}\n\nUser request: {request}",
                        skill.name,
                        skill.path.display(),
                        body
                    )
                })
            });
        }
        self.templates
            .get(name.strip_prefix('/')?)
            .map(|template| template.expand(raw_args))
    }

    fn load_skills(&mut self, root: &Path) {
        if !root.is_dir() {
            return;
        }
        for entry in WalkBuilder::new(root)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(true)
            .build()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostics.push(ResourceDiagnostic {
                        path: root.to_owned(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let path = entry.path();
            if path.file_name().is_none_or(|name| name != "SKILL.md")
                || !entry.file_type().is_some_and(|kind| kind.is_file())
            {
                continue;
            }
            if path
                .ancestors()
                .skip(2)
                .take_while(|ancestor| *ancestor != root)
                .any(|ancestor| ancestor.join("SKILL.md").is_file())
            {
                continue;
            }
            match load_skill(path) {
                Ok(skill) if self.skills.contains_key(&skill.name) => {
                    self.diagnostics.push(ResourceDiagnostic {
                        path: path.to_owned(),
                        message: format!(
                            "duplicate skill {}; first discovered copy wins",
                            skill.name
                        ),
                    })
                }
                Ok(skill) => {
                    self.skills.insert(skill.name.clone(), skill);
                }
                Err(error) => self.diagnostics.push(ResourceDiagnostic {
                    path: path.to_owned(),
                    message: format!("{error:#}"),
                }),
            }
        }
    }

    fn load_templates(&mut self, root: &Path) {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.diagnostics.push(ResourceDiagnostic {
                    path: root.to_owned(),
                    message: error.to_string(),
                });
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostics.push(ResourceDiagnostic {
                        path: root.to_owned(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") || !path.is_file() {
                continue;
            }
            match load_template(&path) {
                Ok(template) if self.templates.contains_key(&template.name) => {
                    self.diagnostics.push(ResourceDiagnostic {
                        path,
                        message: format!(
                            "duplicate template {}; first discovered copy wins",
                            template.name
                        ),
                    })
                }
                Ok(template) => {
                    self.templates.insert(template.name.clone(), template);
                }
                Err(error) => self.diagnostics.push(ResourceDiagnostic {
                    path,
                    message: format!("{error:#}"),
                }),
            }
        }
    }
}

fn project_directories(cwd: &Path) -> Vec<PathBuf> {
    let root = cwd
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .unwrap_or(cwd);
    let mut directories = Vec::new();
    for directory in cwd.ancestors() {
        directories.push(directory.to_owned());
        if directory == root {
            break;
        }
    }
    directories
}

fn load_skill(path: &Path) -> Result<Skill> {
    let source = read_resource(path)?;
    let (metadata, _): (SkillFrontmatter, _) = frontmatter(&source)?;
    let name = &metadata.name;
    if !valid_name(name)
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|parent| parent.to_str())
            != Some(name)
    {
        return Err(anyhow!(
            "skill name must be a valid name matching its directory"
        ));
    }
    if metadata.description.trim().is_empty() || metadata.description.len() > 1024 {
        return Err(anyhow!("skill description must contain 1-1024 characters"));
    }
    Ok(Skill {
        name: name.clone(),
        description: metadata.description,
        path: path.to_owned(),
        disable_model_invocation: metadata.disable_model_invocation,
    })
}

pub(super) fn read_resource(path: &Path) -> Result<String> {
    let bytes = crate::file_io::read_bounded(path, MAX_RESOURCE_BYTES).with_context(|| {
        format!(
            "cannot read resource {} (regular file, 1 MiB maximum)",
            path.display()
        )
    })?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("cannot read {} as UTF-8", path.display()))?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

pub(super) fn frontmatter<T: DeserializeOwned>(source: &str) -> Result<(T, &str)> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err(anyhow!("missing YAML frontmatter"));
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let metadata = serde_yaml_ng::from_str(&source[first.len()..offset])
                .context("invalid YAML frontmatter")?;
            return Ok((metadata, &source[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(anyhow!("unclosed YAML frontmatter"))
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_skills_and_templates_with_precedence_and_explicit_loading() {
        let root = std::env::temp_dir().join(format!("ion-resources-{}", uuid::Uuid::now_v7()));
        let project = root.join("project");
        let config = root.join("config");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::write(project.join(".gitignore"), "**/.agents/\n").unwrap();
        fs::create_dir_all(project.join("src/.agents/skills/review")).unwrap();
        fs::create_dir_all(project.join(".agents/skills/review")).unwrap();
        fs::create_dir_all(project.join(".ion/prompts")).unwrap();
        fs::write(project.join("src/.agents/skills/review/SKILL.md"), "---\nname: review\ndescription: Inspect Rust changes when reviewing code.\n---\n# Review\nRead the diff.\n").unwrap();
        fs::write(
            project.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Older root skill.\n---\n",
        )
        .unwrap();
        fs::write(
            project.join(".ion/prompts/check.md"),
            "---\ndescription: Check a named area\n---\nCheck $1; scope ${2:-all}; arguments $@.\n",
        )
        .unwrap();
        let resources = Resources::load_with_home(&project.join("src"), &config, None).unwrap();
        assert!(resources.instructions().contains("Inspect Rust changes"));
        assert!(!resources.instructions().contains("Older root skill"));
        assert_eq!(resources.diagnostics().len(), 1);
        assert!(
            resources
                .expand_command("/skill:review current diff")
                .unwrap()
                .unwrap()
                .contains("User request: current diff")
        );
        assert_eq!(
            resources
                .expand_command("/check 'Rust API'")
                .unwrap()
                .unwrap()
                .trim(),
            "Check Rust API; scope all; arguments Rust API."
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_resources_warn_and_template_arguments_do_not_reexpand() {
        let root = std::env::temp_dir().join(format!("ion-resources-{}", uuid::Uuid::now_v7()));
        let project = root.join("project");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::create_dir_all(project.join(".agents/skills/bad")).unwrap();
        fs::create_dir_all(project.join(".ion/prompts")).unwrap();
        fs::write(
            project.join(".agents/skills/bad/SKILL.md"),
            "---\nname: BAD\ndescription: no\n---\n",
        )
        .unwrap();
        fs::write(project.join(".ion/prompts/use.md"), "Argument: $1").unwrap();
        let resources = Resources::load_with_home(&project, &root.join("config"), None).unwrap();
        assert_eq!(resources.diagnostics().len(), 1);
        assert_eq!(
            resources.expand_command("/use '$@'").unwrap().unwrap(),
            "Argument: $@"
        );
        assert!(
            resources
                .expand_command("/use 'unfinished")
                .unwrap()
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_without_git_does_not_inherit_parent_project_resources() {
        let root = std::env::temp_dir().join(format!("ion-resources-{}", uuid::Uuid::now_v7()));
        let nested = root.join("nested");
        fs::create_dir_all(root.join(".ion/prompts")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(".ion/prompts/parent.md"), "Parent template").unwrap();
        let resources = Resources::load_with_home(&nested, &root.join("config"), None).unwrap();
        assert!(resources.expand_command("/parent").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
