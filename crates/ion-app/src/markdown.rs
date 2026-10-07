//! Assistant-only Markdown presentation. No HTML renderer, fetches or terminal links.
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::display_text::push_styled;

struct Prefix {
    first: String,
    continuation: String,
    used: bool,
}

struct View<'a> {
    rows: &'a mut Vec<Line<'static>>,
    width: usize,
    spans: Vec<Span<'static>>,
    separator: Option<Line<'static>>,
    styles: Vec<Style>,
    prefixes: Vec<Prefix>,
    lists: Vec<Option<u64>>,
    links: Vec<(String, usize)>,
}

pub(super) fn render(rows: &mut Vec<Line<'static>>, source: &str, width: usize) {
    let start = rows.len();
    let mut view = View {
        rows,
        width,
        spans: Vec::new(),
        separator: None,
        styles: vec![Style::default()],
        prefixes: Vec::new(),
        lists: Vec::new(),
        links: Vec::new(),
    };
    for event in Parser::new(source) {
        match event {
            Event::Start(tag) => view.start(tag),
            Event::End(tag) => view.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                view.text(text.into_string());
            }
            Event::Code(text) => view.spans.push(Span::styled(
                text.into_string(),
                view.style().fg(Color::Magenta),
            )),
            Event::SoftBreak => view.text(" ".into()),
            Event::HardBreak => view.text("\n".into()),
            Event::Rule => {
                view.flush();
                view.text("─".repeat(width.clamp(1, 12)));
                view.flush();
                view.separate();
            }
            // These require parser options which this CommonMark view does not enable.
            _ => {}
        }
    }
    view.flush();
    while view.rows.len() > start && view.rows.last().is_some_and(|row| row.spans.is_empty()) {
        view.rows.pop();
    }
}

impl View<'_> {
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
            Tag::CodeBlock(kind) => {
                self.flush();
                if let CodeBlockKind::Fenced(info) = kind
                    && !info.is_empty()
                {
                    self.spans.push(Span::styled(
                        format!("code · {info}"),
                        Style::default().add_modifier(Modifier::DIM),
                    ));
                    self.flush();
                }
                self.push_prefix("  ".into(), "  ".into());
                self.styles.push(self.style().fg(Color::Magenta));
            }
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push((dest_url.into_string(), self.spans.len()));
                self.styles.push(self.style().fg(Color::Cyan));
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
            TagEnd::CodeBlock => {
                // A parser-supplied final newline terminates the block, not an extra row.
                if let Some(span) = self.spans.last_mut()
                    && span.content.ends_with('\n')
                {
                    span.content.to_mut().pop();
                }
                self.flush();
                self.prefixes.pop();
                self.styles.pop();
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
        render(&mut rows, source, width);
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
                .any(|span| span.content == "code" && span.style.fg == Some(Color::Magenta))
        );
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
        assert!(rows.last().unwrap().spans.last().unwrap().style == Style::default());
        assert!(view("界\t界", 1).iter().all(|row| row.width() <= 1));
    }
}
