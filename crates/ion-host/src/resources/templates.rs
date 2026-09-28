//! Prompt template loading and one-pass argument expansion.
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::{frontmatter, read_resource, valid_name};

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
    pub(super) fn expand(&self, raw_args: &str) -> Result<String> {
        let args = parse_args(raw_args)?;
        Ok(substitute(&self.body, &args))
    }
}

fn parse_args(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (Some(open), close) if open == close => quote = None,
            (None, ch) if ch.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (Some('\''), ch) => {
                current.push(ch);
                started = true;
            }
            (_, '\\') => {
                current.push(
                    chars
                        .next()
                        .ok_or_else(|| anyhow!("trailing escape in arguments"))?,
                );
                started = true;
            }
            (_, ch) => {
                current.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(anyhow!("unclosed quote in arguments"));
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

fn substitute(template: &str, args: &[String]) -> String {
    let mut output = String::new();
    let mut rest = template;
    while let Some(index) = rest.find('$') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some((value, consumed)) = placeholder(rest, args) {
            output.push_str(&value);
            rest = &rest[consumed..];
        } else {
            output.push('$');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

fn placeholder(input: &str, args: &[String]) -> Option<(String, usize)> {
    let all = || args.join(" ");
    if let Some(rest) = input.strip_prefix("${") {
        let end = rest.find('}')?;
        let expression = &rest[..end];
        let value = if let Some((target, default)) = expression.split_once(":-") {
            let value = if target == "@" || target == "ARGUMENTS" {
                all()
            } else {
                argument(args, target)?
            };
            if value.is_empty() {
                default.to_owned()
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
            args.iter()
                .skip(start)
                .take(length.unwrap_or(usize::MAX))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Some((value, end + 3));
    }
    if input.starts_with("$ARGUMENTS") {
        return Some((all(), 10));
    }
    if input.starts_with("$@") {
        return Some((all(), 2));
    }
    let digits = input[1..].bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| {
        (
            argument(args, &input[1..1 + digits]).unwrap_or_default(),
            1 + digits,
        )
    })
}

fn argument(args: &[String], position: &str) -> Option<String> {
    let index = position.parse::<usize>().ok()?.checked_sub(1)?;
    Some(args.get(index).cloned().unwrap_or_default())
}
