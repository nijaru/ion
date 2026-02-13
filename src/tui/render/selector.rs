//! Selector UI rendering (provider, model, session pickers).
//!
//! Terminal APIs use u16 for dimensions; numeric casts are intentional.
#![allow(clippy::cast_possible_truncation)]

use crate::tui::rnk_text::render_truncated_text_line;
use crate::tui::util::{display_width, truncate_to_display_width};
use crossterm::{
    cursor::MoveTo,
    execute,
    terminal::{Clear, ClearType},
};
use rnk::components::{Span, Text};
use rnk::core::Color as RnkColor;
use std::io::Write;

/// Maximum visible items in selector list.
pub const MAX_VISIBLE_ITEMS: u16 = 15;

/// A single item in the selector list.
pub struct SelectorItem {
    pub label: String,
    pub is_valid: bool,
    pub hint: String,
    /// Optional warning text (rendered in yellow).
    pub warning: Option<String>,
}

/// Data needed to render the selector UI.
pub struct SelectorData {
    pub title: &'static str,
    pub description: &'static str,
    pub items: Vec<SelectorItem>,
    pub selected_idx: usize,
    pub filter_text: String,
    pub show_tabs: bool,
    pub active_tab: usize, // 0 = providers, 1 = models
}

fn paint_row_text<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    text: &str,
    color: Option<RnkColor>,
    bold: bool,
    dim: bool,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 {
        return Ok(());
    }

    let clipped = truncate_to_display_width(text, max_cells);
    let mut line = Text::new(clipped);
    if let Some(color) = color {
        line = line.color(color);
    }
    if bold {
        line = line.bold();
    }
    if dim {
        line = line.dim();
    }
    let rendered = render_truncated_text_line(line, max_cells);
    write!(w, "{rendered}")?;
    Ok(())
}

fn paint_row_spans<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    spans: Vec<Span>,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 || spans.is_empty() {
        return Ok(());
    }
    let rendered = render_truncated_text_line(Text::spans(spans), max_cells);
    write!(w, "{rendered}")?;
    Ok(())
}

fn push_clipped_span(
    spans: &mut Vec<Span>,
    text: &str,
    remaining: &mut usize,
    color: Option<RnkColor>,
    bold: bool,
    dim: bool,
) {
    if *remaining == 0 {
        return;
    }
    let clipped = truncate_to_display_width(text, *remaining);
    if clipped.is_empty() {
        return;
    }
    *remaining = remaining.saturating_sub(display_width(&clipped));

    let mut span = Span::new(clipped);
    if let Some(color) = color {
        span = span.color(color);
    }
    if bold {
        span = span.bold();
    }
    if dim {
        span = span.dim();
    }
    spans.push(span);
}

/// Render the selector UI. Returns (`filter_cursor_col`, `filter_cursor_row`) for cursor
/// positioning.
pub fn render_selector<W: Write>(
    w: &mut W,
    data: &SelectorData,
    start_row: u16,
    width: u16,
) -> std::io::Result<(u16, u16)> {
    execute!(w, MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
    let line_width = width.saturating_sub(1) as usize;

    let mut row = start_row;

    // Tab bar (provider/model) or section header (session selector).
    if data.show_tabs {
        let tabs = if data.active_tab == 0 {
            " [Providers]  Models"
        } else {
            " Providers  [Models]"
        };
        paint_row_text(w, row, width, tabs, None, false, false)?;
    } else {
        paint_row_spans(
            w,
            row,
            width,
            vec![
                Span::new(" "),
                Span::new(data.title).color(RnkColor::Yellow).bold(),
            ],
        )?;
    }
    row += 1;

    // Description.
    let description = format!(" {}", data.description);
    paint_row_text(w, row, width, &description, None, false, false)?;
    row += 1;

    // Search box top border.
    let title_block = format!(" {} ", data.title);
    let border_fill = "─".repeat(
        line_width
            .saturating_sub(display_width("┌─"))
            .saturating_sub(display_width(&title_block))
            .saturating_sub(display_width("┐")),
    );
    let top_border = format!("┌─{title_block}{border_fill}┐");
    paint_row_text(
        w,
        row,
        width,
        &top_border,
        Some(RnkColor::Cyan),
        false,
        false,
    )?;
    row += 1;

    // Search box query row.
    let query_budget = line_width.saturating_sub(display_width("│ "));
    let query = truncate_to_display_width(&data.filter_text, query_budget);
    paint_row_spans(
        w,
        row,
        width,
        vec![
            Span::new("│").color(RnkColor::Cyan),
            Span::new(" "),
            Span::new(query.clone()),
        ],
    )?;
    let filter_cursor_col =
        ((display_width("│ ") + display_width(&query)) as u16).min(width.saturating_sub(1));
    let filter_cursor_row = row;
    row += 1;

    let bottom_border = format!("└{}┘", "─".repeat(line_width.saturating_sub(2)));
    paint_row_text(
        w,
        row,
        width,
        &bottom_border,
        Some(RnkColor::Cyan),
        false,
        false,
    )?;
    row += 1;

    // Render list items.
    row = render_list(w, data, row, width)?;

    // Hint line.
    paint_row_text(
        w,
        row,
        width,
        " Type to filter • Enter to select • Esc to close",
        None,
        false,
        true,
    )?;

    Ok((filter_cursor_col, filter_cursor_row))
}

/// Render the selector item list with scrolling. Returns the next row after the list.
fn render_list<W: Write>(
    w: &mut W,
    data: &SelectorData,
    start_row: u16,
    width: u16,
) -> std::io::Result<u16> {
    let list_height = (data.items.len() as u16).clamp(3, MAX_VISIBLE_ITEMS);

    // Keep selection visible.
    let scroll_offset = if data.selected_idx >= list_height as usize {
        data.selected_idx.saturating_sub(list_height as usize - 1)
    } else {
        0
    };

    let visible_items: Vec<_> = data
        .items
        .iter()
        .skip(scroll_offset)
        .take(list_height as usize)
        .collect();

    let mut row = start_row;
    let line_width = width.saturating_sub(1) as usize;

    for (i, item) in visible_items.into_iter().enumerate() {
        let actual_idx = scroll_offset + i;
        let is_selected = actual_idx == data.selected_idx;
        let default_color = is_selected.then_some(RnkColor::Yellow);
        let default_bold = is_selected;
        let default_dim = !is_selected && !item.is_valid;

        let mut spans = Vec::new();
        let mut remaining = line_width;

        let prefix = if is_selected { " >" } else { "  " };
        push_clipped_span(
            &mut spans,
            prefix,
            &mut remaining,
            default_color,
            default_bold,
            default_dim,
        );
        let marker = if item.is_valid { " ● " } else { " ○ " };
        push_clipped_span(
            &mut spans,
            marker,
            &mut remaining,
            default_color,
            default_bold,
            default_dim,
        );
        push_clipped_span(
            &mut spans,
            &item.label,
            &mut remaining,
            default_color,
            default_bold,
            default_dim,
        );

        if !item.hint.is_empty() {
            push_clipped_span(
                &mut spans,
                "  ",
                &mut remaining,
                default_color,
                default_bold,
                default_dim,
            );
            push_clipped_span(
                &mut spans,
                &item.hint,
                &mut remaining,
                default_color,
                default_bold,
                true,
            );
        }

        if let Some(warning) = &item.warning {
            push_clipped_span(
                &mut spans,
                "  ",
                &mut remaining,
                default_color,
                default_bold,
                default_dim,
            );
            push_clipped_span(
                &mut spans,
                warning,
                &mut remaining,
                Some(RnkColor::Yellow),
                default_bold,
                default_dim,
            );
        }

        paint_row_spans(w, row, width, spans)?;
        row += 1;
    }

    Ok(row)
}
