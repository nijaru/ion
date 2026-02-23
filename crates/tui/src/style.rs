bitflags::bitflags! {
    /// Text styling modifiers (bold, italic, etc.).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct StyleModifiers: u16 {
        const BOLD          = 0b000000001;
        const DIM           = 0b000000010;
        const ITALIC        = 0b000000100;
        const UNDERLINE     = 0b000001000;
        const BLINK         = 0b000010000;
        const REVERSED      = 0b000100000;
        const HIDDEN        = 0b001000000;
        const STRIKETHROUGH = 0b010000000;
    }
}

/// A terminal color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// Reset to terminal default.
    #[default]
    Reset,
    // Standard ANSI colors (dark variants, indices 0–7)
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    // Bright/light variants (indices 8–15)
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    Gray,
    /// 24-bit true color.
    Rgb(u8, u8, u8),
    /// 256-color indexed.
    Indexed(u8),
}

/// Cell styling: foreground, background, and modifiers.
///
/// Methods return `Self` for builder-style chaining: `Style::new().fg(Color::Red).bold()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: StyleModifiers,
}

impl Style {
    pub const fn new() -> Self {
        Self {
            fg: None,
            bg: None,
            modifiers: StyleModifiers::empty(),
        }
    }

    pub fn fg(mut self, c: Color) -> Self {
        self.fg = Some(c);
        self
    }

    pub fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }

    pub fn bold(mut self) -> Self {
        self.modifiers |= StyleModifiers::BOLD;
        self
    }

    pub fn dim(mut self) -> Self {
        self.modifiers |= StyleModifiers::DIM;
        self
    }

    pub fn italic(mut self) -> Self {
        self.modifiers |= StyleModifiers::ITALIC;
        self
    }

    pub fn underline(mut self) -> Self {
        self.modifiers |= StyleModifiers::UNDERLINE;
        self
    }

    pub fn strikethrough(mut self) -> Self {
        self.modifiers |= StyleModifiers::STRIKETHROUGH;
        self
    }

    pub fn reset(mut self) -> Self {
        self.fg = None;
        self.bg = None;
        self.modifiers = StyleModifiers::empty();
        self
    }

    /// Layer `self` on top of `other` — `self` wins on non-`None` fields.
    pub fn patch(self, other: Style) -> Style {
        Style {
            fg: self.fg.or(other.fg),
            bg: self.bg.or(other.bg),
            modifiers: self.modifiers | other.modifiers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_default_is_empty() {
        let s = Style::default();
        assert_eq!(s.fg, None);
        assert_eq!(s.bg, None);
        assert!(s.modifiers.is_empty());
    }

    #[test]
    fn style_builder_chain() {
        let s = Style::new().fg(Color::Red).bg(Color::Blue).bold().italic();
        assert_eq!(s.fg, Some(Color::Red));
        assert_eq!(s.bg, Some(Color::Blue));
        assert!(s.modifiers.contains(StyleModifiers::BOLD));
        assert!(s.modifiers.contains(StyleModifiers::ITALIC));
    }

    #[test]
    fn style_patch_self_wins_fg() {
        let base = Style::new().fg(Color::Blue);
        let overlay = Style::new().fg(Color::Red);
        let patched = overlay.patch(base);
        assert_eq!(patched.fg, Some(Color::Red));
    }

    #[test]
    fn style_patch_falls_back_to_other() {
        let base = Style::new().fg(Color::Blue).bold();
        let overlay = Style::new().italic();
        let patched = overlay.patch(base);
        assert_eq!(patched.fg, Some(Color::Blue)); // from base
        assert!(patched.modifiers.contains(StyleModifiers::BOLD)); // from base
        assert!(patched.modifiers.contains(StyleModifiers::ITALIC)); // from overlay
    }

    #[test]
    fn style_reset_clears() {
        let s = Style::new().fg(Color::Red).bold().reset();
        assert_eq!(s.fg, None);
        assert!(s.modifiers.is_empty());
    }
}
