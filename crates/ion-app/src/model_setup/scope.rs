//! Ordered model preferences, not routing authority. Exact identities remain literal.
use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use ignore::overrides::OverrideBuilder;
use ion_ai::ModelRef;
use serde::{Deserialize, Serialize};

use super::{ModelChoice, matching};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelScopeEntry {
    Identity(ModelRef),
    Pattern(String),
}

impl ModelScopeEntry {
    // Saved qualified IDs are literal, even if the route disappears. The
    // explicit object form also round-trips IDs containing glob characters.
    pub(super) fn configured(&self) -> Self {
        if let Self::Pattern(text) = self
            && !text.contains(['*', '?', '['])
            && let Ok(mut identity) = text.trim().parse::<ModelRef>()
        {
            identity.provider.make_ascii_lowercase();
            if super::validate_provider_id(&identity.provider).is_ok() {
                return Self::Identity(identity);
            }
        }
        self.clone()
    }
}

pub(super) fn resolve(
    entries: &[ModelScopeEntry],
    choices: Vec<ModelChoice>,
) -> Result<Vec<ModelChoice>> {
    let order = indices(entries, &choices)?;
    let mut choices: Vec<_> = choices.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .map(|index| choices[index].take().expect("deduplicated scope indices"))
        .collect())
}

pub(super) fn indices(entries: &[ModelScopeEntry], choices: &[ModelChoice]) -> Result<Vec<usize>> {
    let resolver = Resolver::new(choices);
    let mut order = Vec::new();
    let mut included = HashSet::new();
    for entry in entries {
        for index in resolver.matches(entry)? {
            let selected = &choices[index].selected;
            if included.insert((&selected.provider, &selected.model)) {
                order.push(index);
            }
        }
    }
    Ok(order)
}

/// Expand matching preferences in place; preserve missing IDs and unresolved
/// patterns as removable rows. Reading the editor never rewrites configuration.
pub(super) fn checklist(
    entries: &[ModelScopeEntry],
    choices: &[ModelChoice],
) -> Vec<ModelScopeEntry> {
    let resolver = Resolver::new(choices);
    let mut included = HashSet::new();
    entries
        .iter()
        .flat_map(|entry| match entry.configured() {
            identity @ ModelScopeEntry::Identity(_) => vec![identity],
            pattern => match resolver.matches(&pattern) {
                Ok(indices) => indices
                    .into_iter()
                    .map(|index| ModelScopeEntry::Identity(choices[index].selected.identity()))
                    .collect(),
                Err(_) => vec![pattern],
            },
        })
        .filter(|entry| included.insert(entry.clone()))
        .collect()
}

struct Resolver<'a> {
    choices: &'a [ModelChoice],
    candidates: Vec<matching::Candidate<'a>>,
}

impl<'a> Resolver<'a> {
    fn new(choices: &'a [ModelChoice]) -> Self {
        let candidates = choices
            .iter()
            .map(|choice| matching::Candidate {
                provider: &choice.selected.provider,
                id: &choice.selected.model,
                name: super::catalog::find(&choice.selected.provider, &choice.selected.model)
                    .map_or(choice.selected.model.as_str(), |entry| entry.label),
            })
            .collect();
        Self {
            choices,
            candidates,
        }
    }

    fn matches(&self, entry: &ModelScopeEntry) -> Result<Vec<usize>> {
        match entry {
            ModelScopeEntry::Identity(identity) => Ok(self
                .choices
                .iter()
                .position(|choice| {
                    choice.selected.provider == identity.provider
                        && choice.selected.model == identity.model
                })
                .into_iter()
                .collect()),
            ModelScopeEntry::Pattern(text) => self.pattern_matches(text),
        }
    }

    fn pattern_matches(&self, text: &str) -> Result<Vec<usize>> {
        let text = text.trim();
        ensure!(!text.is_empty(), "model scope pattern must not be empty");
        let lower = text.to_lowercase();
        let provider = text.split_once('/').and_then(|(prefix, _)| {
            self.candidates
                .iter()
                .any(|candidate| candidate.provider.eq_ignore_ascii_case(prefix))
                .then_some(prefix)
        });
        let in_provider = |choice: &ModelChoice| {
            provider.is_none_or(|provider| choice.selected.provider.eq_ignore_ascii_case(provider))
        };
        // An outside raw ID must not hide a hard-provider glob.
        let literal = self
            .choices
            .iter()
            .filter(|choice| in_provider(choice))
            .any(|choice| {
                choice.selected.model.to_lowercase() == lower
                    || format!("{}/{}", choice.selected.provider, choice.selected.model)
                        .to_lowercase()
                        == lower
            });
        let matches = if literal || !text.contains(['*', '?', '[']) {
            let selected = matching::resolve(&self.candidates, text, None, |candidate| {
                let choice = self
                    .choices
                    .iter()
                    .find(|choice| {
                        choice.selected.provider == candidate.provider
                            && choice.selected.model == candidate.id
                    })
                    .expect("scope candidate belongs to choices");
                Ok(choice.is_ready())
            })?;
            self.choices
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
            self.choices
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
        Ok(matches)
    }
}
