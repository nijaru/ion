//! Shared popup rendering for command completer, file completer, and history search.

use crossterm::{
    cursor::MoveTo,
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// Visual style for a popup list.
#[derive(Clone, Copy)]
pub struct PopupStyle {
    pub primary_color: Color,
    pub show_secondary_dimmed: bool,
    /// Apply Dim attribute to unselected items (e.g., history search).
    pub dim_unselected: bool,
}

/// A single item in a popup list.
pub struct PopupItem<'a> {
    pub primary: &'a str,
    pub secondary: &'a str,
    pub is_selected: bool,
    /// Override the style's primary color for this item.
    pub color_override: Option<Color>,
}

/// Re-export Region as PopupRegion for popup callers.
pub use crate::tui::render::layout::Region as PopupRegion;

/// Render a popup list within a given region.
/// Items render top-down starting at `region.row`.
/// Each row is cleared before drawing.
#[allow(clippy::cast_possible_truncation)]
pub fn render_popup<W: Write>(
    w: &mut W,
    items: &[PopupItem],
    region: PopupRegion,
    style: PopupStyle,
    popup_width: u16,
) -> std::io::Result<()> {
    for (i, item) in items.iter().enumerate().take(region.height as usize) {
        let row = region.row + i as u16;

        execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

        if item.is_selected {
            execute!(w, SetAttribute(Attribute::Reverse))?;
        } else if style.dim_unselected {
            execute!(w, SetAttribute(Attribute::Dim))?;
        }

        let color = item.color_override.unwrap_or(style.primary_color);

        // Primary text in configured color
        execute!(
            w,
            Print(" "),
            SetForegroundColor(color),
            Print(item.primary),
            ResetColor,
        )?;

        // Secondary text (dimmed)
        if !item.secondary.is_empty() && style.show_secondary_dimmed {
            execute!(
                w,
                SetAttribute(Attribute::Dim),
                Print(item.secondary),
                SetAttribute(Attribute::NormalIntensity),
            )?;
        }

        // Pad to popup width for consistent reverse-video highlight
        let secondary_len = if style.show_secondary_dimmed {
            item.secondary.len()
        } else {
            0
        };
        let content_len = 1 + item.primary.len() + secondary_len;
        let padding = (popup_width as usize).saturating_sub(content_len);
        if padding > 0 {
            execute!(w, Print(" ".repeat(padding)))?;
        }

        if item.is_selected || style.dim_unselected {
            execute!(w, SetAttribute(Attribute::Reset))?;
        }
    }

    Ok(())
}
