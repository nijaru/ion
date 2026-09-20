//! Future-turn configuration and one Turn's frozen semantic environment.
//!
//! ConversationConfig is mutable only as a default for later Turns. Starting a Turn
//! captures an immutable TurnEnvironment plus a constrained TurnSettings selection.
//! Credentials and live authority are deliberately absent.

use std::collections::BTreeSet;

use ion_ai::{GenerationControls, Message, ModelRef, Reasoning, Role, ToolChoice, ToolSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CommitSeq, ContentDigest};

const MAX_ID_BYTES: usize = 160;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
                let value = value.into();
                validate_id(stringify!($name), &value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(SemanticCompatibilityId);
string_id!(ProviderBindingId);
string_id!(ToolBindingId);

fn validate_id(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(ConfigError::EmptyIdentity { field });
    }
    if value.len() > MAX_ID_BYTES {
        return Err(ConfigError::IdentityTooLong {
            field,
            length: value.len(),
            maximum: MAX_ID_BYTES,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EgressRealm {
    Local,
    Remote(String),
}

impl EgressRealm {
    fn validate(&self) -> Result<(), ConfigError> {
        if let Self::Remote(realm) = self {
            validate_id("egress realm", realm)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnedModelPolicy {
    Exact,
    ServerRoute { allowed_family: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub tools: bool,
    pub parallel_tool_calls: bool,
    pub structured_output: bool,
    pub replay: bool,
    pub reasoning: bool,
}

impl ProviderCapabilities {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_input_tokens == 0 {
            return Err(ConfigError::NonPositiveSetting {
                setting: "provider.max_input_tokens",
            });
        }
        if self.max_output_tokens == 0 {
            return Err(ConfigError::NonPositiveSetting {
                setting: "provider.max_output_tokens",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderBinding {
    pub id: ProviderBindingId,
    pub model: ModelRef,
    pub adapter: SemanticCompatibilityId,
    pub request_encoding: SemanticCompatibilityId,
    pub replay_family: Option<SemanticCompatibilityId>,
    pub capabilities: ProviderCapabilities,
    pub returned_model: ReturnedModelPolicy,
    pub egress: EgressRealm,
}

impl ProviderBinding {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.model.provider.is_empty() || self.model.model.is_empty() {
            return Err(ConfigError::EmptyModel);
        }
        self.capabilities.validate()?;
        self.egress.validate()?;
        if let ReturnedModelPolicy::ServerRoute { allowed_family } = &self.returned_model {
            if allowed_family.is_empty() || allowed_family.iter().any(String::is_empty) {
                return Err(ConfigError::EmptyReturnedModelFamily);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolConcurrency {
    Serial,
    ParallelSafeReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolRecoveryPolicy {
    NeverRepeat,
    RepeatAfterNotStartedOrNoMutation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolBinding {
    pub id: ToolBindingId,
    pub spec: ToolSpec,
    pub schema_digest: ContentDigest,
    pub implementation: SemanticCompatibilityId,
    pub concurrency: ToolConcurrency,
    pub recovery: ToolRecoveryPolicy,
    pub egress: EgressRealm,
}

impl ToolBinding {
    pub fn new(
        id: ToolBindingId,
        spec: ToolSpec,
        implementation: SemanticCompatibilityId,
        concurrency: ToolConcurrency,
        recovery: ToolRecoveryPolicy,
        egress: EgressRealm,
    ) -> Result<Self, ConfigError> {
        let schema_digest =
            ContentDigest::of(&spec.input_schema).map_err(ConfigError::Serialization)?;
        let binding = Self {
            id,
            spec,
            schema_digest,
            implementation,
            concurrency,
            recovery,
            egress,
        };
        binding.validate()?;
        Ok(binding)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.spec.name.is_empty() {
            return Err(ConfigError::EmptyToolName);
        }
        self.egress.validate()?;
        let actual = ContentDigest::of(&self.spec.input_schema).map_err(ConfigError::Serialization)?;
        if actual != self.schema_digest {
            return Err(ConfigError::ToolSchemaDigestMismatch {
                binding: self.id.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    pub id: String,
    pub canonical_root: String,
    pub backend: String,
    pub object_identity: String,
}

impl WorkspaceBinding {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_id("workspace binding", &self.id)?;
        if self.canonical_root.is_empty() {
            return Err(ConfigError::EmptyIdentity {
                field: "workspace canonical root",
            });
        }
        validate_id("workspace backend", &self.backend)?;
        validate_id("workspace object identity", &self.object_identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityCeiling {
    pub workspace_mutation: bool,
    pub unconfined_execution: bool,
    pub remote_tools: bool,
    pub egress_realms: Vec<EgressRealm>,
}

impl AuthorityCeiling {
    fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = BTreeSet::new();
        for realm in &self.egress_realms {
            realm.validate()?;
            let encoded = serde_json::to_string(realm).map_err(ConfigError::Serialization)?;
            if !seen.insert(encoded) {
                return Err(ConfigError::DuplicateEgressRealm);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn permits(&self, realm: &EgressRealm) -> bool {
        self.egress_realms.contains(realm)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPolicy {
    pub max_request_bytes: u32,
    pub max_input_tokens: u32,
    pub max_checkpoint_bytes: u32,
    pub max_tail_bytes: u32,
}

impl ContextPolicy {
    fn validate(&self) -> Result<(), ConfigError> {
        for (setting, value) in [
            ("context.max_request_bytes", self.max_request_bytes),
            ("context.max_input_tokens", self.max_input_tokens),
            ("context.max_checkpoint_bytes", self.max_checkpoint_bytes),
            ("context.max_tail_bytes", self.max_tail_bytes),
        ] {
            if value == 0 {
                return Err(ConfigError::NonPositiveSetting { setting });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLimits {
    pub max_model_steps: u32,
    pub max_model_attempts_per_step: u32,
    pub max_tool_invocations: u32,
    pub max_parallel_read_tools: u32,
    pub max_response_bytes: u32,
    pub max_tool_preview_bytes: u32,
    pub max_cost_microusd: Option<u64>,
}

impl TurnLimits {
    fn validate(&self) -> Result<(), ConfigError> {
        for (setting, value) in [
            ("limits.max_model_steps", u64::from(self.max_model_steps)),
            (
                "limits.max_model_attempts_per_step",
                u64::from(self.max_model_attempts_per_step),
            ),
            (
                "limits.max_tool_invocations",
                u64::from(self.max_tool_invocations),
            ),
            (
                "limits.max_parallel_read_tools",
                u64::from(self.max_parallel_read_tools),
            ),
            (
                "limits.max_response_bytes",
                u64::from(self.max_response_bytes),
            ),
            (
                "limits.max_tool_preview_bytes",
                u64::from(self.max_tool_preview_bytes),
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::NonPositiveSetting { setting });
            }
        }
        if self.max_cost_microusd == Some(0) {
            return Err(ConfigError::NonPositiveSetting {
                setting: "limits.max_cost_microusd",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlCeiling {
    pub max_output_tokens: u32,
    pub sampling: bool,
    pub parallel_tool_calls: bool,
    pub allowed_reasoning: Vec<Reasoning>,
}

impl ControlCeiling {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_output_tokens == 0 {
            return Err(ConfigError::NonPositiveSetting {
                setting: "controls.max_output_tokens",
            });
        }
        if self.allowed_reasoning.is_empty() {
            return Err(ConfigError::NoReasoningMode);
        }
        Ok(())
    }

    fn permits(&self, controls: &GenerationControls) -> Result<(), ConfigError> {
        controls.validate().map_err(ConfigError::Controls)?;
        if controls.max_output_tokens > self.max_output_tokens {
            return Err(ConfigError::ControlsOutsideCeiling(
                "max_output_tokens exceeds the turn ceiling".to_owned(),
            ));
        }
        if !self.sampling && (controls.temperature.is_some() || controls.top_p.is_some()) {
            return Err(ConfigError::ControlsOutsideCeiling(
                "sampling controls are outside the turn ceiling".to_owned(),
            ));
        }
        if !self.parallel_tool_calls && controls.parallel_tool_calls {
            return Err(ConfigError::ControlsOutsideCeiling(
                "parallel tool calls are outside the turn ceiling".to_owned(),
            ));
        }
        if !self.allowed_reasoning.contains(&controls.reasoning) {
            return Err(ConfigError::ControlsOutsideCeiling(
                "reasoning mode is outside the turn ceiling".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationConfig {
    pub instructions: String,
    pub project_context: Vec<Message>,
    pub providers: Vec<ProviderBinding>,
    pub default_provider: ProviderBindingId,
    pub fallback_route: Vec<ProviderBindingId>,
    pub compaction_route: Vec<ProviderBindingId>,
    pub tools: Vec<ToolBinding>,
    pub initial_tools: Vec<ToolBindingId>,
    pub controls: GenerationControls,
    pub control_ceiling: ControlCeiling,
    pub context: ContextPolicy,
    pub workspace: WorkspaceBinding,
    pub authority: AuthorityCeiling,
    pub limits: TurnLimits,
}

impl ConversationConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
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
                .any(|content| !matches!(content, ion_ai::Content::Text(_)))
            {
                return Err(ConfigError::ProjectContextNotText { message: index });
            }
        }

        if self.providers.is_empty() {
            return Err(ConfigError::NoProviders);
        }
        let mut providers = BTreeSet::new();
        for provider in &self.providers {
            provider.validate()?;
            if !providers.insert(provider.id.clone()) {
                return Err(ConfigError::DuplicateProviderBinding(
                    provider.id.as_str().to_owned(),
                ));
            }
            if !self.authority.permits(&provider.egress) {
                return Err(ConfigError::EgressOutsideCeiling);
            }
        }
        if !providers.contains(&self.default_provider) {
            return Err(ConfigError::UnknownProviderBinding(
                self.default_provider.as_str().to_owned(),
            ));
        }
        for provider in self
            .fallback_route
            .iter()
            .chain(self.compaction_route.iter())
        {
            if !providers.contains(provider) {
                return Err(ConfigError::UnknownProviderBinding(
                    provider.as_str().to_owned(),
                ));
            }
        }

        let mut tools = BTreeSet::new();
        for tool in &self.tools {
            tool.validate()?;
            if !tools.insert(tool.id.clone()) {
                return Err(ConfigError::DuplicateToolBinding(
                    tool.id.as_str().to_owned(),
                ));
            }
            if matches!(&tool.egress, EgressRealm::Remote(_)) && !self.authority.remote_tools {
                return Err(ConfigError::RemoteToolOutsideCeiling);
            }
            if !self.authority.permits(&tool.egress) {
                return Err(ConfigError::EgressOutsideCeiling);
            }
        }
        let mut active = BTreeSet::new();
        for tool in &self.initial_tools {
            if !tools.contains(tool) {
                return Err(ConfigError::UnknownToolBinding(tool.as_str().to_owned()));
            }
            if !active.insert(tool) {
                return Err(ConfigError::DuplicateActiveTool(
                    tool.as_str().to_owned(),
                ));
            }
        }

        self.authority.validate()?;
        self.workspace.validate()?;
        self.context.validate()?;
        self.limits.validate()?;
        self.control_ceiling.validate()?;
        self.control_ceiling.permits(&self.controls)?;

        let environment = TurnEnvironment::from_config(CommitSeq::new(1).expect("positive"), self);
        let settings = TurnSettings {
            revision: 0,
            provider: self.default_provider.clone(),
            controls: self.controls.clone(),
            active_tools: self.initial_tools.clone(),
        };
        settings.validate(&environment)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledConfig {
    pub revision: CommitSeq,
    pub config: ConversationConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnEnvironment {
    pub config_revision: CommitSeq,
    pub instructions: String,
    pub project_context: Vec<Message>,
    pub providers: Vec<ProviderBinding>,
    pub default_provider: ProviderBindingId,
    pub fallback_route: Vec<ProviderBindingId>,
    pub compaction_route: Vec<ProviderBindingId>,
    pub tools: Vec<ToolBinding>,
    pub control_ceiling: ControlCeiling,
    pub context: ContextPolicy,
    pub workspace: WorkspaceBinding,
    pub authority: AuthorityCeiling,
    pub limits: TurnLimits,
}

impl TurnEnvironment {
    #[must_use]
    pub fn from_config(revision: CommitSeq, config: &ConversationConfig) -> Self {
        Self {
            config_revision: revision,
            instructions: config.instructions.clone(),
            project_context: config.project_context.clone(),
            providers: config.providers.clone(),
            default_provider: config.default_provider.clone(),
            fallback_route: config.fallback_route.clone(),
            compaction_route: config.compaction_route.clone(),
            tools: config.tools.clone(),
            control_ceiling: config.control_ceiling.clone(),
            context: config.context.clone(),
            workspace: config.workspace.clone(),
            authority: config.authority.clone(),
            limits: config.limits.clone(),
        }
    }

    pub fn capture(installed: &InstalledConfig) -> Result<(Self, TurnSettings), ConfigError> {
        installed.config.validate()?;
        let environment = Self::from_config(installed.revision, &installed.config);
        let settings = TurnSettings {
            revision: 0,
            provider: installed.config.default_provider.clone(),
            controls: installed.config.controls.clone(),
            active_tools: installed.config.initial_tools.clone(),
        };
        settings.validate(&environment)?;
        Ok((environment, settings))
    }

    pub fn digest(&self) -> Result<ContentDigest, ConfigError> {
        ContentDigest::of(self).map_err(ConfigError::Serialization)
    }

    #[must_use]
    pub fn provider(&self, id: &ProviderBindingId) -> Option<&ProviderBinding> {
        self.providers.iter().find(|provider| &provider.id == id)
    }

    #[must_use]
    pub fn tool(&self, id: &ToolBindingId) -> Option<&ToolBinding> {
        self.tools.iter().find(|tool| &tool.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSettings {
    pub revision: u32,
    pub provider: ProviderBindingId,
    pub controls: GenerationControls,
    pub active_tools: Vec<ToolBindingId>,
}

impl TurnSettings {
    pub fn validate(&self, environment: &TurnEnvironment) -> Result<(), ConfigError> {
        let provider = environment
            .provider(&self.provider)
            .ok_or_else(|| ConfigError::UnknownProviderBinding(self.provider.as_str().to_owned()))?;

        environment.control_ceiling.permits(&self.controls)?;
        if self.controls.max_output_tokens > provider.capabilities.max_output_tokens {
            return Err(ConfigError::ProviderControlMismatch(
                "requested output cap exceeds provider capability".to_owned(),
            ));
        }
        if !provider.capabilities.reasoning
            && !matches!(self.controls.reasoning, Reasoning::ProviderDefault | Reasoning::Off)
        {
            return Err(ConfigError::ProviderControlMismatch(
                "provider does not support requested reasoning".to_owned(),
            ));
        }

        let mut active = BTreeSet::new();
        for id in &self.active_tools {
            let binding = environment
                .tool(id)
                .ok_or_else(|| ConfigError::UnknownToolBinding(id.as_str().to_owned()))?;
            if !active.insert(id) {
                return Err(ConfigError::DuplicateActiveTool(id.as_str().to_owned()));
            }
            if !provider.capabilities.tools {
                return Err(ConfigError::ProviderControlMismatch(
                    "provider does not support tools".to_owned(),
                ));
            }
            if !environment.authority.permits(&binding.egress) {
                return Err(ConfigError::EgressOutsideCeiling);
            }
        }

        if let ToolChoice::Named(name) = &self.controls.tool_choice {
            let offered = self.active_tools.iter().any(|id| {
                environment
                    .tool(id)
                    .is_some_and(|binding| binding.spec.name == *name)
            });
            if !offered {
                return Err(ConfigError::NamedToolNotActive(name.clone()));
            }
        }
        if self.controls.parallel_tool_calls && !provider.capabilities.parallel_tool_calls {
            return Err(ConfigError::ProviderControlMismatch(
                "provider does not support parallel tool calls".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{field} must not be empty")]
    EmptyIdentity { field: &'static str },
    #[error("{field} is {length} bytes; maximum is {maximum}")]
    IdentityTooLong {
        field: &'static str,
        length: usize,
        maximum: usize,
    },
    #[error("provider binding must name both provider and model")]
    EmptyModel,
    #[error("a server-routed provider must name at least one returned-model family")]
    EmptyReturnedModelFamily,
    #[error("no provider bindings were configured")]
    NoProviders,
    #[error("provider binding {0:?} is unknown")]
    UnknownProviderBinding(String),
    #[error("tool binding {0:?} is unknown")]
    UnknownToolBinding(String),
    #[error("provider binding {0:?} is duplicated")]
    DuplicateProviderBinding(String),
    #[error("tool binding {0:?} is duplicated")]
    DuplicateToolBinding(String),
    #[error("active tool binding {0:?} is duplicated")]
    DuplicateActiveTool(String),
    #[error("tool declaration name must not be empty")]
    EmptyToolName,
    #[error("tool binding {binding:?} schema digest does not match its declaration")]
    ToolSchemaDigestMismatch { binding: String },
    #[error("project context message {message} is not a user message")]
    ProjectContextNotUser { message: usize },
    #[error("project context message {message} carries provider replay")]
    ProjectContextReplay { message: usize },
    #[error("project context message {message} is not text")]
    ProjectContextNotText { message: usize },
    #[error("{setting} must be positive")]
    NonPositiveSetting { setting: &'static str },
    #[error("the control ceiling must allow at least one reasoning mode")]
    NoReasoningMode,
    #[error("{0}")]
    Controls(ion_ai::ProviderError),
    #[error("generation controls exceed the turn ceiling: {0}")]
    ControlsOutsideCeiling(String),
    #[error("generation controls are incompatible with the selected provider: {0}")]
    ProviderControlMismatch(String),
    #[error("named tool {0:?} is not active")]
    NamedToolNotActive(String),
    #[error("egress realm is duplicated")]
    DuplicateEgressRealm,
    #[error("provider/tool egress lies outside the authority ceiling")]
    EgressOutsideCeiling,
    #[error("a remote tool is outside the authority ceiling")]
    RemoteToolOutsideCeiling,
    #[error("cannot encode durable semantic content: {0}")]
    Serialization(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Reasoning, ToolChoice};

    fn provider() -> ProviderBinding {
        ProviderBinding {
            id: ProviderBindingId::new("scripted").expect("id"),
            model: ModelRef {
                provider: "scripted".to_owned(),
                model: "test".to_owned(),
            },
            adapter: SemanticCompatibilityId::new("scripted-adapter-v1").expect("id"),
            request_encoding: SemanticCompatibilityId::new("scripted-request-v1").expect("id"),
            replay_family: None,
            capabilities: ProviderCapabilities {
                max_input_tokens: 100_000,
                max_output_tokens: 8_192,
                tools: true,
                parallel_tool_calls: true,
                structured_output: true,
                replay: false,
                reasoning: true,
            },
            returned_model: ReturnedModelPolicy::Exact,
            egress: EgressRealm::Local,
        }
    }

    fn tool() -> ToolBinding {
        ToolBinding::new(
            ToolBindingId::new("read").expect("id"),
            ToolSpec {
                name: "read".to_owned(),
                description: "read".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
            },
            SemanticCompatibilityId::new("read-v1").expect("id"),
            ToolConcurrency::ParallelSafeReadOnly,
            ToolRecoveryPolicy::RepeatAfterNotStartedOrNoMutation,
            EgressRealm::Local,
        )
        .expect("tool")
    }

    fn config() -> ConversationConfig {
        ConversationConfig {
            instructions: "be careful".to_owned(),
            project_context: Vec::new(),
            providers: vec![provider()],
            default_provider: ProviderBindingId::new("scripted").expect("id"),
            fallback_route: Vec::new(),
            compaction_route: Vec::new(),
            tools: vec![tool()],
            initial_tools: vec![ToolBindingId::new("read").expect("id")],
            controls: GenerationControls {
                max_output_tokens: 4096,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ToolChoice::Auto,
                parallel_tool_calls: true,
            },
            control_ceiling: ControlCeiling {
                max_output_tokens: 8192,
                sampling: false,
                parallel_tool_calls: true,
                allowed_reasoning: vec![
                    Reasoning::ProviderDefault,
                    Reasoning::Off,
                    Reasoning::Low,
                    Reasoning::Medium,
                    Reasoning::High,
                ],
            },
            context: ContextPolicy {
                max_request_bytes: 4 * 1024 * 1024,
                max_input_tokens: 100_000,
                max_checkpoint_bytes: 128 * 1024,
                max_tail_bytes: 1024 * 1024,
            },
            workspace: WorkspaceBinding {
                id: "workspace".to_owned(),
                canonical_root: "/tmp/project".to_owned(),
                backend: "local".to_owned(),
                object_identity: "dev:ino".to_owned(),
            },
            authority: AuthorityCeiling {
                workspace_mutation: true,
                unconfined_execution: false,
                remote_tools: false,
                egress_realms: vec![EgressRealm::Local],
            },
            limits: TurnLimits {
                max_model_steps: 32,
                max_model_attempts_per_step: 4,
                max_tool_invocations: 128,
                max_parallel_read_tools: 8,
                max_response_bytes: 1024 * 1024,
                max_tool_preview_bytes: 64 * 1024,
                max_cost_microusd: None,
            },
        }
    }

    #[test]
    fn capture_freezes_config_and_initial_settings() {
        let installed = InstalledConfig {
            revision: CommitSeq::new(7).expect("revision"),
            config: config(),
        };
        let (environment, settings) = TurnEnvironment::capture(&installed).expect("capture");
        assert_eq!(environment.config_revision.get(), 7);
        assert_eq!(settings.provider.as_str(), "scripted");
        assert_eq!(settings.active_tools[0].as_str(), "read");
        assert!(settings.validate(&environment).is_ok());
        assert_ne!(environment.digest().expect("digest").to_string(), "");
    }

    #[test]
    fn a_changed_schema_cannot_keep_the_old_digest() {
        let mut binding = tool();
        binding.spec.input_schema = serde_json::json!({"type":"object","required":["path"]});
        let mut cfg = config();
        cfg.tools = vec![binding];
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::ToolSchemaDigestMismatch { .. })
        ));
    }

    #[test]
    fn settings_cannot_introduce_a_new_binding() {
        let installed = InstalledConfig {
            revision: CommitSeq::new(1).expect("revision"),
            config: config(),
        };
        let (environment, mut settings) = TurnEnvironment::capture(&installed).expect("capture");
        settings.active_tools = vec![ToolBindingId::new("new-tool").expect("id")];
        assert!(matches!(
            settings.validate(&environment),
            Err(ConfigError::UnknownToolBinding(_))
        ));
    }
}
