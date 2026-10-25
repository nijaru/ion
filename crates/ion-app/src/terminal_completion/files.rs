//! File-token grammar, matching and literal editor replacements. No I/O.
use std::ops::Range;

use super::Suggestion;

pub(super) struct Prefix {
    pub range: Range<usize>,
    pub query: String,
}

pub(super) fn prefix(draft: &str, cursor: usize) -> Option<Prefix> {
    let before = &draft[..cursor];
    for (start, _) in before.char_indices().filter(|(_, ch)| *ch == '@') {
        if before[..start].chars().next_back().is_some_and(|ch| {
            !ch.is_whitespace() && !matches!(ch, '(' | '[' | '{' | '<' | '\'' | '"' | '`')
        }) {
            continue;
        }
        let raw = &before[start + 1..];
        let quoted = raw.starts_with('"');
        let query = if let Some(raw) = raw.strip_prefix('"') {
            // Decode the same JSON quoting emitted on acceptance. An unfinished
            // escape isn't yet a usable query; never complete a different path
            // by dropping or lossily rewriting its bytes.
            if raw.contains(['\n', '\r', '\t']) {
                continue;
            }
            let Ok(query) = serde_json::from_str::<String>(&format!("\"{raw}\"")) else {
                continue;
            };
            query
        } else {
            if raw.chars().any(char::is_whitespace) {
                continue;
            }
            raw.to_owned()
        };
        // Query only the prefix before the caret, but replace the complete
        // path token. Editing inside a quoted path must not duplicate its tail
        // or leave an unmatched closing quote in the surrounding prose.
        let after = &draft[cursor..];
        let end = if quoted {
            let mut escaped = false;
            after
                .char_indices()
                .find_map(|(at, ch)| {
                    if matches!(ch, '\n' | '\r' | '\t') {
                        return Some(cursor);
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
                .unwrap_or(cursor)
        } else {
            cursor
                + after
                    .find(|ch: char| {
                        ch.is_whitespace()
                            || matches!(
                                ch,
                                ')' | ']' | '}' | '>' | '"' | '\'' | '`' | ',' | ';' | '!' | '?'
                            )
                    })
                    .unwrap_or(after.len())
        };
        return Some(Prefix {
            range: start..end,
            query,
        });
    }
    None
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
    let quoted = draft[range.clone()].starts_with("@\"")
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
            "@{}",
            serde_json::to_string(path).expect("a string is JSON representable")
        )
    } else {
        format!("@{path}")
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
            let token = prefix(draft, draft.len()).unwrap();
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
            assert!(prefix(draft, draft.len()).is_none(), "{draft:?}");
        }
        let draft = "Read @\"a \\\"b";
        assert_eq!(prefix(draft, draft.len()).unwrap().query, "a \"b");
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
            ("@we", 3, "weird\"\\\n.rs", "@\"weird\\\"\\\\\\n.rs\" ", 18),
        ] {
            let token = prefix(draft, cursor).unwrap();
            let (range, text, next) = replacement(draft, &token.range, path);
            let mut result = draft.to_owned();
            result.replace_range(range, &text);
            assert_eq!(result, expected);
            assert_eq!(next, expected_cursor, "{result:?}");
        }
    }
}
