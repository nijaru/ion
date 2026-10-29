//! Captured skill paths, discovery and explicit expansion.
#[cfg(test)]
mod tests;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ignore::WalkBuilder;
use serde::Deserialize;

use super::{ResourceDiagnostic, frontmatter, project_directories, read_resource, valid_name};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub disable_model_invocation: bool,
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// Invocation-local skill selection, retained by the Host across reloads and
/// workspace changes. Paths are anchored once; discovery still follows the cwd.
#[derive(Debug, Clone)]
pub struct SkillSelection {
    include_defaults: bool,
    paths: Vec<PathBuf>,
}

impl Default for SkillSelection {
    fn default() -> Self {
        Self {
            include_defaults: true,
            paths: Vec::new(),
        }
    }
}

impl SkillSelection {
    /// Capture ordered file/directory paths relative to this working directory.
    /// Only a leading `~` component expands; no environment templates or commands.
    pub fn new(cwd: &Path, include_defaults: bool, paths: Vec<PathBuf>) -> Result<Self> {
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", cwd.display()))?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let paths = paths
            .into_iter()
            .map(|path| {
                ensure!(!path.as_os_str().is_empty(), "skill path must not be empty");
                let path = if let Ok(suffix) = path.strip_prefix("~") {
                    home.as_ref()
                        .context("HOME is not set for skill path")?
                        .join(suffix)
                } else {
                    path
                };
                Ok(if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            include_defaults,
            paths,
        })
    }
}

pub(super) fn discover(
    cwd: &Path,
    config_root: &Path,
    home: Option<&Path>,
    selection: &SkillSelection,
) -> (BTreeMap<String, Skill>, Vec<ResourceDiagnostic>) {
    let mut loader = Loader::default();
    if selection.include_defaults {
        let mut roots = vec![config_root.join("skills")];
        if let Some(home) = home {
            roots.push(home.join(".agents/skills"));
        }
        roots.extend(
            project_directories(cwd)
                .into_iter()
                .map(|dir| dir.join(".agents/skills")),
        );
        for root in roots {
            if root.is_dir() {
                loader.directory(&root, false);
            }
        }
    }
    for path in &selection.paths {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => loader.directory(path, true),
            Ok(metadata)
                if metadata.is_file() && path.extension().is_some_and(|ext| ext == "md") =>
            {
                loader.file(path)
            }
            Ok(_) => loader.diagnostic(
                path,
                "skill path must be a directory or a regular Markdown (.md) file",
            ),
            Err(error) => loader.diagnostic(path, format!("cannot inspect skill path: {error}")),
        }
    }
    (loader.skills, loader.diagnostics)
}

#[derive(Default)]
struct Loader {
    skills: BTreeMap<String, Skill>,
    diagnostics: Vec<ResourceDiagnostic>,
    directories: HashSet<(PathBuf, bool)>,
    files: HashSet<PathBuf>,
}

impl Loader {
    fn diagnostic(&mut self, path: &Path, message: impl Into<String>) {
        self.diagnostics.push(ResourceDiagnostic {
            path: path.to_owned(),
            message: message.into(),
        });
    }

    fn directory(&mut self, root: &Path, include_root_files: bool) {
        // A skill directory is one skill, not a container whose examples or
        // bundled references become independent skills.
        let main = root.join("SKILL.md");
        if main.is_file() {
            self.file(&main);
            return;
        }
        // Explicit roots also expose top-level Markdown files. An earlier
        // automatic scan of the same root must not suppress that wider view.
        if let Ok(identity) = root.canonicalize()
            && !self.directories.insert((identity, include_root_files))
        {
            return;
        }
        for entry in WalkBuilder::new(root)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(true)
            .filter_entry(|entry| {
                entry.depth() == 0
                    || entry
                        .path()
                        .file_name()
                        .is_some_and(|name| name == "SKILL.md")
                        && entry.file_type().is_some_and(|kind| kind.is_file())
                    || entry
                        .path()
                        .parent()
                        .is_none_or(|parent| !parent.join("SKILL.md").is_file())
            })
            .build()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostic(root, error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            let declared = path.file_name().is_some_and(|name| name == "SKILL.md");
            let root_file = include_root_files
                && entry.depth() == 1
                && path.extension().is_some_and(|ext| ext == "md");
            if !(declared || root_file) || !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            self.file(path);
        }
    }

    fn file(&mut self, path: &Path) {
        let identity = path.canonicalize().ok();
        if identity
            .as_ref()
            .is_some_and(|path| self.files.contains(path))
        {
            return;
        }
        let skill = match load(path) {
            Ok(skill) => skill,
            Err(error) => {
                self.diagnostic(path, format!("{error:#}"));
                return;
            }
        };
        // Invalid lexical aliases must not reserve a valid physical source.
        if let Some(identity) = identity {
            self.files.insert(identity);
        }
        if self.skills.contains_key(&skill.name) {
            self.diagnostic(
                path,
                format!("duplicate skill {}; first discovered copy wins", skill.name),
            );
        } else {
            self.skills.insert(skill.name.clone(), skill);
        }
    }
}

pub(super) fn expand(skill: &Skill, raw_args: &str, max_output_bytes: usize) -> Result<String> {
    if raw_args.len() > max_output_bytes {
        bail!(
            "request for /skill:{} exceeds the {} byte expansion limit; shorten the invocation",
            skill.name,
            max_output_bytes
        );
    }
    let source = read_resource(&skill.path)?;
    let (metadata, body): (SkillFrontmatter, _) = frontmatter(&source)?;
    if metadata.name != skill.name {
        return Err(anyhow!(
            "skill name changed since discovery; reload resources"
        ));
    }
    let request = raw_args.trim();
    let path = serde_json::to_string(&skill.path).context("cannot encode skill path")?;
    let header = format!("Skill {} from {path}:\n", skill.name);
    const USER_REQUEST: &str = "\n\nUser request: ";
    let suffix_bytes = if request.is_empty() {
        0
    } else {
        USER_REQUEST
            .len()
            .checked_add(request.len())
            .context("skill expansion size overflow")?
    };
    let bytes = header
        .len()
        .checked_add(body.len())
        .and_then(|bytes| bytes.checked_add(suffix_bytes))
        .context("skill expansion size overflow")?;
    if bytes > max_output_bytes {
        bail!(
            "skill /skill:{} expands to more than the {} byte limit; shorten the skill or request",
            skill.name,
            max_output_bytes
        );
    }
    let mut output = String::with_capacity(bytes);
    output.push_str(&header);
    output.push_str(body);
    if !request.is_empty() {
        output.push_str(USER_REQUEST);
        output.push_str(request);
    }
    Ok(output)
}

pub(super) fn load(path: &Path) -> Result<Skill> {
    if path.to_str().is_none() {
        return Err(anyhow!(
            "skill path is not representable as UTF-8 and cannot be advertised"
        ));
    }
    let source = read_resource(path)?;
    let (metadata, _): (SkillFrontmatter, _) = frontmatter(&source)?;
    let name = &metadata.name;
    if !valid_name(name)
        || path.file_name().is_some_and(|name| name == "SKILL.md")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|parent| parent.to_str())
                != Some(name)
    {
        return Err(anyhow!(
            "skill name must be valid and SKILL.md must match its directory"
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
