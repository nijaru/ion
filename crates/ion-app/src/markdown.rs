//! Model-authored Markdown presentation. No HTML renderer, fetches or terminal links.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

mod code;
mod table;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    display_text::{Component, MappedLine, ParsedComponent, Wrapping, wrap_mapped},
    presentation_style::{Role, Theme},
};

struct Prefix {
    first: String,
    continuation: String,
    used: bool,
}

struct View<'a> {
    rows: &'a mut Vec<MappedLine>,
    source: &'a str,
    range: Range<usize>,
    theme: &'a Theme,
    width: usize,
    content: MappedLine,
    separator: Option<MappedLine>,
    styles: Vec<Style>,
    prefixes: Vec<Prefix>,
    lists: Vec<Option<u64>>,
    links: Vec<(String, usize, usize)>,
    wrapping: Wrapping,
}

pub(super) fn render(rows: &mut Vec<Line<'static>>, source: &str, width: usize, theme: &Theme) {
    rows.extend(
        render_mapped(source, width, theme)
            .into_iter()
            .map(|row| row.line),
    );
}

pub(super) fn render_mapped(source: &str, width: usize, theme: &Theme) -> Vec<MappedLine> {
    let mut rows = Vec::new();
    let mut view = View::new(&mut rows, source, width, theme.style(Role::Text), theme);
    let mut events = Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter();
    while let Some((event, range)) = events.next() {
        view.range = range.clone();
        match event {
            Event::Start(Tag::Table(alignments)) => view.table(&mut events, alignments),
            Event::Start(Tag::CodeBlock(kind)) => view.code_block(&mut events, kind),
            event => view.event(event, range),
        }
    }
    view.flush();
    while view
        .rows
        .last()
        .is_some_and(|row| row.line.spans.is_empty())
    {
        view.rows.pop();
    }
    rows
}

impl<'a> View<'a> {
    fn new(
        rows: &'a mut Vec<MappedLine>,
        source: &'a str,
        width: usize,
        base: Style,
        theme: &'a Theme,
    ) -> Self {
        Self {
            rows,
            source,
            range: 0..0,
            theme,
            width,
            content: MappedLine::default(),
            separator: None,
            styles: vec![base],
            prefixes: Vec::new(),
            lists: Vec::new(),
            links: Vec::new(),
            wrapping: Wrapping::Words,
        }
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        self.range = range;
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                self.parsed_text(text.into_string(), self.style(), ParsedComponent::Text)
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.parsed_text(text.into_string(), self.style(), ParsedComponent::Html)
            }
            Event::Code(text) => self.component_text(
                text.into_string(),
                self.style().patch(self.theme.style(Role::Code)),
                ParsedComponent::Code,
            ),
            Event::SoftBreak => self.parsed_text(" ".into(), self.style(), ParsedComponent::Text),
            Event::HardBreak => self.parsed_text("\n".into(), self.style(), ParsedComponent::Text),
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
        self.content.append(&MappedLine::generated(
            Line::from(Span::styled(text, self.style())),
            self.range.start,
        ));
    }

    fn parsed_text(&mut self, text: String, style: Style, kind: ParsedComponent) {
        if self.source[self.range.clone()] == text {
            self.content.append(&MappedLine::raw(
                Line::from(Span::styled(text, style)),
                self.range.clone(),
            ));
        } else {
            self.component_text(text, style, kind);
        }
    }

    fn component_text(&mut self, text: String, style: Style, kind: ParsedComponent) {
        self.content.append(&MappedLine::component(
            Line::from(Span::styled(text, style)),
            Component::Parsed {
                node: self.range.start,
                kind,
            },
            0,
        ));
    }

    fn flush(&mut self) {
        if self.content.line.spans.is_empty() {
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
        self.rows.extend(wrap_mapped(
            &first,
            &continuation,
            std::mem::take(&mut self.content),
            self.width,
            self.wrapping,
        ));
        for prefix in &mut self.prefixes {
            prefix.used = true;
        }
    }

    fn separate(&mut self) {
        if self.prefixes.is_empty() {
            self.separator = Some(MappedLine::generated(Line::default(), self.range.start));
        }
    }

    fn paragraph_separator(&mut self) {
        let prefix = self
            .prefixes
            .iter()
            .map(|prefix| prefix.continuation.as_str())
            .collect::<String>();
        self.separator = Some(MappedLine::generated(
            if prefix.is_empty() {
                Line::default()
            } else {
                Line::raw(prefix)
            },
            self.range.start,
        ));
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
            Tag::Paragraph => self.flush(),
            Tag::HtmlBlock => {
                self.flush();
                self.wrapping = Wrapping::Literal;
            }
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
                self.links.push((
                    dest_url.into_string(),
                    self.content.line.spans.len(),
                    self.content.map.len(),
                ));
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
                self.wrapping = Wrapping::Words;
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
                if let Some((target, start, map_start)) = self.links.pop() {
                    let label = self.content.line.spans[start..]
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    let destination = visible_target(&target);
                    if label == target && destination != target {
                        self.content.line.spans.truncate(start);
                        self.content.map.truncate(map_start);
                        self.text(format!("[{destination}]"));
                    } else if label != target {
                        self.text(" (".into());
                        if destination == target {
                            self.component_text(
                                destination.to_owned(),
                                self.style(),
                                ParsedComponent::LinkDestination,
                            );
                        } else {
                            self.text(destination.to_owned());
                        }
                        self.text(")".into());
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
    use crate::display_text::Origin;

    fn view(source: &str, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        render(&mut rows, source, width, &Theme::default());
        rows
    }

    #[test]
    fn parser_normalization_maps_original_tokens_not_fitted_text_occurrences() {
        for source in [
            "` alpha\nbeta `",
            "`alpha\r\nbeta`",
            "`alpha\rbeta`",
            "> `alpha\n> beta`",
            "- `alpha\n  beta`",
            "-    `alpha\n     beta`",
            "> 2. `alpha\n>    beta`",
            "- > `alpha\n  > beta`",
            "\t`literal`",
            "| A | B |\n| --- | --- |\n| `a\\|beta` | x |",
            "-\t`alpha\n\tbeta`",
            "`  `",
            "`` `alpha` ``",
            "   - `alpha\n     beta`",
            ">   - `alpha\n>     beta`",
            "> `alpha\nbeta`",
            "- `alpha\nbeta`",
            "> - `alpha\n>\tbeta`",
            "1.   `alpha\n     beta`",
            "  12. `alpha\n      beta`",
            "- - `alpha\n    beta`",
            "-\t> `alpha\n\t> beta`",
            "> -\t`alpha\n>   beta`",
            "> -\t`alpha\n> \tbeta`",
            "- -\t`alpha\n    beta`",
            "> 1.\t`alpha\n>     beta`",
            "intro\r\r> -\t`alpha\r>   beta`",
        ] {
            for width in [1, 7, 80] {
                let mapped = render_mapped(source, width, &Theme::default());
                assert!(
                    mapped.iter().all(|row| row.line.width() <= width),
                    "{source}"
                );
                for (event, range) in
                    Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter()
                {
                    if let Event::Code(text) = event
                        && let Some(byte) = text.find("beta")
                    {
                        let expected = Component::Parsed {
                            node: range.start,
                            kind: ParsedComponent::Code,
                        };
                        assert!(mapped.iter().flat_map(|row| &row.map).any(|mapping| matches!(&mapping.origin, Origin::Component { component, bytes } if *component == expected && bytes.contains(&byte))), "{source}: {mapped:?}");
                    }
                }
            }
        }
        let source = "**e**\u{301} &amp; \\* [label](https://example.org)\n\n```rust\nlet n = \"界\\t\";\n```";
        let rows = render_mapped(source, 7, &Theme::default());
        let entity = source.find("&amp;").unwrap();
        assert!(rows.iter().any(|row| {
            row.map.iter().any(|mapping| {
                mapping.origin
                    == Origin::Component {
                        component: Component::Parsed {
                            node: entity,
                            kind: ParsedComponent::Text,
                        },
                        bytes: 0..1,
                    }
                    && &row.line.to_string()[mapping.display.clone()] == "&"
            })
        }));
        let label = source.find("label").unwrap();
        assert!(
            rows.iter()
                .flat_map(|row| &row.map)
                .any(|mapping| matches!(&mapping.origin, Origin::Raw(raw) if raw.start == label))
        );
        let suppressed = render_mapped("[x](javascript:SECRET)", 8, &Theme::default());
        assert!(
            !suppressed
                .iter()
                .flat_map(|row| &row.map)
                .any(|mapping| matches!(
                    mapping.origin,
                    Origin::Component {
                        component: Component::Parsed {
                            kind: ParsedComponent::LinkDestination,
                            ..
                        },
                        ..
                    }
                ))
        );
        let code = source.find("let n").unwrap();
        assert!(
            rows.iter()
                .flat_map(|row| &row.map)
                .any(|mapping| matches!(&mapping.origin, Origin::Raw(raw) if raw.start == code))
        );
    }

    #[test]
    fn valid_tabbed_quote_list_code_uses_parser_text_without_a_second_normalizer() {
        let source = ">\t*\t`\n>     a`";
        let parsed = Parser::new(source)
            .find_map(|event| {
                if let Event::Code(text) = event {
                    Some(text.into_string())
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(parsed, "     a");
        let rows = view(source, 80);
        assert!(rows.iter().any(|row| row.to_string().contains(&parsed)));
    }

    #[test]
    fn paragraphs_wrap_at_words_but_code_blocks_remain_literal() {
        let rows = view("alpha beta\n\n```text\nalpha beta\n```\n\nalpha beta", 10);
        assert!(
            rows.windows(2).any(|pair| {
                pair[0].to_string() == "  alpha be" && pair[1].to_string() == "  ta"
            })
        );
        assert_eq!(rows.last().unwrap().to_string(), "alpha beta");
        let prose = view("alpha beta", 8);
        assert_eq!(
            prose.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["alpha ", "beta"]
        );
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
