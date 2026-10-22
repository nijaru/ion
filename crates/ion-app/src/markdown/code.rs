//! Bundled grammars produce styles, never terminal escapes or replacement text.
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, TagEnd};
use ratatui::{style::Style, text::Span};
use two_face::re_exports::syntect::{
    easy::ScopeRegionIterator,
    parsing::{ParseState, Scope, ScopeStack, SyntaxSet},
};

use super::View;
use crate::presentation_style::{Role, style};

// Presentation is synchronous. Limit grammar work, not displayed/source content;
// an oversized block or line remains literal code, without partial tokenization.
const MAX_BLOCK_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 2048;
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static SCOPES: LazyLock<Vec<(Scope, Role)>> = LazyLock::new(|| {
    [
        ("comment", Role::SyntaxComment),
        ("string", Role::SyntaxString),
        ("constant.numeric", Role::SyntaxNumber),
        ("constant.language", Role::SyntaxNumber),
        ("keyword", Role::SyntaxKeyword),
        ("storage", Role::SyntaxKeyword),
        ("entity.name.function", Role::SyntaxFunction),
        ("support.function", Role::SyntaxFunction),
        ("entity.name.type", Role::SyntaxType),
        ("entity.name.class", Role::SyntaxType),
        ("support.type", Role::SyntaxType),
        ("support.class", Role::SyntaxType),
    ]
    .into_iter()
    .map(|(scope, role)| (Scope::new(scope).expect("static syntax scope"), role))
    .collect()
});

impl View<'_> {
    pub(super) fn code_block<'a>(
        &mut self,
        events: &mut impl Iterator<Item = Event<'a>>,
        kind: CodeBlockKind<'a>,
    ) {
        self.flush();
        let info = match kind {
            CodeBlockKind::Fenced(info) => info.into_string(),
            CodeBlockKind::Indented => String::new(),
        };
        if !info.is_empty() {
            self.spans.push(Span::styled(
                format!("code · {info}"),
                style(Role::Secondary),
            ));
            self.flush();
        }
        let mut source = String::new();
        for event in events {
            match event {
                Event::End(TagEnd::CodeBlock) => break,
                Event::Text(text) => source.push_str(&text),
                _ => {}
            }
        }
        if source.is_empty() {
            self.separate();
            return;
        }
        // The parser's final newline terminates the block, not an extra row.
        let source = source.strip_suffix('\n').unwrap_or(&source);
        self.push_prefix("  ".into(), "  ".into());
        self.spans = highlight(source, &info, self.style()).unwrap_or_else(|| {
            vec![Span::styled(
                source.to_owned(),
                self.style().patch(style(Role::Code)),
            )]
        });
        self.flush();
        self.prefixes.pop();
        self.separate();
    }
}

fn highlight(source: &str, info: &str, base: Style) -> Option<Vec<Span<'static>>> {
    let token = info.split_whitespace().next()?.to_ascii_lowercase();
    if source.is_empty()
        || matches!(token.as_str(), "text" | "txt" | "plain" | "plaintext")
        || source.len() > MAX_BLOCK_BYTES
        || source.lines().any(|line| line.len() > MAX_LINE_BYTES)
    {
        return None;
    }
    let token = match token.as_str() {
        "shell" | "shellscript" => "sh",
        "rs" => "rust",
        "py" => "python",
        _ => &token,
    };
    let syntax = SYNTAXES.find_syntax_by_token(token)?;
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for line in source.split_inclusive('\n') {
        let ops = parser.parse_line(line, &SYNTAXES).ok()?;
        for (text, op) in ScopeRegionIterator::new(&ops, line) {
            stack.apply(op).ok()?;
            if text.is_empty() {
                continue;
            }
            let role = stack.scopes.iter().rev().find_map(|&scope| {
                SCOPES
                    .iter()
                    .find_map(|&(prefix, role)| prefix.is_prefix_of(scope).then_some(role))
            });
            let style = role.map_or(base, |role| base.patch(style(role)));
            if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
                last.content.to_mut().push_str(text);
            } else {
                spans.push(Span::styled(text.to_owned(), style));
            }
        }
    }
    Some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_limits_preserve_literal_fallback_without_loading_source_paths() {
        for source in [
            "x".repeat(MAX_LINE_BYTES + 1),
            "// line\n".repeat(MAX_BLOCK_BYTES / 8 + 1),
        ] {
            assert!(highlight(&source, "rust", Style::default()).is_none());
        }
        assert!(highlight("fn main() {}", "/tmp/example.rs", Style::default()).is_none());
        let source = "fn main() {\n  /* unfinished";
        let spans = highlight(source, "rust", Style::default()).unwrap();
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            source
        );
        assert_eq!(spans.last().unwrap().style, style(Role::SyntaxComment));
        let fresh = highlight("let n = 42;", "rust", Style::default()).unwrap();
        assert_eq!(fresh[0].style, style(Role::SyntaxKeyword));
    }
}
