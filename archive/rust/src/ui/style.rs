//! Style bridge — converts ion rendering types to `tui` style types.

use tui::style::{Color as TuiColor, Style, StyleModifiers};

use crate::tui::terminal::{Color as IonColor, TextStyle};

/// Convert an [`IonColor`] to a [`TuiColor`].
///
/// Ion uses crossterm naming: bright variants have no "Dark" prefix, dark
/// variants have the "Dark" prefix. `tui::Color` uses the inverse convention:
/// standard variants are dark, `Light*` variants are bright.
pub(crate) fn ion_color_to_tui(c: IonColor) -> TuiColor {
    match c {
        IonColor::Reset => TuiColor::Reset,
        IonColor::Black => TuiColor::Black,
        IonColor::DarkGrey => TuiColor::DarkGray,
        // Bright variants (ANSI 8–15)
        IonColor::Red => TuiColor::LightRed,
        IonColor::Green => TuiColor::LightGreen,
        IonColor::Yellow => TuiColor::LightYellow,
        IonColor::Blue => TuiColor::LightBlue,
        IonColor::Magenta => TuiColor::LightMagenta,
        IonColor::Cyan => TuiColor::LightCyan,
        // Dark variants (ANSI 0–7)
        IonColor::DarkRed => TuiColor::Red,
        IonColor::DarkGreen => TuiColor::Green,
        IonColor::DarkYellow => TuiColor::Yellow,
        IonColor::DarkBlue => TuiColor::Blue,
        IonColor::DarkMagenta => TuiColor::Magenta,
        IonColor::DarkCyan => TuiColor::Cyan,
        IonColor::White => TuiColor::White,
        IonColor::Grey => TuiColor::Gray,
        IonColor::Rgb { r, g, b } => TuiColor::Rgb(r, g, b),
        IonColor::AnsiValue(i) => TuiColor::Indexed(i),
    }
}

/// Convert a [`TextStyle`] to a [`tui::Style`].
pub(crate) fn text_style_to_tui(s: &TextStyle) -> Style {
    let mut style = Style::new();
    if let Some(fg) = s.foreground_color {
        style = style.fg(ion_color_to_tui(fg));
    }
    if let Some(bg) = s.background_color {
        style = style.bg(ion_color_to_tui(bg));
    }
    if s.bold {
        style = style.bold();
    }
    if s.dim {
        style = style.dim();
    }
    if s.italic {
        style = style.italic();
    }
    if s.underlined {
        style = style.underline();
    }
    if s.crossed_out {
        style = style.strikethrough();
    }
    if s.reverse {
        style.modifiers |= StyleModifiers::REVERSED;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_mapping_round_trips_expected_ansi() {
        // ion::Red (ANSI bright red 9) should map to tui::LightRed
        assert!(matches!(
            ion_color_to_tui(IonColor::Red),
            TuiColor::LightRed
        ));
        // ion::DarkRed (ANSI 1) should map to tui::Red
        assert!(matches!(ion_color_to_tui(IonColor::DarkRed), TuiColor::Red));
        // ion::Cyan (ANSI bright cyan 11) → tui::LightCyan
        assert!(matches!(
            ion_color_to_tui(IonColor::Cyan),
            TuiColor::LightCyan
        ));
    }
}
