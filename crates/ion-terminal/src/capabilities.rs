/// Whether a terminal feature has been observed, rejected, or not queried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapabilitySupport {
    #[default]
    Unknown,
    Unsupported,
    Supported,
}

/// Features that affect the input and output contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCapabilities {
    pub bracketed_paste: CapabilitySupport,
    pub focus_reporting: CapabilitySupport,
    pub mouse: CapabilitySupport,
    pub kitty_keyboard: CapabilitySupport,
}
