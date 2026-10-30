//! Ordered, local model-cycle policy; binding preflight remains shared control's job.
use ion_ai::ModelRef;

use super::{ModelChoice, Selection};
use crate::CredentialStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelCycleDirection {
    Forward,
    Backward,
}

impl ModelCycleDirection {
    /// Choose from the existing catalogue/configuration order without probing a
    /// provider, saving a default or attempting another route after a failure.
    /// Local credential presence is not remote authentication.
    pub fn candidate(self, choices: Vec<ModelChoice>, current: &ModelRef) -> Option<Selection> {
        let ready: Vec<_> = choices
            .into_iter()
            .filter(|choice| {
                !choice.selected.requires_key || choice.credential != CredentialStatus::Missing
            })
            .collect();
        if ready.len() <= 1 {
            return None;
        }
        // Match Pi's unscoped cycle: an absent current model starts at index 0
        // before taking the requested step, rather than synthesizing a route.
        let index = ready
            .iter()
            .position(|choice| {
                choice.selected.provider == current.provider
                    && choice.selected.model == current.model
            })
            .unwrap_or(0);
        let next = match self {
            Self::Forward => (index + 1) % ready.len(),
            Self::Backward if index == 0 => ready.len() - 1,
            Self::Backward => index - 1,
        };
        ready.into_iter().nth(next).map(|choice| choice.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelStore, SavedSelection, Wire, catalog};
    use std::path::PathBuf;

    fn choices() -> Vec<ModelChoice> {
        let store = ModelStore::new(PathBuf::new());
        let mut choices: Vec<_> = catalog::models()[..3]
            .iter()
            .zip([
                CredentialStatus::Missing,
                CredentialStatus::Saved,
                CredentialStatus::Environment,
            ])
            .map(|(entry, credential)| ModelChoice {
                selected: store
                    .resolve_identity(&ModelRef {
                        provider: entry.provider.into(),
                        model: entry.id.into(),
                    })
                    .unwrap(),
                label: entry.label,
                credential,
            })
            .collect();
        choices.insert(
            1,
            ModelChoice {
                selected: store
                    .resolve_saved(&SavedSelection {
                        provider: "cycling".into(),
                        model: "keyless".into(),
                        endpoint: Some("http://127.0.0.1:1/v1".into()),
                        wire: Some(Wire::ChatCompletions),
                        api_key_env: None,
                        context_window_tokens: None,
                        max_output_tokens: None,
                        image_input: false,
                    })
                    .unwrap(),
                label: "Keyless",
                credential: CredentialStatus::Missing,
            },
        );
        choices
    }

    #[test]
    fn cycles_ready_choices_in_order_with_wrap_and_exact_identity() {
        let identities: Vec<_> = choices()[1..]
            .iter()
            .map(|choice| choice.selected.identity())
            .collect();
        for (index, current) in identities.iter().enumerate() {
            for (direction, expected) in [
                (ModelCycleDirection::Forward, (index + 1) % 3),
                (ModelCycleDirection::Backward, (index + 2) % 3),
            ] {
                let next = direction.candidate(choices(), current).unwrap();
                assert_eq!(next.identity(), identities[expected]);
            }
        }
        let absent = choices()[0].selected.identity();
        for (direction, expected) in [
            (ModelCycleDirection::Forward, 1),
            (ModelCycleDirection::Backward, 2),
        ] {
            assert_eq!(
                direction.candidate(choices(), &absent).unwrap().identity(),
                identities[expected]
            );
        }
    }

    #[test]
    fn zero_or_one_ready_choice_is_a_noop() {
        let current = choices()[1].selected.identity();
        for direction in [ModelCycleDirection::Forward, ModelCycleDirection::Backward] {
            assert!(direction.candidate(Vec::new(), &current).is_none());
            let mut only_one = choices();
            only_one.truncate(2); // One missing native key and one keyless route.
            assert!(direction.candidate(only_one, &current).is_none());
        }
    }
}
