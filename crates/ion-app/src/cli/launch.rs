//! Ordered launch prompts and explicit files; `--` protects literal text.
use std::path::Path;

use anyhow::{Result, ensure};
use clap::Args;

#[derive(Args, Default)]
pub(crate) struct LaunchInput {
    /// Ordered prompts and @file operands; files accompany the first prompt.
    #[arg(id = "prompt", value_name = "PROMPT|@FILE", num_args = 0..)]
    operands: Vec<String>,
    /// Literal prompts after --, including leading @ and command names.
    #[arg(last = true, value_name = "LITERAL_PROMPT", num_args = 0..)]
    literal_prompts: Vec<String>,
}

impl LaunchInput {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.operands.iter().any(|value| value == "@"),
            "a file operand needs a path after @; use -- @ for literal text"
        );
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.operands.is_empty() && self.literal_prompts.is_empty()
    }

    pub(crate) fn prompts(&self) -> impl Iterator<Item = &str> {
        self.operands
            .iter()
            .filter(|value| !value.starts_with('@'))
            .chain(&self.literal_prompts)
            .map(String::as_str)
    }

    pub(crate) fn into_prompts(self) -> impl Iterator<Item = String> {
        self.operands
            .into_iter()
            .filter(|value| !value.starts_with('@'))
            .chain(self.literal_prompts)
    }

    pub(crate) fn files(&self) -> impl Iterator<Item = &Path> {
        self.operands
            .iter()
            .filter_map(|value| value.strip_prefix('@').map(Path::new))
    }
}
