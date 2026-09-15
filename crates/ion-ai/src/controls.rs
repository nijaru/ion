//! Per-request generation controls.
//!
//! Every field is explicit: `None` and [`Reasoning::ProviderDefault`] mean the
//! wire field is deliberately omitted, not "use whatever the host defaults to
//! today". A request that needs a default to be interpreted is not reproducible,
//! and a frozen request must mean the same thing when it is replayed.
//!
//! Values are validated here once, for both callers: durable configuration
//! refuses a malformed setting before it is stored, and a provider refuses it
//! again before it is encoded. Model-specific ranges are *not* this type's
//! concern; an adapter or profile applies those, because they differ per model.

use serde::{Deserialize, Serialize};

use crate::{ProviderError, ProviderErrorKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationControls {
    /// Positive output cap. A provider without an optional cap still sends one.
    pub max_output_tokens: u32,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub reasoning: Reasoning,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: bool,
}

impl GenerationControls {
    /// Reject controls that cannot be encoded unambiguously.
    ///
    /// Deliberately not `Unsupported`: these values are invalid for every
    /// provider, whereas an unsupported *combination* is a profile decision.
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.max_output_tokens == 0 {
            return Err(invalid("max_output_tokens must be positive"));
        }
        if let Some(temperature) = self.temperature
            && (!temperature.is_finite() || temperature < 0.0)
        {
            return Err(invalid("temperature must be a finite number >= 0"));
        }
        if let Some(top_p) = self.top_p
            && (!top_p.is_finite() || top_p <= 0.0 || top_p > 1.0)
        {
            return Err(invalid("top_p must be a finite number in (0, 1]"));
        }
        if let Reasoning::BudgetTokens(budget) = self.reasoning
            && budget == 0
        {
            return Err(invalid("a reasoning token budget must be positive"));
        }
        if let ToolChoice::Named(name) = &self.tool_choice
            && name.is_empty()
        {
            return Err(invalid("a named tool choice needs a tool name"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reasoning {
    /// Omit the reasoning field entirely.
    ProviderDefault,
    /// Explicitly disable reasoning, or fail as unsupported.
    Off,
    Low,
    Medium,
    High,
    /// A concrete token budget, when the target profile supports one.
    BudgetTokens(u32),
}

impl Reasoning {
    /// Whether the provider must encode this as a disabled reasoning field.
    #[must_use]
    pub const fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    Named(String),
}

impl ToolChoice {
    /// Whether this choice refers to tools at all.
    ///
    /// A profile omits the choice and parallel-tool fields when no tools are
    /// offered, because the constraint is vacuous there rather than unsupported.
    #[must_use]
    pub const fn uses_tools(&self) -> bool {
        !matches!(self, Self::None)
    }
}

fn invalid(message: &str) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::InvalidRequest,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls() -> GenerationControls {
        GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        }
    }

    #[test]
    fn omitted_sampling_is_not_a_default_value() {
        let controls = controls();
        assert!(controls.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&controls).expect("serialize"),
            serde_json::json!({
                "max_output_tokens": 4096,
                "temperature": null,
                "top_p": null,
                "reasoning": "ProviderDefault",
                "tool_choice": "Auto",
                "parallel_tool_calls": false,
            })
        );
    }

    #[test]
    fn malformed_controls_are_invalid_not_unsupported() {
        let cases: Vec<GenerationControls> = vec![
            GenerationControls {
                max_output_tokens: 0,
                ..controls()
            },
            GenerationControls {
                temperature: Some(-0.1),
                ..controls()
            },
            GenerationControls {
                temperature: Some(f64::NAN),
                ..controls()
            },
            GenerationControls {
                top_p: Some(0.0),
                ..controls()
            },
            GenerationControls {
                top_p: Some(1.5),
                ..controls()
            },
            GenerationControls {
                reasoning: Reasoning::BudgetTokens(0),
                ..controls()
            },
            GenerationControls {
                tool_choice: ToolChoice::Named(String::new()),
                ..controls()
            },
        ];
        for case in cases {
            let error = case.validate().expect_err("must be rejected");
            assert_eq!(
                error.kind,
                ProviderErrorKind::InvalidRequest,
                "{case:?} must be an invalid request, not unsupported"
            );
        }
    }

    #[test]
    fn boundary_values_are_accepted() {
        let controls = GenerationControls {
            temperature: Some(0.0),
            top_p: Some(1.0),
            reasoning: Reasoning::BudgetTokens(1),
            tool_choice: ToolChoice::Named("read".to_owned()),
            ..controls()
        };
        assert!(controls.validate().is_ok());
    }

    #[test]
    fn vacuous_and_disabled_choices_are_distinguishable() {
        assert!(!ToolChoice::None.uses_tools());
        assert!(ToolChoice::Required.uses_tools());
        assert!(Reasoning::Off.is_off());
        assert!(!Reasoning::ProviderDefault.is_off());
    }
}
