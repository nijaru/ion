//! One compiled gitignore-style glob over paths relative to the search root.
use std::path::Path;

use ignore::overrides::{Override, OverrideBuilder};

use super::{output::preview, pattern};

pub(super) struct GlobFilter {
    matcher: Override,
}

impl GlobFilter {
    pub(super) fn new(root: &Path, pattern_text: &str) -> Result<Self, String> {
        pattern(pattern_text, "glob")?;
        // OverrideBuilder follows ignore-file syntax, where a leading `#` is a
        // comment and a leading `!` negates the rule. Discovery accepts a
        // normal glob in both positions, so escape those prefixes rather than
        // silently producing an empty or inverted matcher.
        let (prefix, rest) = if let Some(rest) = pattern_text.strip_prefix('#') {
            ("\\#", rest)
        } else if let Some(rest) = pattern_text.strip_prefix('!') {
            ("\\!", rest)
        } else {
            ("", pattern_text)
        };
        let glob = format!("{prefix}{rest}");
        let mut builder = OverrideBuilder::new(root);
        builder
            .add(&glob)
            .map_err(|error| format!("invalid glob: {}", preview(&error.to_string(), 512)))?;
        let matcher = builder
            .build()
            .map_err(|error| format!("invalid glob: {}", preview(&error.to_string(), 512)))?;
        Ok(Self { matcher })
    }

    /// Inputs are slash-normalized paths relative to the returned root. A glob
    /// without `/` matches a basename at any depth; a glob containing `/`
    /// matches the relative path. `**` is recursive.
    pub(super) fn matches(&self, relative: &str) -> bool {
        self.matcher
            .matched(Path::new(relative), false)
            .is_whitelist()
    }
}
