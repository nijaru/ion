//! Selector UI rendering (provider, model, session pickers).
//!
//! Terminal APIs use u16 for dimensions; numeric casts are intentional.
#![allow(clippy::cast_possible_truncation)]

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

    let mut row = start_row;

    // Tab bar (only for provider/model, session has its own header)
    if data.show_tabs {
        let provider_bold = data.active_tab == 0;
        execute!(w, MoveTo(0, row))?;
        write!(w, " ")?;
        if provider_bold {
            execute!(
                w,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
        } else {
            execute!(w, SetAttribute(Attribute::Dim))?;
        }
        write!(w, "Providers")?;
        execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;
        write!(w, "  ")?;
        if provider_bold {
            execute!(w, SetAttribute(Attribute::Dim))?;
        } else {
            execute!(
                w,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
        }
        write!(w, "Models")?;
        execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;
    } else {
        execute!(w, MoveTo(0, row))?;
        execute!(
            w,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print(" "),
            Print(data.title),
            SetAttribute(Attribute::Reset),
            ResetColor
        )?;
    }
    row += 1;

    // Description
    execute!(w, MoveTo(0, row))?;
    write!(w, " {}", data.description)?;
    row += 1;

    // Search box
    execute!(
        w,
        MoveTo(0, row),
        SetForegroundColor(Color::Cyan),
        Print("┌─ "),
        Print(data.title),
        Print(" "),
        Print("─".repeat((width as usize).saturating_sub(data.title.len() + 5))),
        Print("┐"),
        ResetColor
    )?;
    row += 1;

    execute!(
        w,
        MoveTo(0, row),
        SetForegroundColor(Color::Cyan),
        Print("│"),
        ResetColor,
        Print(" "),
        Print(&data.filter_text),
    )?;
    // Save cursor position for filter input
    let filter_cursor_col = 2 + data.filter_text.len() as u16;
    let filter_cursor_row = row;
    execute!(
        w,
        MoveTo(width.saturating_sub(1), row),
        SetForegroundColor(Color::Cyan),
        Print("│"),
        ResetColor
    )?;
    row += 1;

    execute!(
        w,
        MoveTo(0, row),
        SetForegroundColor(Color::Cyan),
        Print("└"),
        Print("─".repeat((width as usize).saturating_sub(2))),
        Print("┘"),
        ResetColor
    )?;
    row += 1;

    // Render list items
    row = render_list(w, data, row)?;

    // Hint line
    execute!(w, MoveTo(0, row), SetAttribute(Attribute::Dim))?;
    write!(w, " Type to filter · Enter to select · Esc to close")?;
    execute!(w, SetAttribute(Attribute::Reset))?;

    Ok((filter_cursor_col, filter_cursor_row))
}

/// Render the selector item list with scrolling. Returns the next row after the list.
fn render_list<W: Write>(w: &mut W, data: &SelectorData, start_row: u16) -> std::io::Result<u16> {
    let list_height = (data.items.len() as u16).clamp(3, MAX_VISIBLE_ITEMS);

    // Calculate scroll offset to keep selection visible
    let scroll_offset = if data.selected_idx >= list_height as usize {
        data.selected_idx.saturating_sub(list_height as usize - 1)
    } else {
        0
    };

    // Calculate max label length for column alignment (visible items only)
    let visible_items: Vec<_> = data
        .items
        .iter()
        .skip(scroll_offset)
        .take(list_height as usize)
        .collect();
    let max_label_len = visible_items
        .iter()
        .map(|item| item.label.chars().count())
        .max()
        .unwrap_or(0);

    let mut row = start_row;
    for (i, item) in visible_items.into_iter().enumerate() {
        execute!(w, MoveTo(0, row))?;
        let actual_idx = scroll_offset + i;
        let is_selected = actual_idx == data.selected_idx;

        // Selection indicator
        if is_selected {
            execute!(
                w,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
            write!(w, " >")?;
        } else {
            write!(w, "  ")?;
        }

        // Validity indicator
        if item.is_valid {
            execute!(w, SetForegroundColor(Color::Green), Print(" ● "))?;
        } else {
            execute!(w, SetAttribute(Attribute::Dim), Print(" ○ "))?;
        }

        // Label styling
        if is_selected {
            execute!(
                w,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
        } else if !item.is_valid {
            execute!(w, SetAttribute(Attribute::Dim))?;
        }

        // Label with padding for column alignment
        let label_len = item.label.chars().count();
        let padding = max_label_len.saturating_sub(label_len);
        write!(w, "{}", item.label)?;
        execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;

        // Auth hint in second column (dimmed)
        if !item.hint.is_empty() {
            write!(w, "{:padding$}  ", "", padding = padding)?;
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, "{}", item.hint)?;
            execute!(w, SetAttribute(Attribute::Reset))?;
        }

        row += 1;
    }

    Ok(row)
}
