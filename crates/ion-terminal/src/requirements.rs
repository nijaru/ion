/// The terminal surface used by the current inline frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalSurface {
    #[default]
    Inline,
    Fullscreen,
}

/// Features requested by a frontend before it starts consuming input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRequirements {
    pub surface: TerminalSurface,
    pub bracketed_paste: bool,
    pub keyboard_enhancement: bool,
    pub synchronized_output: bool,
    pub focus_reporting: bool,
    pub mouse: bool,
}

impl Default for TerminalRequirements {
    fn default() -> Self {
        Self {
            surface: TerminalSurface::Inline,
            bracketed_paste: true,
            keyboard_enhancement: true,
            synchronized_output: false,
            focus_reporting: false,
            mouse: false,
        }
    }
}
