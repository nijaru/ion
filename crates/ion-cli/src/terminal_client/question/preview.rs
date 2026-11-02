//! A bounded, independently pageable Markdown preview in a closed box.
use ratatui::text::{Line, Span};

use crate::{
    display_text::{fit_line, push_styled},
    presentation_style::{Role, Theme},
};

pub(super) struct Preview {
    pub(super) rows: Vec<Line<'static>>,
    pub(super) offset: usize,
    pub(super) page: usize,
}

pub(super) fn render(
    source: Option<&str>,
    label: &str,
    width: usize,
    height: usize,
    offset: usize,
    theme: &Theme,
) -> Preview {
    // The layout only calls this when all three structural rows fit.
    debug_assert!(width >= 4 && height >= 3);
    let inner = width - 4;
    let mut body = Vec::new();
    if let Some(source) = source {
        crate::markdown::render(&mut body, source, inner, theme);
    } else {
        push_styled(
            &mut body,
            "",
            "",
            Line::styled("No preview for this option", theme.style(Role::Secondary)),
            inner,
        );
    }
    if body.is_empty() {
        body.push(Line::raw(""));
    }
    let visible = body.len().min(height - 2);
    let offset = offset.min(body.len().saturating_sub(visible));
    let total = body.len();
    let border_style = theme.style(Role::Secondary);
    let mut rows = vec![border(
        &format!("Preview · {label}"),
        width,
        '┌',
        '┐',
        theme,
    )];
    for line in body.into_iter().skip(offset).take(visible) {
        let padding = inner.saturating_sub(line.width());
        let mut spans = vec![Span::styled("│ ", border_style)];
        spans.extend(line.spans.into_iter().map(|mut span| {
            span.style = line.style.patch(span.style);
            span
        }));
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(Span::styled(" │", border_style));
        rows.push(Line::from(spans));
    }
    rows.push(border(
        &format!("{}–{}/{total}", offset + 1, offset + visible),
        width,
        '└',
        '┘',
        theme,
    ));
    Preview {
        rows,
        offset,
        page: visible.saturating_sub(1).max(1),
    }
}

fn border(text: &str, width: usize, left: char, right: char, theme: &Theme) -> Line<'static> {
    let text = fit_line(text, width - 4);
    let padding = (width - 4).saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
    Line::styled(
        format!("{left} {text}{}{right}", "─".repeat(padding + 1)),
        theme.style(Role::Secondary),
    )
}
