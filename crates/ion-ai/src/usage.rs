use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// `None` means the provider did not report usage, which is distinct from a
    /// reported zero. An interrupted request must not be recorded as zero cost.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl Usage {
    #[must_use]
    pub const fn known(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some()
    }
}
