//! Passive accounting over committed history, independent of context selection.
use std::collections::BTreeMap;

use ion_ai::{ModelRef, Usage};
use serde::Serialize;

use crate::{SessionEntry, SessionView};

/// A known subtotal, not an estimate for observations with missing measurements.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenCount {
    /// Wider than each provider's u64 measurement so accumulation does not wrap.
    pub reported: u128,
    /// Number of observations that did not report this particular measurement.
    pub unreported: usize,
}

impl TokenCount {
    fn add(&mut self, value: Option<u64>) {
        match value {
            Some(value) => self.reported += u128::from(value),
            None => self.unreported += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.reported += other.reported;
        self.unreported += other.unreported;
    }
}

/// Counts describe recorded observations, not every attempted provider request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct UsageTotals {
    pub observations: usize,
    /// Includes cache reads and writes; do not add those subsets again.
    pub input_tokens: TokenCount,
    pub output_tokens: TokenCount,
    pub cache_read_input_tokens: TokenCount,
    pub cache_write_input_tokens: TokenCount,
}

impl UsageTotals {
    fn add(&mut self, usage: Usage) {
        self.observations += 1;
        self.input_tokens.add(usage.input_tokens);
        self.output_tokens.add(usage.output_tokens);
        self.cache_read_input_tokens
            .add(usage.cache_read_input_tokens);
        self.cache_write_input_tokens
            .add(usage.cache_write_input_tokens);
    }

    fn merge(&mut self, other: Self) {
        self.observations += other.observations;
        self.input_tokens.merge(other.input_tokens);
        self.output_tokens.merge(other.output_tokens);
        self.cache_read_input_tokens
            .merge(other.cache_read_input_tokens);
        self.cache_write_input_tokens
            .merge(other.cache_write_input_tokens);
    }

    /// Known input plus known output. Missing measurements remain in their fields.
    #[must_use]
    pub fn reported_tokens(&self) -> u128 {
        self.input_tokens.reported + self.output_tokens.reported
    }

    /// Complete only for recorded observations, never a claim of billing coverage.
    #[must_use]
    pub fn has_complete_tokens(&self) -> bool {
        self.observations > 0
            && self.input_tokens.unreported == 0
            && self.output_tokens.unreported == 0
    }
}

/// Captured route identity, not a reinterpretation through today's model catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelUsage {
    pub logical: ModelRef,
    pub effective: ModelRef,
    /// Provider-reported identity; does not identify or authorize a new route.
    pub returned_model: Option<String>,
    pub usage: UsageTotals,
}

/// Recorded history of one Session, including copied and inactive alternatives.
/// Not a bill, account quota, or recursive total of child Sessions. In-flight or
/// failed requests without committed accounting cannot be inferred from this data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionUsage {
    pub assistant: UsageTotals,
    pub compaction: UsageTotals,
    pub cache_warm: UsageTotals,
    /// Stable lexical order by captured logical/effective/returned identities.
    pub models: Vec<ModelUsage>,
}

impl SessionUsage {
    #[must_use]
    pub fn total(&self) -> UsageTotals {
        let mut total = self.assistant;
        total.merge(self.compaction);
        total.merge(self.cache_warm);
        total
    }
}

impl SessionView {
    /// Sum all raw accounting facts exactly once, not only the selected context.
    /// No prices are persisted with these facts, so no historical cost is inferred.
    #[must_use]
    pub fn usage(&self) -> SessionUsage {
        collect(&self.entries)
    }
}

fn collect(entries: &[SessionEntry]) -> SessionUsage {
    let mut result = SessionUsage::default();
    let mut models = BTreeMap::new();
    for entry in entries {
        let (execution, usage, kind) = match entry {
            SessionEntry::Assistant {
                execution, usage, ..
            } => (execution, usage, &mut result.assistant),
            SessionEntry::Compacted {
                execution, usage, ..
            } => (execution, usage, &mut result.compaction),
            SessionEntry::CacheWarm {
                execution, usage, ..
            } => (execution, usage, &mut result.cache_warm),
            _ => continue,
        };
        kind.add(*usage);
        let route = &execution.route;
        let key = (
            route.logical.provider.as_str(),
            route.logical.model.as_str(),
            route.effective.provider.as_str(),
            route.effective.model.as_str(),
            execution.returned_model.as_deref(),
        );
        models
            .entry(key)
            .or_insert_with(UsageTotals::default)
            .add(*usage);
    }
    result.models = models
        .into_iter()
        .map(|((lp, lm, ep, em, returned), usage)| ModelUsage {
            logical: ModelRef {
                provider: lp.into(),
                model: lm.into(),
            },
            effective: ModelRef {
                provider: ep.into(),
                model: em.into(),
            },
            returned_model: returned.map(str::to_owned),
            usage,
        })
        .collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Message, ModelExecution, ModelRoute, ModelRouteReason, ResponseTermination};

    fn execution(logical: &str, effective: &str, returned: Option<&str>) -> ModelExecution {
        ModelExecution {
            route: ModelRoute {
                logical: logical.parse().unwrap(),
                effective: effective.parse().unwrap(),
                reason: ModelRouteReason::UserRequest,
            },
            returned_model: returned.map(str::to_owned),
        }
    }

    fn assistant(execution: ModelExecution, usage: Usage) -> SessionEntry {
        SessionEntry::Assistant {
            turn: 1,
            message: Message {
                role: ion_ai::Role::Assistant,
                content: vec![],
                provider_replay: None,
            },
            tool_activities: vec![],
            execution,
            usage,
            termination: ResponseTermination::Completed,
        }
    }

    #[test]
    fn all_recorded_kinds_survive_context_selection_and_keep_captured_attribution() {
        let routed = execution("logical/auto", "physical/route", Some("served"));
        let direct = execution("other/model", "other/model", None);
        let entries = vec![
            assistant(routed.clone(), Usage::known_with_cache(100, 10, 60, 20)),
            SessionEntry::Compacted {
                through_entry: 1,
                summary: "summary".into(),
                execution: direct,
                usage: Usage::known(30, 5),
            },
            SessionEntry::ContextSelected { head_entry: 0 },
            SessionEntry::CacheWarm {
                turn: 1,
                execution: routed,
                usage: Usage::known_with_cache(100, 1, 100, 0),
            },
        ];
        let stats = collect(&entries);
        assert_eq!(stats.total().reported_tokens(), 246);
        assert_eq!(stats.total().input_tokens.reported, 230);
        assert_eq!(
            stats.total().cache_read_input_tokens,
            TokenCount {
                reported: 160,
                unreported: 1
            }
        );
        assert_eq!(
            (
                stats.assistant.observations,
                stats.compaction.observations,
                stats.cache_warm.observations
            ),
            (1, 1, 1)
        );
        assert_eq!(stats.models.len(), 2);
        assert_eq!(stats.models[0].logical.provider, "logical");
        assert_eq!(stats.models[0].effective.provider, "physical");
        assert_eq!(stats.models[0].returned_model.as_deref(), Some("served"));
        assert_eq!(stats.models[0].usage.reported_tokens(), 211);
        let mut model_total = UsageTotals::default();
        for model in &stats.models {
            model_total.merge(model.usage);
        }
        assert_eq!(model_total, stats.total());
    }

    #[test]
    fn unknown_partial_and_reported_zero_are_not_interchangeable() {
        let route = execution("p/m", "p/m", None);
        let stats = collect(&[
            assistant(route.clone(), Usage::unknown()),
            assistant(
                route.clone(),
                Usage {
                    input_tokens: Some(7),
                    output_tokens: None,
                    cache_read_input_tokens: Some(0),
                    cache_write_input_tokens: None,
                },
            ),
            assistant(route, Usage::known_with_cache(0, 0, 0, 0)),
        ]);
        let total = stats.total();
        assert_eq!(
            total.input_tokens,
            TokenCount {
                reported: 7,
                unreported: 1
            }
        );
        assert_eq!(
            total.output_tokens,
            TokenCount {
                reported: 0,
                unreported: 2
            }
        );
        assert_eq!(
            total.cache_read_input_tokens,
            TokenCount {
                reported: 0,
                unreported: 1
            }
        );
        assert!(!total.has_complete_tokens());
        assert!(!collect(&[]).total().has_complete_tokens());
        assert!(
            collect(&[assistant(execution("p/m", "p/m", None), Usage::known(0, 0))])
                .total()
                .has_complete_tokens()
        );
    }

    #[test]
    fn sums_do_not_wrap_at_the_per_observation_limit() {
        let route = execution("p/m", "p/m", None);
        let stats = collect(&[
            assistant(route.clone(), Usage::known(u64::MAX, u64::MAX)),
            assistant(route, Usage::known(u64::MAX, u64::MAX)),
        ]);
        assert_eq!(stats.total().reported_tokens(), u128::from(u64::MAX) * 4);
    }
}
