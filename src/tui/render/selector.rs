//! Selector UI rendering (provider, model, session pickers).
//!
//! Terminal APIs use u16 for dimensions; numeric casts are intentional.
#![allow(clippy::cast_possible_truncation)]

use crate::tui::util::{display_width, truncate_to_display_width};
use crossterm::{
    cursor::MoveTo,
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
};
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

/// Render the selector UI. Returns (`filter_cursor_col`, `filter_cursor_row`) for cursor positioning.
pub fn render_selector<W: Write>(
    w: &mut W,
    data: &SelectorData,
    start_row: u16,
    width: u16,
) -> std::io::Result<(u16, u16)> {
    // Clear from start_row to bottom
    execute!(w, MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
    let line_width = width.saturating_sub(1) as usize;

    let mut row = start_row;

    // Tab bar (only for provider/model, session has its own header)
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    if data.show_tabs {
        let tabs = if data.active_tab == 0 {
            " [Providers]  Models"
        } else {
            " Providers  [Models]"
        };
        write!(w, "{}", truncate_to_display_width(tabs, line_width))?;
    } else {
        execute!(
            w,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print(" "),
            Print(truncate_to_display_width(
                data.title,
                line_width.saturating_sub(1)
            )),
            SetAttribute(Attribute::Reset),
            ResetColor
        )?;
    }
    row += 1;

    // Description
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let description = format!(" {}", data.description);
    write!(w, "{}", truncate_to_display_width(&description, line_width))?;
    row += 1;

    // Search box top border
    let top_border = format!(
        "┌─ {} {}┐",
        data.title,
        "─".repeat((width as usize).saturating_sub(data.title.len() + 5))
    );
    execute!(
        w,
        MoveTo(0, row),
        Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::Cyan),
        Print(truncate_to_display_width(&top_border, line_width)),
        ResetColor
    )?;
    row += 1;

    let query_budget = line_width.saturating_sub(display_width("│ "));
    let query = truncate_to_display_width(&data.filter_text, query_budget);
    execute!(
        w,
        MoveTo(0, row),
        Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::Cyan),
        Print("│"),
        ResetColor,
        Print(" "),
        Print(&query),
    )?;
    // Save cursor position for filter input
    let filter_cursor_col =
        ((display_width("│ ") + display_width(&query)) as u16).min(width.saturating_sub(1));
    let filter_cursor_row = row;
    row += 1;

    let bottom_border = format!("└{}┘", "─".repeat((width as usize).saturating_sub(2)));
    execute!(
        w,
        MoveTo(0, row),
        Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::Cyan),
        Print(truncate_to_display_width(&bottom_border, line_width)),
        ResetColor
    )?;
    row += 1;

    // Render list items
    row = render_list(w, data, row, width)?;

    // Hint line
    execute!(
        w,
        MoveTo(0, row),
        Clear(ClearType::CurrentLine),
        SetAttribute(Attribute::Dim)
    )?;
    write!(
        w,
        "{}",
        truncate_to_display_width(
            " Type to filter • Enter to select • Esc to close",
            line_width
        )
    )?;
    execute!(w, SetAttribute(Attribute::Reset))?;

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

    // Calculate scroll offset to keep selection visible
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
    for (i, item) in visible_items.into_iter().enumerate() {
        execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let actual_idx = scroll_offset + i;
        let is_selected = actual_idx == data.selected_idx;

        let mut line = String::new();
        if is_selected {
            line.push_str(" >");
        } else {
            line.push_str("  ");
        }

        if item.is_valid {
            line.push_str(" ● ");
        } else {
            line.push_str(" ○ ");
        }

        line.push_str(&item.label);

        if !item.hint.is_empty() {
            line.push_str("  ");
            line.push_str(&item.hint);
        }

        if let Some(ref warning) = item.warning {
            line.push_str("  ");
            line.push_str(warning);
        }

        let clipped = truncate_to_display_width(&line, width.saturating_sub(1) as usize);
        if is_selected {
            execute!(
                w,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
        } else if !item.is_valid {
            execute!(w, SetAttribute(Attribute::Dim))?;
        }
        write!(w, "{clipped}")?;
        execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;

        row += 1;
    }

    Ok(row)
}
