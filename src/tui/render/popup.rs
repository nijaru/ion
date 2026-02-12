//! Shared popup rendering for command completer, file completer, and history search.

use crate::tui::util::{display_width, truncate_to_display_width};
use crossterm::{
    cursor::MoveTo,
    execute,
    terminal::{Clear, ClearType},
};
use rnk::components::{Box as RnkBox, Span, Text};
use rnk::core::{Color as RnkColor, FlexDirection, TextWrap};
use std::io::Write;

/// Visual style for a popup list.
#[derive(Clone, Copy)]
pub struct PopupStyle {
    pub primary_color: RnkColor,
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
    pub color_override: Option<RnkColor>,
}

/// Re-export Region as PopupRegion for popup callers.
pub use crate::tui::render::layout::Region as PopupRegion;

fn render_rnk_text_line(text: Text, max_cells: usize) -> String {
    let element = RnkBox::new()
        .flex_direction(FlexDirection::Row)
        .width(max_cells as u16)
        .child(text.wrap(TextWrap::Truncate).into_element())
        .into_element();
    let rendered = rnk::render_to_string_no_trim(&element, max_cells as u16);
    rendered.lines().next().unwrap_or_default().to_string()
}

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
    let max_cells = popup_width as usize;
    if max_cells == 0 {
        return Ok(());
    }

    for (i, item) in items.iter().enumerate().take(region.height as usize) {
        let row = region.row + i as u16;
        execute!(
            w,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            MoveTo(1, row)
        )?;

        let color = item.color_override.unwrap_or(style.primary_color);

        let mut cells_used = 0usize;
        let mut spans = Vec::new();
        spans.push(Span::new(" "));
        cells_used += 1;

        // Primary text in configured color (clamped).
        let primary = truncate_to_display_width(item.primary, max_cells.saturating_sub(cells_used));
        let primary_width = display_width(&primary);
        let mut primary_span = Span::new(primary);
        primary_span = primary_span.color(color);
        if !item.is_selected && style.dim_unselected {
            primary_span = primary_span.dim();
        }
        spans.push(primary_span);
        cells_used += primary_width;

        // Secondary text (dimmed, clamped).
        if !item.secondary.is_empty() && style.show_secondary_dimmed {
            let secondary =
                truncate_to_display_width(item.secondary, max_cells.saturating_sub(cells_used));
            let secondary_width = display_width(&secondary);
            let mut secondary_span = Span::new(secondary).dim();
            if item.is_selected {
                secondary_span = secondary_span.color(RnkColor::BrightWhite);
            }
            spans.push(secondary_span);
            cells_used += secondary_width;
        }

        // Pad to popup width for consistent reverse-video highlight.
        let padding = max_cells.saturating_sub(cells_used);
        if padding > 0 {
            spans.push(Span::new(" ".repeat(padding)));
        }

        if item.is_selected {
            for span in &mut spans {
                span.style.background_color = Some(RnkColor::BrightBlack);
            }
        }

        let rendered = render_rnk_text_line(Text::spans(spans), max_cells);
        write!(w, "{rendered}")?;
    }

    Ok(())
}
