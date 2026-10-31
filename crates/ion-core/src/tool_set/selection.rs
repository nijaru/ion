//! Model-visible loadout selection; never an OS sandbox or executor permission.

/// Application-selected model-visible tool loadout. Patterns use `*` as a
/// wildcard; exclusions win over inclusions. Selection narrows what the model can
/// call for this application; it is not an OS sandbox or authority grant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSelection {
    include: Option<Vec<String>>,
    exclude: Vec<String>,
}

impl ToolSelection {
    pub fn all() -> Self {
        Self::default()
    }

    /// Allow only matching tools. An empty inclusion allows no tools; use
    /// `all()` to leave inclusion unrestricted.
    pub fn include(patterns: impl IntoIterator<Item = String>) -> Self {
        Self {
            include: Some(patterns.into_iter().collect()),
            exclude: Vec::new(),
        }
    }

    pub fn with_exclude(mut self, patterns: impl IntoIterator<Item = String>) -> Self {
        self.exclude = patterns.into_iter().collect();
        self
    }

    pub fn is_filtered(&self) -> bool {
        self.include.is_some() || !self.exclude.is_empty()
    }

    pub fn allows(&self, name: &str) -> bool {
        let included = self.include.as_ref().is_none_or(|patterns| {
            patterns
                .iter()
                .any(|pattern| tool_name_matches(pattern, name))
        });
        included
            && !self
                .exclude
                .iter()
                .any(|pattern| tool_name_matches(pattern, name))
    }
}

/// Match an exact tool name or a pattern where only `*` is special.
/// A star matches any sequence of characters; other glob/regex syntax is literal.
pub fn tool_name_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.as_bytes();
    let name = name.as_bytes();
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    let mut retry = 0;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some(at) = star {
            p = at + 1;
            retry += 1;
            n = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
