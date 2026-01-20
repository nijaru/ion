use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub fn top_matches<'a, I>(query: &str, candidates: I, limit: usize) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(&'a str, i64)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            matcher
                .fuzzy_match(candidate, query)
                .map(|score| (candidate, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.truncate(limit);

    scored.into_iter().map(|(candidate, _)| candidate).collect()
}

pub fn top_path_matches(query: &str, root: &Path, limit: usize) -> Vec<PathBuf> {
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(PathBuf, i64)> = Vec::new();

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .build()
        .flatten()
    {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let display = path.strip_prefix(root).unwrap_or(path);
        let display_str = display.to_string_lossy();
        if let Some(score) = matcher.fuzzy_match(&display_str, query) {
            scored.push((path.to_path_buf(), score));
        }
    }

    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.truncate(limit);
    scored.into_iter().map(|(path, _)| path).collect()
}
