use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Total provider-reported input tokens. This includes cache reads/writes
    /// when the provider reports those as separate sub-buckets.
    ///
    /// `None` means the provider did not report usage, which is distinct from
    /// a reported zero. An interrupted request must not be recorded as zero cost.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Subset of `input_tokens` served from a prompt cache, when reported.
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    /// Subset of `input_tokens` written to a prompt cache, when reported.
    #[serde(default)]
    pub cache_write_input_tokens: Option<u64>,
}

impl Usage {
    #[must_use]
    pub const fn known(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        }
    }

    #[must_use]
    pub const fn known_with_cache(
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_write_input_tokens: u64,
    ) -> Self {
        Self {
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            cache_read_input_tokens: Some(cache_read_input_tokens),
            cache_write_input_tokens: Some(cache_write_input_tokens),
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
        }
    }

    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some()
    }

    /// Ordinary uncached input tokens when both cache sub-buckets were reported
    /// and form a valid subset of total input.
    #[must_use]
    pub fn uncached_input_tokens(&self) -> Option<u64> {
        self.input_tokens?
            .checked_sub(self.cache_read_input_tokens?)?
            .checked_sub(self.cache_write_input_tokens?)
    }
}
