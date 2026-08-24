/// Features requested by a frontend before it starts consuming input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRequirements {
    pub bracketed_paste: bool,
    pub keyboard_enhancement: bool,
    pub synchronized_output: bool,
    pub focus_reporting: bool,
    pub mouse: bool,
}

impl Default for TerminalRequirements {
    fn default() -> Self {
        Self {
            bracketed_paste: true,
            keyboard_enhancement: true,
            synchronized_output: false,
            focus_reporting: false,
            mouse: false,
        }
    }
}
