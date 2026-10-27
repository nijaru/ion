//! Assistant-only Markdown presentation. No HTML renderer, fetches or terminal links.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

mod code;
mod table;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    display_text::push_styled,
    presentation_style::{Role, Theme},
};

struct Prefix {
    first: String,
    continuation: String,
    used: bool,
}

struct View<'a> {
    rows: &'a mut Vec<Line<'static>>,
    theme: &'a Theme,
    width: usize,
    spans: Vec<Span<'static>>,
    separator: Option<Line<'static>>,
    styles: Vec<Style>,
    prefixes: Vec<Prefix>,
    lists: Vec<Option<u64>>,
    links: Vec<(String, usize)>,
}

pub(super) fn render(rows: &mut Vec<Line<'static>>, source: &str, width: usize, theme: &Theme) {
    let start = rows.len();
    let mut view = View::new(rows, width, theme.style(Role::Text), theme);
    let mut events = Parser::new_ext(source, Options::ENABLE_TABLES);
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Table(alignments)) => view.table(&mut events, alignments),
            Event::Start(Tag::CodeBlock(kind)) => view.code_block(&mut events, kind),
            event => view.event(event),
        }
    }
    view.flush();
    while view.rows.len() > start && view.rows.last().is_some_and(|row| row.spans.is_empty()) {
        view.rows.pop();
    }
}

impl<'a> View<'a> {
    fn new(rows: &'a mut Vec<Line<'static>>, width: usize, base: Style, theme: &'a Theme) -> Self {
        Self {
            rows,
            theme,
            width,
            spans: Vec::new(),
            separator: None,
            styles: vec![base],
            prefixes: Vec::new(),
            lists: Vec::new(),
            links: Vec::new(),
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.text(text.into_string());
            }
            Event::Code(text) => self.spans.push(Span::styled(
                text.into_string(),
                self.style().patch(self.theme.style(Role::Code)),
            )),
            Event::SoftBreak => self.text(" ".into()),
            Event::HardBreak => self.text("\n".into()),
            Event::Rule => {
                self.flush();
                self.text("─".repeat(self.width.clamp(1, 12)));
                self.flush();
                self.separate();
            }
            // Other extensions remain disabled.
            _ => {}
        }
    }

    fn style(&self) -> Style {
        // The parser's balanced start/end events retain the base style.
        *self.styles.last().expect("Markdown base style")
    }

    fn text(&mut self, text: String) {
        self.spans.push(Span::styled(text, self.style()));
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        if let Some(separator) = self.separator.take() {
            self.rows.push(separator);
        }
        let first = self
            .prefixes
            .iter()
            .map(|prefix| {
                if prefix.used {
                    prefix.continuation.as_str()
                } else {
                    prefix.first.as_str()
                }
            })
            .collect::<String>();
        let continuation = self
            .prefixes
            .iter()
            .map(|prefix| prefix.continuation.as_str())
            .collect::<String>();
        push_styled(
            self.rows,
            &first,
            &continuation,
            Line::from(std::mem::take(&mut self.spans)),
            self.width,
        );
        for prefix in &mut self.prefixes {
            prefix.used = true;
        }
    }

    fn separate(&mut self) {
        if self.prefixes.is_empty() {
            self.separator = Some(Line::default());
        }
    }

    fn paragraph_separator(&mut self) {
        let prefix = self
            .prefixes
            .iter()
            .map(|prefix| prefix.continuation.as_str())
            .collect::<String>();
        self.separator = Some(if prefix.is_empty() {
            Line::default()
        } else {
            Line::raw(prefix)
        });
    }

    fn push_prefix(&mut self, first: String, continuation: String) {
        self.prefixes.push(Prefix {
            first,
            continuation,
            used: false,
        });
    }

    fn start(&mut self, tag: Tag<'_>) {
        let image = matches!(tag, Tag::Image { .. });
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.flush(),
            Tag::Heading { .. } => {
                self.flush();
                self.styles.push(self.style().add_modifier(Modifier::BOLD));
            }
            Tag::Emphasis => self
                .styles
                .push(self.style().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(self.style().add_modifier(Modifier::BOLD)),
            Tag::BlockQuote(_) => {
                self.flush();
                self.push_prefix("│ ".into(), "│ ".into());
            }
            Tag::List(number) => {
                self.flush();
                self.lists.push(number);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number = number.saturating_add(1);
                        marker
                    }
                    _ => "• ".into(),
                };
                let continuation = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
                self.push_prefix(marker, continuation);
            }
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push((dest_url.into_string(), self.spans.len()));
                self.styles
                    .push(self.style().patch(self.theme.style(Role::Link)));
                if image {
                    self.text("[image: ".into());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => {
                self.flush();
                self.paragraph_separator();
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.styles.pop();
                self.separate();
            }
            TagEnd::Emphasis | TagEnd::Strong => {
                self.styles.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.prefixes.pop();
                self.separate();
            }
            TagEnd::Item => {
                if self.prefixes.last().is_some_and(|prefix| !prefix.used) {
                    self.text(String::new());
                }
                self.flush();
                self.prefixes.pop();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.separate();
            }
            TagEnd::Link | TagEnd::Image => {
                if tag == TagEnd::Image {
                    self.text("]".into());
                }
                self.styles.pop();
                if let Some((target, start)) = self.links.pop() {
                    let label = self.spans[start..]
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    let destination = visible_target(&target);
                    if label == target && destination != target {
                        self.spans.truncate(start);
                        self.text(format!("[{destination}]"));
                    } else if label != target {
                        self.text(format!(" ({destination})"));
                    }
                }
            }
            _ => {}
        }
    }
}

fn visible_target(target: &str) -> &str {
    // Targets are displayed, never interpreted. Opaque data and executable schemes
    // are not useful terminal destinations; retain their original source in inspection.
    if let Some((scheme, _)) = target.split_once(':')
        && !["http", "https", "mailto", "file"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return "unsupported target";
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(source: &str, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        render(&mut rows, source, width, &Theme::default());
        rows
    }

    #[test]
    fn commonmark_blocks_retain_inline_styles_and_visible_destinations() {
        let rows = view(
            "# Heading\n\n**bold *both*** and `code` [label](https://example.org)\n\n> 3. first\n>    - nested\n> 4. last",
            80,
        );
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Heading\n\nbold both and code label (https://example.org)"),
            "{text}"
        );
        assert!(
            text.contains("│ 3. first\n│    • nested\n│ 4. last"),
            "{text}"
        );
        assert!(rows.iter().flat_map(|row| &row.spans).any(|span| {
            span.content == "both"
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::ITALIC)
        }));
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content == "code"
                    && span.style.fg == Theme::default().style(Role::Code).fg)
        );
    }

    #[test]
    fn tables_align_columns_and_retain_cell_styles() {
        let rows = view(
            "| Name | Count | Flag |\n| :--- | ---: | :---: |\n| *alpha* | 7 | ok |",
            80,
        );
        assert_eq!(
            rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Name  │ Count │ Flag",
                "──────┼───────┼─────",
                "alpha │     7 │  ok ",
            ]
        );
        assert!(
            rows[0]
                .spans
                .iter()
                .filter(|span| span.content.chars().any(char::is_alphabetic))
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            rows[2].spans.iter().any(|span| span.content == "alpha"
                && span.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    #[test]
    fn invisible_cell_graphemes_do_not_push_values_out_of_their_columns() {
        let rows = view("| A | B |\n| --- | --- |\n| \u{200b} | x |", 5);
        assert_eq!(
            rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["A │ B", "──┼──", "� │ x"]
        );
    }

    #[test]
    fn tables_wrap_or_stack_without_losing_values_or_terminal_safety() {
        let source = "| Item | Details | Note |\n| --- | --- | --- |\n| **e**\u{0301}界 | `foo_bar_baz` | [x](javascript:SECRET) |\n| tail | a\\|b | end |\n| empty |";
        for width in [1, 2, 7, 13, 24, 48, 80] {
            let rows = view(source, width);
            let text = rows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                rows.iter().all(|line| line.width() <= width),
                "width={width}: {text}"
            );
            assert!(!text.contains("SECRET"));
            assert!(!text.chars().any(|ch| ch.is_control() && ch != '\n'));
            assert!(text.contains("e\u{0301}"));
            assert!(
                rows.iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.style.fg == Theme::default().style(Role::Code).fg)
            );
            if width == 7 {
                let joined = rows.iter().map(ToString::to_string).collect::<String>();
                assert!(joined.contains("Details: foo_bar_baz"), "{text}");
                assert!(joined.contains("Details: a|b"), "{text}");
                assert!(joined.contains("Item: empty"), "{text}");
            }
        }
        let rows = view(
            "> - | Key | Value |\n>   | --- | --- |\n>   | abc | long_value |",
            18,
        );
        assert!(rows.iter().all(|line| line.width() <= 18));
        assert!(rows[0].to_string().starts_with("│ • "));
    }

    #[test]
    fn code_html_and_links_are_control_safe_literal_presentation() {
        let rows = view(
            "```sh\n  **literal**\n\t界\u{1b}[2J\n```\n\n<script>alert(1)</script>\n\n[x](javascript:alert) ![alt](data:image/png;base64,SECRET) <data:SECRET>",
            80,
        );
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("    **literal**\n      界�[2J"), "{text}");
        assert!(text.contains("<script>alert(1)</script>"));
        assert!(
            text.contains("x (unsupported target) [image: alt] (unsupported target)"),
            "{text}"
        );
        assert!(!text.contains("SECRET"));
        assert!(!text.chars().any(|ch| ch.is_control() && ch != '\n'));
    }

    #[test]
    fn fenced_languages_highlight_tokens_and_keep_multiline_state_local() {
        let rows = view(
            "```RuSt title\nfn main() {\n/* comment\nstill comment */\nlet name = \"界e\u{0301}\";\n}\n```\n\nafter\n\n```rust\nlet n = 42;\n```",
            80,
        );
        let token_style = |text: &str| {
            rows.iter()
                .flat_map(|row| &row.spans)
                .find(|span| span.content.contains(text))
                .unwrap_or_else(|| panic!("missing {text}: {rows:?}"))
                .style
        };
        assert_ne!(token_style("fn"), token_style("main"));
        assert_eq!(token_style("comment"), token_style("still comment"));
        assert_ne!(token_style("still comment"), token_style("let"));
        assert_eq!(token_style("after"), Theme::default().style(Role::Text));
        for label in ["python", "sh", "typescript", "tsx", "json", "toml"] {
            let rows = view(&format!("```{label}\nvalue = \"literal\"\n```"), 80);
            assert!(
                rows.iter().flat_map(|row| &row.spans).any(|span| {
                    span.content.contains("literal")
                        && span.style.fg == Theme::default().style(Role::SyntaxString).fg
                }),
                "{label}: {rows:?}"
            );
        }
    }

    #[test]
    fn highlighted_code_wraps_safely_without_changing_text_or_following_prose() {
        let source =
            "> - ```rust\n>   let e\u{0301} = \"界\u{1b}[2J\";\n>   \t// tail\n>   ```\n\nplain";
        for width in [1, 2, 7, 20, 80] {
            let rows = view(source, width);
            let text = rows.iter().map(ToString::to_string).collect::<String>();
            assert!(rows.iter().all(|row| row.width() <= width));
            assert!(!text.chars().any(char::is_control));
            assert!(text.contains("e\u{0301}"), "{text}");
            assert!(text.ends_with("plain"), "{text}");
            assert_eq!(
                rows.last().unwrap().spans.last().unwrap().style,
                Theme::default().style(Role::Text)
            );
            if width == 80 {
                assert!(text.contains("│     let e\u{0301} = \"界�[2J\";"), "{text}");
            }
        }
        for (body, expected) in [("", vec!["code · rust"]), ("\n", vec!["code · rust", "  "])] {
            let rows = view(&format!("```rust\n{body}```"), 80);
            assert_eq!(
                rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
                expected
            );
        }
        for label in ["", "unknown-language", "text"] {
            let rows = view(&format!("```{label}\nlet n = 42;\n\n```"), 80);
            assert!(
                rows.iter()
                    .flat_map(|row| &row.spans)
                    .filter(|span| !span.content.trim().is_empty()
                        && !span.content.starts_with("code ·"))
                    .all(|span| span.style.fg == Theme::default().style(Role::Code).fg)
            );
            assert_eq!(rows.last().unwrap().to_string(), "  ");
        }
    }

    #[test]
    fn paragraph_boundaries_and_empty_items_survive_container_layout() {
        for (source, expected) in [
            ("> a\n>\n> b", "│ a\n│ \n│ b"),
            ("- a\n\n  b", "• a\n  \n  b"),
            ("-\n- kept", "• \n• kept"),
        ] {
            let text = view(source, 80)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text, expected, "{source}");
        }
    }

    #[test]
    fn wrapping_preserves_graphemes_and_nested_emphasis_without_style_leaks() {
        let rows = view("**e**\u{0301}界 *italic* plain", 5);
        assert_eq!(
            rows.iter().map(ToString::to_string).collect::<String>(),
            "e\u{0301}界 italic plain"
        );
        assert!(rows.iter().all(|row| row.width() <= 5));
        assert!(
            rows[0]
                .spans
                .iter()
                .any(|span| span.content.contains("e\u{0301}")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert_eq!(
            rows.last().unwrap().spans.last().unwrap().style,
            Theme::default().style(Role::Text)
        );
        assert!(view("界\t界", 1).iter().all(|row| row.width() <= 1));
    }
}
