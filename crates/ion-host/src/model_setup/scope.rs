//! Ordered model preferences, not routing authority. Exact identities remain literal.
use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use ignore::overrides::OverrideBuilder;

use super::{ModelChoice, matching};

pub(super) fn resolve(patterns: &[String], choices: Vec<ModelChoice>) -> Result<Vec<ModelChoice>> {
    let candidates: Vec<_> = choices
        .iter()
        .map(|choice| matching::Candidate {
            provider: &choice.selected.provider,
            id: &choice.selected.model,
            name: super::catalog::find(&choice.selected.provider, &choice.selected.model)
                .map_or(choice.selected.model.as_str(), |entry| entry.label),
        })
        .collect();
    let mut order = Vec::new();
    let mut included = HashSet::new();
    for text in patterns {
        let text = text.trim();
        ensure!(!text.is_empty(), "model scope pattern must not be empty");
        let lower = text.to_lowercase();
        let provider = text.split_once('/').and_then(|(prefix, _)| {
            candidates
                .iter()
                .any(|candidate| candidate.provider.eq_ignore_ascii_case(prefix))
                .then_some(prefix)
        });
        let in_provider = |choice: &ModelChoice| {
            provider.is_none_or(|provider| choice.selected.provider.eq_ignore_ascii_case(provider))
        };
        // An outside raw ID must not hide a hard-provider glob.
        let literal = choices
            .iter()
            .filter(|choice| in_provider(choice))
            .any(|choice| {
                choice.selected.model.to_lowercase() == lower
                    || format!("{}/{}", choice.selected.provider, choice.selected.model)
                        .to_lowercase()
                        == lower
            });
        let matches = if literal || !text.contains(['*', '?', '[']) {
            let selected = matching::resolve(&candidates, text, None, |candidate| {
                let choice = choices
                    .iter()
                    .find(|choice| {
                        choice.selected.provider == candidate.provider
                            && choice.selected.model == candidate.id
                    })
                    .expect("scope candidate belongs to choices");
                Ok(choice.is_ready())
            })?;
            choices
                .iter()
                .enumerate()
                .filter_map(|(index, choice)| {
                    (choice.selected.provider == selected.provider
                        && choice.selected.model == selected.id)
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        } else {
            // Anchor both forms: a bare glob matches the complete ID, not the
            // basename of a namespaced ID. This performs no filesystem lookup.
            let mut builder = OverrideBuilder::new("");
            builder
                .case_insensitive(true)?
                .add(&format!("/{text}"))
                .with_context(|| format!("invalid model scope glob {text:?}"))?;
            let matcher = builder.build()?;
            choices
                .iter()
                .enumerate()
                .filter(|(_, choice)| in_provider(choice))
                .filter_map(|(index, choice)| {
                    let full = format!("{}/{}", choice.selected.provider, choice.selected.model);
                    (matcher.matched(&full, false).is_whitelist()
                        || matcher
                            .matched(&choice.selected.model, false)
                            .is_whitelist())
                    .then_some(index)
                })
                .collect::<Vec<_>>()
        };
        ensure!(
            !matches.is_empty(),
            "no known models match scope pattern {text:?}; run `ion models` to list routes"
        );
        for index in matches {
            let selected = &choices[index].selected;
            if included.insert((selected.provider.as_str(), selected.model.as_str())) {
                order.push(index);
            }
        }
    }
    let mut choices: Vec<_> = choices.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .map(|index| choices[index].take().expect("deduplicated scope indices"))
        .collect())
}
