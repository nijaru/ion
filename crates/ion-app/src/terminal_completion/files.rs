//! File-token grammar, matching and literal editor replacements. No I/O.
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::Suggestion;

/// Filesystem metadata is keyed by lookup scope, never by editor ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileScope {
    Project(PathBuf),
    Directory(PathBuf),
    HomeUnavailable,
}

pub(super) struct Prefix {
    pub range: Range<usize>,
    pub query: String,
    attachment: bool,
}

impl Prefix {
    fn directory_parts(&self) -> Option<(String, &str)> {
        if self.attachment && !self.query.starts_with(['/', '~', '.']) {
            return None;
        }
        if matches!(self.query.as_str(), "." | ".." | "~") {
            return Some((format!("{}/", self.query), ""));
        }
        let split = self.query.rfind('/').map_or(0, |at| at + 1);
        Some((self.query[..split].to_owned(), &self.query[split..]))
    }

    pub(super) fn scope(&self, cwd: &Path, home: Option<&Path>) -> FileScope {
        let Some((directory, _)) = self.directory_parts() else {
            return FileScope::Project(cwd.to_owned());
        };
        let path = if let Some(relative) = directory.strip_prefix("~/") {
            let Some(home) = home.filter(|home| home.is_absolute()) else {
                return FileScope::HomeUnavailable;
            };
            home.join(relative)
        } else {
            cwd.join(directory)
        };
        FileScope::Directory(path)
    }

    pub(super) fn suggestions(&self, paths: &[String]) -> Vec<Suggestion> {
        let Some((directory, query)) = self.directory_parts() else {
            return suggestions(paths, &self.query);
        };
        let mut items = if self.attachment {
            suggestions(paths, query)
        } else {
            let query = query.to_lowercase();
            paths
                .iter()
                .filter(|path| path.to_lowercase().starts_with(&query))
                .map(|path| Suggestion {
                    name: path.clone(),
                    description: String::new(),
                })
                .collect()
        };
        if !self.attachment {
            items.sort_by(|a, b| {
                b.name
                    .ends_with('/')
                    .cmp(&a.name.ends_with('/'))
                    .then(a.name.cmp(&b.name))
            });
        }
        items.truncate(20);
        for item in &mut items {
            item.name.insert_str(0, &directory);
        }
        items
    }
}

pub(super) fn path_prefix(draft: &str, cursor: usize, force: bool) -> Option<Prefix> {
    let before = &draft[..cursor];
    let mut start = 0;
    let mut quoted = false;
    let mut closed = false;
    let mut escaped = false;
    for (at, ch) in before.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
                closed = true;
            }
            continue;
        }
        if ch.is_whitespace() || matches!(ch, '=' | '\'') {
            start = at + ch.len_utf8();
            closed = false;
        } else if ch == '"' && (at == start || &before[start..at] == "@") {
            quoted = true;
        } else if at == start {
            let closer = match ch {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '<' => Some('>'),
                '`' => Some('`'),
                _ => None,
            };
            if closer.is_some_and(|closer| !before[at + ch.len_utf8()..].contains(closer)) {
                start += ch.len_utf8();
            }
        }
    }
    if closed {
        return None;
    }
    let raw = &before[start..];
    let attachment = raw.starts_with('@');
    let raw = if attachment { &raw[1..] } else { raw };
    let quoted = raw.starts_with('"');
    let query = if let Some(raw) = raw.strip_prefix('"') {
        if raw.contains(['\n', '\r', '\t']) {
            return None;
        }
        serde_json::from_str::<String>(&format!("\"{raw}\"")).ok()?
    } else {
        if raw.chars().any(char::is_whitespace) {
            return None;
        }
        raw.to_owned()
    };
    if !attachment {
        // Preserve first-token command completion until a second slash makes
        // the absolute directory intent unambiguous.
        if start == draft.len() - draft.trim_start().len()
            && query.starts_with('/')
            && !query[1..].contains('/')
        {
            return None;
        }
        if query.contains("://") {
            return None;
        }
        if !force && !quoted && !query.contains('/') && !query.starts_with('.') && query != "~" {
            return None;
        }
    }
    Some(Prefix {
        range: start..token_end(draft, cursor, quoted),
        query,
        attachment,
    })
}

fn token_end(draft: &str, cursor: usize, quoted: bool) -> usize {
    let after = &draft[cursor..];
    if quoted {
        let mut escaped = false;
        after
            .char_indices()
            .find_map(|(at, ch)| {
                if matches!(ch, '\n' | '\r' | '\t') {
                    return Some(token_end(draft, cursor, false));
                }
                if escaped {
                    escaped = false;
                    return None;
                }
                if ch == '\\' {
                    escaped = true;
                    return None;
                }
                (ch == '"').then_some(cursor + at + ch.len_utf8())
            })
            // Without a closing quote, consume an adjacent token tail but retain
            // whitespace-delimited prose rather than guessing that it is a filename.
            .unwrap_or_else(|| token_end(draft, cursor, false))
    } else {
        cursor
            + after
                .find(|ch: char| {
                    ch.is_whitespace()
                        || matches!(
                            ch,
                            ')' | ']' | '}' | '>' | '"' | '\'' | '`' | ',' | ';' | '!' | '?' | '='
                        )
                })
                .unwrap_or(after.len())
    }
}

pub(super) fn suggestions(paths: &[String], query: &str) -> Vec<Suggestion> {
    let query = query.to_lowercase();
    let mut matches = paths
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            if path.ends_with('/') && lower == query {
                return None;
            }
            // Contiguous matches outrank subsequence matches, then favor shallow,
            // short paths. This keeps directory navigation and fuzzy basename input
            // useful without hiding the actual project-relative replacement.
            let score = if let Some(at) = lower.find(&query) {
                (0, at)
            } else {
                let mut chars = lower.chars();
                if !query
                    .chars()
                    .all(|wanted| chars.by_ref().any(|ch| ch == wanted))
                {
                    return None;
                }
                (1, lower.len())
            };
            Some((score, path.matches('/').count(), path.len(), path))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .into_iter()
        .take(20)
        .map(|(_, _, _, path)| Suggestion {
            name: path.clone(),
            description: String::new(),
        })
        .collect()
}

pub(super) fn replacement(
    draft: &str,
    range: &Range<usize>,
    path: &str,
) -> (Range<usize>, String, usize) {
    let attachment = draft[range.clone()].starts_with('@');
    let marker = if attachment { "@" } else { "" };
    let quoted = draft[range.clone()]
        .trim_start_matches('@')
        .starts_with('"')
        || path.chars().any(|ch| {
            ch.is_whitespace()
                || ch.is_control()
                || (ch.is_ascii()
                    && !ch.is_ascii_alphanumeric()
                    && !matches!(ch, '_' | '-' | '.' | '/' | '~'))
        })
        || path.ends_with('.');
    let range = range.clone();
    let mut text = if quoted {
        format!(
            "{marker}{}",
            serde_json::to_string(path).expect("a string is JSON representable")
        )
    } else {
        format!("{marker}{path}")
    };
    let cursor = if path.ends_with('/') {
        range.start + text.len() - usize::from(quoted)
    } else {
        let following = draft[range.end..]
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace());
        if following.is_none() {
            text.push(' ');
        }
        range.start + text.len() + following.map_or(0, char::len_utf8)
    };
    (range, text, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_prefix_respects_boundaries_quotes_and_unicode() {
        for (draft, query) in [
            ("@", ""),
            ("Read\u{2003}@src/ma", "src/ma"),
            ("(`@ma", "ma"),
            ("@\"space dir/ma", "space dir/ma"),
        ] {
            let token = path_prefix(draft, draft.len(), false).unwrap();
            assert_eq!(token.query, query);
            assert_eq!(&draft[token.range][..1], "@");
        }
        for draft in [
            "nick@example.com",
            "user@host:repo",
            "@src/main.rs next",
            "@\"closed\"",
            "@\"unfinished\\",
        ] {
            assert!(
                path_prefix(draft, draft.len(), false).is_none(),
                "{draft:?}"
            );
        }
        let draft = "Read @\"a \\\"b";
        assert_eq!(
            path_prefix(draft, draft.len(), false).unwrap().query,
            "a \"b"
        );
    }

    #[test]
    fn path_scopes_preserve_spelling_and_require_explicit_bare_completion() {
        let cwd = Path::new("/workspace");
        let home = Path::new("/owned-home");
        for (draft, force, scope, candidate) in [
            ("Read ma", true, FileScope::Directory(cwd.into()), "main.rs"),
            (
                "./ma",
                false,
                FileScope::Directory(cwd.join("./")),
                "./main.rs",
            ),
            (
                "~/ma",
                false,
                FileScope::Directory(home.into()),
                "~/main.rs",
            ),
            (
                "../ma",
                false,
                FileScope::Directory(cwd.join("../")),
                "../main.rs",
            ),
            (
                "/tmp/ma",
                false,
                FileScope::Directory("/tmp/".into()),
                "/tmp/main.rs",
            ),
            (
                "Read (./ma",
                false,
                FileScope::Directory(cwd.join("./")),
                "./main.rs",
            ),
            (
                "path=./ma",
                false,
                FileScope::Directory(cwd.join("./")),
                "./main.rs",
            ),
            (
                "Read \"./space @dir/ma",
                false,
                FileScope::Directory(cwd.join("./space @dir/")),
                "./space @dir/main.rs",
            ),
            ("@ma", false, FileScope::Project(cwd.into()), "main.rs"),
            (
                "@~/ma",
                false,
                FileScope::Directory(home.into()),
                "~/main.rs",
            ),
        ] {
            let token = path_prefix(draft, draft.len(), force).unwrap();
            assert_eq!(token.scope(cwd, Some(home)), scope, "{draft}");
            assert_eq!(token.suggestions(&["main.rs".into()])[0].name, candidate);
        }
        for draft in [
            "ordinary prose",
            "nick@example.com",
            "https://example.com/file",
            "/model",
            "/help",
        ] {
            assert!(path_prefix(draft, draft.len(), false).is_none(), "{draft}");
        }
        let token = path_prefix("~/", 2, false).unwrap();
        assert_eq!(token.scope(cwd, None), FileScope::HomeUnavailable);
    }

    #[test]
    fn replacements_quote_paths_and_preserve_suffix_and_directory_cursor() {
        for (draft, cursor, path, expected, expected_cursor) in [
            (
                "Read @ma next",
                8,
                "src/main.rs",
                "Read @src/main.rs next",
                18,
            ),
            (
                "@\"space d\" next",
                9,
                "space dir/main.rs",
                "@\"space dir/main.rs\" next",
                21,
            ),
            (
                "@\"space dir/main.rs\" next",
                "@\"space dir/ma".len(),
                "space dir/main.rs",
                "@\"space dir/main.rs\" next",
                21,
            ),
            (
                "Read @main.rs next",
                "Read @ma".len(),
                "src/main.rs",
                "Read @src/main.rs next",
                18,
            ),
            ("@sp", 3, "space dir/", "@\"space dir/\"", 12),
            ("Read ma next", 7, "main.rs", "Read main.rs next", 13),
            (
                "Read ./ma=keep",
                "Read ./ma".len(),
                "./main.rs",
                "Read ./main.rs =keep",
                "Read ./main.rs ".len(),
            ),
            (
                "Read \"./main.rs\nnext",
                "Read \"./ma".len(),
                "./main.rs",
                "Read \"./main.rs\"\nnext",
                "Read \"./main.rs\"\n".len(),
            ),
            (
                "Read \"./src/main.rs",
                "Read \"./src/ma".len(),
                "./src/main.rs",
                "Read \"./src/main.rs\" ",
                "Read \"./src/main.rs\" ".len(),
            ),
            (
                "\"space dir/main.rs\" next",
                13,
                "space dir/main.rs",
                "\"space dir/main.rs\" next",
                20,
            ),
            ("./sp", 4, "./space dir/", "\"./space dir/\"", 13),
            ("@we", 3, "weird\"\\\n.rs", "@\"weird\\\"\\\\\\n.rs\" ", 18),
        ] {
            let token = path_prefix(draft, cursor, true).unwrap();
            let (range, text, next) = replacement(draft, &token.range, path);
            let mut result = draft.to_owned();
            result.replace_range(range, &text);
            assert_eq!(result, expected);
            assert_eq!(next, expected_cursor, "{result:?}");
        }
    }
}
