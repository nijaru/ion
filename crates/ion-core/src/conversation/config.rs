//! Durable conversation configuration.
//!
//! A conversation's generations are assembled from this record: the model, the
//! resolved instruction text, the controls, the selected tools, the context
//! policy and the run limits. It is committed as one complete typed
//! replacement, never as a partial patch, so a request assembled from it is
//! reproducible.
//!
//! Instructions and project context are *resolved content*, not paths to read
//! later: the host does discovery and file reading outside mutation authority,
//! then commits the exact text it selected plus the revision of the algorithm
//! that selected it. Recovery reuses that text and never rereads a file, which
//! is what makes a frozen request reproducible.
//!
//! Configuration is not authority. Selecting a tool here does not grant
//! permission to run it, and instruction text is never execution authority;
//! call-time checks are separate and re-evaluated.

use ion_ai::{GenerationControls, Message, ModelRef, ProviderError, Role};
use thiserror::Error;

use crate::CommitSeq;

/// The most attempts one step may be configured to make.
///
/// The attempt ceiling is a property of the frozen request, not of a provider:
/// it bounds durable attempt evidence for one generation, so it is refused at
/// configuration time rather than discovered while dispatching.
pub const MAX_ATTEMPTS_PER_STEP: u32 = 10;

/// A conversation's complete generation configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConversationConfig {
    pub model: ModelRef,
    /// The one exact instruction string: host-authorized base text followed by
    /// selected project instruction text. Never empty-by-accident, because an
    /// empty string is a real instruction ("none selected") and is preserved.
    pub instructions: String,
    pub controls: GenerationControls,
    /// Bounded project context the host selected and resolved. These are user
    /// messages, and they precede the transcript in the assembled request.
    pub project_context: Vec<Message>,
    /// Revision of the host's instruction/project-context selection algorithm.
    /// It records how the two fields above were produced, so a later revision
    /// of that algorithm cannot silently reinterpret them.
    pub instruction_revision: String,
    /// Names of the host tools this conversation selected. Specs are resolved
    /// per request from the current catalog; a selected name that cannot be
    /// resolved fails assembly instead of quietly dropping the tool.
    pub tool_names: Vec<String>,
    pub context: ContextPolicy,
    pub limits: RunLimits,
}

impl ConversationConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.model.provider.is_empty() || self.model.model.is_empty() {
            return Err(ConfigError::EmptyModel);
        }
        if self.instruction_revision.is_empty() {
            return Err(ConfigError::EmptyInstructionRevision);
        }
        self.controls.validate().map_err(ConfigError::Controls)?;
        for (index, message) in self.project_context.iter().enumerate() {
            if message.role != Role::User {
                return Err(ConfigError::ProjectContextNotUser { message: index });
            }
            if message.provider_replay.is_some() {
                return Err(ConfigError::ProjectContextReplay { message: index });
            }
            if message
                .content
                .iter()
                .any(|block| !matches!(block, ion_ai::Content::Text(_)))
            {
                return Err(ConfigError::ProjectContextNotText { message: index });
            }
        }
        for (position, name) in self.tool_names.iter().enumerate() {
            if name.is_empty() {
                return Err(ConfigError::EmptyToolName { position });
            }
            if self.tool_names[..position].contains(name) {
                return Err(ConfigError::DuplicateToolName { name: name.clone() });
            }
        }
        self.context.validate()?;
        self.limits.validate()
    }
}

/// How much context a request may carry and when the turn compacts instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextPolicy {
    pub max_request_bytes: u32,
    /// The model's input budget. Required rather than defaulted: guessing a
    /// context window from a model name would silently admit an over-budget
    /// request, so the host must state the budget it is willing to fill.
    pub max_input_tokens: u32,
    /// Estimated input tokens at which the turn compacts before dispatch.
    pub compact_at_tokens: u32,
    /// Output cap for one compaction summary.
    pub summary_max_tokens: u32,
}

impl ContextPolicy {
    fn validate(&self) -> Result<(), ConfigError> {
        for (setting, value) in [
            ("max_request_bytes", u64::from(self.max_request_bytes)),
            ("max_input_tokens", u64::from(self.max_input_tokens)),
            ("compact_at_tokens", u64::from(self.compact_at_tokens)),
            ("summary_max_tokens", u64::from(self.summary_max_tokens)),
        ] {
            if value == 0 {
                return Err(ConfigError::NonPositiveSetting { setting });
            }
        }
        if self.compact_at_tokens > self.max_input_tokens {
            return Err(ConfigError::CompactionAboveInputBudget {
                compact_at_tokens: self.compact_at_tokens,
                max_input_tokens: self.max_input_tokens,
            });
        }
        Ok(())
    }
}

/// Bounds that one admitted turn cannot exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunLimits {
    pub max_model_steps: u32,
    /// Attempts per model step, including retries of the same frozen request.
    pub max_attempts_per_step: u32,
    /// A monetary ceiling. `None` means no ceiling was supplied, which is
    /// distinct from a ceiling of zero.
    pub max_cost_microusd: Option<u64>,
    /// The whole turn's deadline, measured from admission.
    pub deadline_ms: u64,
    pub max_response_bytes: u32,
    pub max_tool_output_bytes: u32,
}

impl RunLimits {
    fn validate(&self) -> Result<(), ConfigError> {
        for (setting, value) in [
            ("max_model_steps", u64::from(self.max_model_steps)),
            (
                "max_attempts_per_step",
                u64::from(self.max_attempts_per_step),
            ),
            ("deadline_ms", self.deadline_ms),
            ("max_response_bytes", u64::from(self.max_response_bytes)),
            (
                "max_tool_output_bytes",
                u64::from(self.max_tool_output_bytes),
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::NonPositiveSetting { setting });
            }
        }
        if self.max_attempts_per_step > MAX_ATTEMPTS_PER_STEP {
            return Err(ConfigError::AttemptsAboveCeiling {
                configured: self.max_attempts_per_step,
                ceiling: MAX_ATTEMPTS_PER_STEP,
            });
        }
        if self.max_cost_microusd == Some(0) {
            return Err(ConfigError::NonPositiveSetting {
                setting: "max_cost_microusd",
            });
        }
        Ok(())
    }
}

/// Why a configuration was refused. Every rule names the setting it judged, so
/// a client can point at the field it must change instead of guessing.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("the model must name both a provider and a model")]
    EmptyModel,
    #[error("the instruction revision must not be empty")]
    EmptyInstructionRevision,
    #[error("{0}")]
    Controls(ProviderError),
    #[error("project context message {message} is not a user message")]
    ProjectContextNotUser { message: usize },
    #[error("project context message {message} carries provider replay material")]
    ProjectContextReplay { message: usize },
    #[error("project context message {message} is not text")]
    ProjectContextNotText { message: usize },
    #[error("tool name at position {position} is empty")]
    EmptyToolName { position: usize },
    #[error("tool name {name:?} is selected twice")]
    DuplicateToolName { name: String },
    #[error("{setting} must be positive")]
    NonPositiveSetting { setting: &'static str },
    #[error(
        "compaction triggers at {compact_at_tokens} tokens, above the {max_input_tokens} token input budget"
    )]
    CompactionAboveInputBudget {
        compact_at_tokens: u32,
        max_input_tokens: u32,
    },
    #[error("{configured} attempts per step exceeds the {ceiling} attempt ceiling")]
    AttemptsAboveCeiling { configured: u32, ceiling: u32 },
}

/// A configuration as it is installed on a conversation, with the commit that
/// installed it.
///
/// The revision is the compare-and-set basis for reconfiguration and the
/// binding a frozen request records, so it is carried with the content rather
/// than recomputed. Only the writer stamps it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InstalledConfig {
    pub revision: CommitSeq,
    pub config: ConversationConfig,
}

impl InstalledConfig {
    #[must_use]
    pub const fn new(revision: CommitSeq, config: ConversationConfig) -> Self {
        Self { revision, config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Content, Reasoning, ToolChoice};

    fn context_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::Text(text.to_owned())],
            provider_replay: None,
        }
    }

    fn sample() -> ConversationConfig {
        ConversationConfig {
            model: ModelRef {
                provider: "scripted".to_owned(),
                model: "test-model".to_owned(),
            },
            instructions: "be careful".to_owned(),
            controls: GenerationControls {
                max_output_tokens: 4096,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ToolChoice::Auto,
                parallel_tool_calls: false,
            },
            project_context: vec![context_message("project rules")],
            instruction_revision: "ion-instructions-v1".to_owned(),
            tool_names: vec!["read".to_owned(), "edit".to_owned()],
            context: ContextPolicy {
                max_request_bytes: 4 * 1024 * 1024,
                max_input_tokens: 100_000,
                compact_at_tokens: 80_000,
                summary_max_tokens: 2048,
            },
            limits: RunLimits {
                max_model_steps: 20,
                max_attempts_per_step: 3,
                max_cost_microusd: None,
                deadline_ms: 600_000,
                max_response_bytes: 1024 * 1024,
                max_tool_output_bytes: 64 * 1024,
            },
        }
    }

    #[test]
    fn a_complete_configuration_is_valid() {
        assert!(sample().validate().is_ok());
    }

    #[test]
    fn an_unset_monetary_ceiling_is_not_a_zero_ceiling() {
        let mut config = sample();
        assert_eq!(config.limits.max_cost_microusd, None);
        assert!(config.validate().is_ok());

        config.limits.max_cost_microusd = Some(0);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::NonPositiveSetting {
                setting: "max_cost_microusd"
            })
        ));
    }

    #[test]
    fn malformed_controls_are_refused_through_the_contract_validator() {
        let mut config = sample();
        config.controls.max_output_tokens = 0;
        match config.validate() {
            Err(ConfigError::Controls(error)) => {
                assert_eq!(error.kind, ion_ai::ProviderErrorKind::InvalidRequest);
            }
            other => panic!("expected a control error, got {other:?}"),
        }
    }

    #[test]
    fn project_context_is_user_text_only() {
        let mut config = sample();
        config.project_context[0].role = Role::Assistant;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::ProjectContextNotUser { message: 0 })
        ));

        let mut config = sample();
        config.project_context[0].content = vec![Content::ToolCall(ion_ai::ToolCall {
            id: "call-1".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({}),
        })];
        assert!(matches!(
            config.validate(),
            Err(ConfigError::ProjectContextNotText { message: 0 })
        ));

        let mut config = sample();
        config.project_context[0].provider_replay = Some(ion_ai::ProviderReplay::new(
            "other",
            "thinking",
            serde_json::json!({}),
        ));
        assert!(matches!(
            config.validate(),
            Err(ConfigError::ProjectContextReplay { message: 0 })
        ));
    }

    #[test]
    fn a_tool_may_not_be_selected_twice() {
        let mut config = sample();
        config.tool_names = vec!["read".to_owned(), "edit".to_owned(), "read".to_owned()];
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicateToolName { name }) if name == "read"
        ));

        let mut config = sample();
        config.tool_names = vec!["read".to_owned(), String::new()];
        assert!(matches!(
            config.validate(),
            Err(ConfigError::EmptyToolName { position: 1 })
        ));
    }

    #[test]
    fn the_attempt_ceiling_bounds_the_frozen_request() {
        let mut config = sample();
        config.limits.max_attempts_per_step = MAX_ATTEMPTS_PER_STEP;
        assert!(config.validate().is_ok());

        config.limits.max_attempts_per_step = MAX_ATTEMPTS_PER_STEP + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::AttemptsAboveCeiling {
                configured,
                ceiling
            }) if configured == MAX_ATTEMPTS_PER_STEP + 1 && ceiling == MAX_ATTEMPTS_PER_STEP
        ));
    }

    #[test]
    fn compaction_must_trigger_within_the_input_budget() {
        let mut config = sample();
        config.context.compact_at_tokens = config.context.max_input_tokens;
        assert!(config.validate().is_ok());

        config.context.compact_at_tokens = config.context.max_input_tokens + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::CompactionAboveInputBudget { .. })
        ));
    }

    #[test]
    fn an_identity_field_may_not_be_empty() {
        let mut config = sample();
        config.model.model = String::new();
        assert!(matches!(config.validate(), Err(ConfigError::EmptyModel)));

        let mut config = sample();
        config.instruction_revision = String::new();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::EmptyInstructionRevision)
        ));
    }
}
