//! Prompt template loading and one-pass argument expansion.
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
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
mod tests {
    use super::*;

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
}
