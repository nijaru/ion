use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

pub fn top_matches<'a, I>(query: &str, candidates: I, limit: usize) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let query_lower = query.to_lowercase();
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(&'a str, bool, i64)> = Vec::new();
    let mut has_substring = false;
    for candidate in candidates {
        let is_substring = candidate.to_lowercase().contains(&query_lower);
        if is_substring {
            has_substring = true;
        }
        if let Some(score) = matcher.fuzzy_match(candidate, query) {
            scored.push((candidate, is_substring, score));
        } else if is_substring {
            scored.push((candidate, true, 0));
        }
    }

    if has_substring {
        scored.retain(|(_, is_substring, _)| *is_substring);
    }
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
    scored.truncate(limit);

    scored
        .into_iter()
        .map(|(candidate, _, _)| candidate)
        .collect()
}
