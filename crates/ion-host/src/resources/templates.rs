//! Prompt template loading and one-pass argument expansion.
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use super::{
    ResourceDiagnostic, ResourcePaths, frontmatter, project_directories, read_resource, valid_name,
};
use crate::command_args::parse_args;

#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub path: PathBuf,
    body: String,
}

#[derive(Default, Deserialize)]
struct TemplateFrontmatter {
    description: Option<String>,
    #[serde(rename = "argument-hint")]
    argument_hint: Option<String>,
}

pub(super) fn discover(
    cwd: &Path,
    config_root: &Path,
    selection: &ResourcePaths,
) -> (BTreeMap<String, PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut loader = Loader::default();
    if selection.include_defaults {
        loader.path(&config_root.join("prompts"), false);
        for directory in project_directories(cwd) {
            loader.path(&directory.join(".ion/prompts"), false);
        }
    }
    for path in &selection.paths {
        loader.path(path, true);
    }
    (loader.templates, loader.diagnostics)
}

#[derive(Default)]
struct Loader {
    templates: BTreeMap<String, PromptTemplate>,
    diagnostics: Vec<ResourceDiagnostic>,
    directories: HashSet<PathBuf>,
    files: HashSet<(PathBuf, String)>,
}

impl Loader {
    fn diagnostic(&mut self, path: &Path, message: impl Into<String>) {
        self.diagnostics.push(ResourceDiagnostic {
            path: path.to_owned(),
            message: message.into(),
        });
    }

    fn path(&mut self, path: &Path, explicit: bool) {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => self.directory(path),
            Ok(metadata)
                if explicit
                    && metadata.is_file()
                    && path.extension().is_some_and(|ext| ext == "md") =>
            {
                self.file(path)
            }
            Ok(_) => self.diagnostic(
                path,
                if explicit {
                    "prompt template path must be a directory or a regular Markdown (.md) file"
                } else {
                    "prompt template discovery root must be a directory"
                },
            ),
            Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => self.diagnostic(
                path,
                format!("cannot inspect prompt template path: {error}"),
            ),
        }
    }

    fn directory(&mut self, root: &Path) {
        if let Ok(identity) = root.canonicalize()
            && !self.directories.insert(identity)
        {
            return;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                self.diagnostic(root, error.to_string());
                return;
            }
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    self.diagnostic(root, error.to_string());
                    continue;
                }
            };
            if path.extension().is_some_and(|ext| ext == "md") && path.is_file() {
                self.file(&path);
            }
        }
    }

    fn file(&mut self, path: &Path) {
        let template = match load_template(path) {
            Ok(template) => template,
            Err(error) => {
                self.diagnostic(path, format!("{error:#}"));
                return;
            }
        };
        // Filename aliases are distinct commands. Deduplicate physical aliases
        // only when they publish the same command name.
        if let Ok(identity) = path.canonicalize()
            && !self.files.insert((identity, template.name.clone()))
        {
            return;
        }
        if self.templates.contains_key(&template.name) {
            self.diagnostic(
                path,
                format!(
                    "duplicate template {}; first discovered copy wins",
                    template.name
                ),
            );
        } else {
            self.templates.insert(template.name.clone(), template);
        }
    }
}

pub(super) fn load_template(path: &Path) -> Result<PromptTemplate> {
    let source = read_resource(path)?;
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid template filename"))?;
    if !valid_name(name) {
        return Err(anyhow!("template filename must be a valid command name"));
    }
    let (metadata, body): (TemplateFrontmatter, _) =
        if source.starts_with("---\n") || source.starts_with("---\r\n") {
            frontmatter(&source)?
        } else {
            (TemplateFrontmatter::default(), source.as_str())
        };
    let description = metadata
        .description
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| {
            body.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(60)
                .collect()
        });
    Ok(PromptTemplate {
        name: name.into(),
        description,
        argument_hint: metadata.argument_hint,
        path: path.to_owned(),
        body: body.into(),
    })
}

impl PromptTemplate {
    pub(super) fn expand(&self, raw_args: &str, max_output_bytes: usize) -> Result<String> {
        // Bound parsing/copying separately from output. Template source is
        // already bounded at load and may shrink under substitution.
        if raw_args.len() > max_output_bytes {
            bail!(
                "arguments for /{} exceed the {} byte expansion limit; shorten the invocation",
                self.name,
                max_output_bytes
            );
        }
        substitute(&self.body, &parse_args(raw_args)?, max_output_bytes)
    }
}

// Borrow both defaults and parsed arguments; never build a substitution String
// merely to discover that it cannot fit the remaining output budget.
enum Replacement<'a> {
    Text(&'a str),
    Arguments(&'a [String]),
}

impl Replacement<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            // Two empty arguments join to one space, not an empty value.
            Self::Arguments(args) => args.is_empty() || (args.len() == 1 && args[0].is_empty()),
        }
    }
}

fn substitute(template: &str, args: &[String], max_output_bytes: usize) -> Result<String> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(index) = rest.find('$') {
        append_bounded(&mut output, &rest[..index], max_output_bytes)?;
        rest = &rest[index..];
        if let Some((value, consumed)) = placeholder(rest, args) {
            match value {
                Replacement::Text(text) => append_bounded(&mut output, text, max_output_bytes)?,
                Replacement::Arguments(args) => {
                    for (index, arg) in args.iter().enumerate() {
                        if index > 0 {
                            append_bounded(&mut output, " ", max_output_bytes)?;
                        }
                        append_bounded(&mut output, arg, max_output_bytes)?;
                    }
                }
            }
            rest = &rest[consumed..];
        } else {
            append_bounded(&mut output, "$", max_output_bytes)?;
            rest = &rest[1..];
        }
    }
    append_bounded(&mut output, rest, max_output_bytes)?;
    Ok(output)
}

fn append_bounded(output: &mut String, value: &str, max_output_bytes: usize) -> Result<()> {
    if value.len() > max_output_bytes.saturating_sub(output.len()) {
        bail!(
            "expanded prompt exceeds the {max_output_bytes} byte limit; shorten the template or arguments"
        );
    }
    output.push_str(value);
    Ok(())
}

fn placeholder<'a>(input: &'a str, args: &'a [String]) -> Option<(Replacement<'a>, usize)> {
    use Replacement::{Arguments, Text};
    if let Some(rest) = input.strip_prefix("${") {
        let end = rest.find('}')?;
        let expression = &rest[..end];
        let value = if let Some((target, default)) = expression.split_once(":-") {
            let value = if target == "@" || target == "ARGUMENTS" {
                Arguments(args)
            } else {
                Text(argument(args, target)?)
            };
            if value.is_empty() {
                Text(default)
            } else {
                value
            }
        } else {
            let slice = expression.strip_prefix("@:")?;
            let (start, length) = slice
                .split_once(':')
                .map_or((slice, None), |(start, length)| (start, Some(length)));
            let start = start.parse::<usize>().ok()?.max(1) - 1;
            let length = length.map(str::parse::<usize>).transpose().ok()?;
            let tail = &args[start.min(args.len())..];
            Arguments(&tail[..length.unwrap_or(usize::MAX).min(tail.len())])
        };
        return Some((value, end + 3));
    }
    if input.starts_with("$ARGUMENTS") {
        return Some((Arguments(args), 10));
    }
    if input.starts_with("$@") {
        return Some((Arguments(args), 2));
    }
    let digits = input[1..].bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| {
        (
            Text(argument(args, &input[1..1 + digits]).unwrap_or("")),
            1 + digits,
        )
    })
}

fn argument<'a>(args: &'a [String], position: &str) -> Option<&'a str> {
    let index = position.parse::<usize>().ok()?.checked_sub(1)?;
    Some(args.get(index).map_or("", String::as_str))
}

#[cfg(test)]
mod tests;
