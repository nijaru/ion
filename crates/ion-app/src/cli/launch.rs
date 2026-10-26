//! One launch prompt and explicit file operands; `--` protects literal text.
use std::path::Path;

use anyhow::{Result, ensure};
use clap::Args;

#[derive(Args, Default)]
pub(crate) struct LaunchInput {
    /// One prompt and any number of @file operands (UTF-8 text or supported images).
    #[arg(id = "prompt", value_name = "PROMPT|@FILE", num_args = 0..)]
    operands: Vec<String>,
    /// Literal prompt after --, including a leading @ or command name.
    #[arg(last = true, value_name = "LITERAL_PROMPT")]
    literal_prompt: Option<String>,
}

impl LaunchInput {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.operands
                .iter()
                .filter(|value| !value.starts_with('@'))
                .count()
                + usize::from(self.literal_prompt.is_some())
                <= 1,
            "supply one prompt argument; quote text containing spaces"
        );
        ensure!(
            !self.operands.iter().any(|value| value == "@"),
            "a file operand needs a path after @; use -- @ for literal text"
        );
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.operands.is_empty() && self.literal_prompt.is_none()
    }

    pub(crate) fn prompt(&self) -> Option<&str> {
        self.literal_prompt.as_deref().or_else(|| {
            self.operands
                .iter()
                .find(|value| !value.starts_with('@'))
                .map(String::as_str)
        })
    }

    pub(crate) fn files(&self) -> impl Iterator<Item = &Path> {
        self.operands
            .iter()
            .filter_map(|value| value.strip_prefix('@').map(Path::new))
    }
}
