//! Shared local host setup for Ion clients and Rust embedders.
//! Session/Turn semantics live in `ion-core`; concrete provider/tool implementations live here.

mod application;
#[cfg(test)]
mod application_tests;
pub mod auth;
mod background;
mod binding;
pub mod catalog;
pub mod code_mode;
mod command_args;
mod command_environment;
mod completion;
pub use completion::CompletionTarget;
pub use ion_core::CompletionDelivery;
mod control;
mod credentials;
pub use background::{
    BackgroundDestination, BackgroundJobStatus, BackgroundOwner, BackgroundRunFailure,
    BackgroundRunSnapshot, BackgroundSnapshot, BackgroundSummary, BackgroundWait,
};
mod delegation;
mod diagnostics;
pub use diagnostics::{ConfigurationCheck, ConfigurationStatus};
mod edit_diff;
pub mod file_io;
pub mod image_input;
mod local_tools;
pub mod mcp;
mod model_http;
pub mod model_setup;
mod preparation;
mod process;
mod project_instructions;
pub mod prompt_files;
mod question;
mod question_tool;
pub mod resources;
mod search_tools;
pub mod session_catalog;
mod tool_veto;
mod user_commands;
pub mod user_settings;
mod web_tools;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_core::{AgentLimits, CodingAgent, PromptCacheWarmingPolicy, ToolSet};

pub use application::{
    Application, ApplicationOwner, ApplicationRuntime, CodingApplication, PreparedShell, UserShell,
};
pub use auth::{CredentialStatus, CredentialStore};
pub use binding::SessionSetup;
pub use command_args::parse_args as parse_command_args;
pub use command_environment::CommandEnvironment;
pub use control::{
    AcceptedMetadata, ControlObserver, ControlSnapshot, ControlState, ControlUpdate, Delivery,
    InputAcceptance, InputCustodyPhase, InputId, InputPhase, InputSnapshot, McpReconnectFinished,
    McpReconnectSnapshot, McpReconnectState, Observation, OperationId, OperationKind,
    OperationOutcome, OperationState, OutcomeSnapshot, RecoveryAdmission, RejectedBatch,
    RejectedInput, RequestAdmission, ReturnReason, ReturnSnapshot, ReturnedInput, SessionControl,
    Settlement, ShutdownReport, SnapshotCursor, SteeringReceipt, SteeringStatus, TurnSteering,
    UpdateSnapshot, UserInput, preflight_messages,
};
pub use credentials::{CredentialResolutionError, CredentialResolver};
pub use delegation::{
    ChildActivity, ChildActivitySnapshot, ChildProgress, ChildProgressSnapshot, ChildSteering,
    ChildSteeringReport, DelegationPreparation, DelegationPreparationError, PreparedDelegation,
    SessionDelegation,
};
pub use local_tools::LocalTools;
pub use mcp::{
    McpConfig, McpExposure, McpHttpServer, McpReconnect, McpReconnectReport, McpServer,
    McpServerSnapshot, McpServerStatus, McpStartup, McpStdioServer, McpToolCounts, McpTools,
    McpValue,
};
pub use model_http::{HttpModelService, HttpWire};
pub use model_setup::{
    ModelChoice, ModelCycleDirection, ModelSelectionRequired, ModelStore, SavedSelection,
    Selection, Wire,
};
pub use process::{
    AttachmentPage, AttachmentScreen, AttachmentWrite, ProcessOwner, ProcessTools,
    TerminalAttachment, TerminalSummary,
};
pub use question::{
    MAX_ANSWER_BYTES, MAX_OPTION_DESCRIPTION_BYTES, MAX_OPTION_LABEL_BYTES,
    MAX_OPTION_PREVIEW_BYTES, MAX_OPTIONS, MAX_QUESTION_BYTES, MAX_QUESTION_HEADER_CHARS,
    MAX_QUESTIONS, MAX_REQUEST_BYTES, PendingQuestion, Question, QuestionAnswer, QuestionCustody,
    QuestionError, QuestionOption, QuestionRequestId, QuestionResponder, SYNTHETIC_NEXT_LABEL,
    SYNTHETIC_TEXT_LABEL,
};
pub use question_tool::QuestionTools;
pub use resources::{
    PromptTemplate, ResourceDiagnostic, ResourcePaths, ResourceSelection, Resources, Skill,
};
pub use search_tools::{SearchBackend, SearchFallback, SearchRoute, SearchSettings, SearchTools};
pub use session_catalog::{SessionCatalog, SessionSummary};
pub use tool_veto::{ToolVeto, ToolVetoSettings};
pub use user_commands::{CommandCatalog, CommandConfig, CommandOrigin, UserCommand};
pub use web_tools::WebTools;

/// Host configuration can be supplied explicitly by an embedding program or
/// resolved from `ION_HOME` (default `~/.ion`) for the executable.
pub struct Host {
    config_root: PathBuf,
    state_root: PathBuf,
    credentials: CredentialStore,
    models: ModelStore,
    resource_selection: ResourceSelection,
}

impl Host {
    pub fn new(config_root: PathBuf, state_root: PathBuf) -> Self {
        let credentials = CredentialStore::new(config_root.join("credentials"));
        let models = ModelStore::new(config_root.clone());
        Self {
            config_root,
            state_root,
            credentials,
            models,
            resource_selection: ResourceSelection::default(),
        }
    }

    pub fn from_environment() -> Result<Self> {
        let home = user_home(|name| std::env::var_os(name))?;
        Ok(Self::new(home.clone(), home))
    }

    pub fn config_root(&self) -> &Path {
        &self.config_root
    }
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }
    pub fn credentials(&self) -> &CredentialStore {
        &self.credentials
    }
    pub fn models(&self) -> &ModelStore {
        &self.models
    }

    pub fn sessions(&self, cwd: Option<PathBuf>) -> SessionCatalog {
        SessionCatalog::new(self.state_root.join("sessions"), cwd)
    }

    /// Capture invocation-local resource selection for all coding bindings,
    /// reloads and coding children using this Host.
    pub fn with_resource_selection(mut self, selection: ResourceSelection) -> Self {
        self.resource_selection = selection;
        self
    }

    pub fn resources(&self, cwd: &Path) -> Result<Resources> {
        Resources::load(cwd, &self.config_root, &self.resource_selection)
    }

    /// User-level MCP configuration only. Callers that are loading a project
    /// configuration must use `mcp_config_for`, which enforces project trust.
    pub fn mcp_config(&self) -> McpConfig {
        McpConfig::new(&self.config_root)
    }

    pub fn mcp_config_for(&self, cwd: &Path) -> Result<McpConfig> {
        McpConfig::with_project(&self.config_root, cwd)
    }

    pub async fn external_tools(
        &self,
        cwd: &Path,
        stop: tokio_util::sync::CancellationToken,
    ) -> McpStartup {
        let config = match self.mcp_config_for(cwd) {
            Ok(config) => config,
            Err(error) => {
                return McpStartup {
                    diagnostics: vec![format!("{error:#}")],
                    ..McpStartup::default()
                };
            }
        };
        McpTools::connect(&config, cwd, stop).await
    }

    /// Bind a selected HTTP route to a complete caller-owned tool set. This
    /// adds no native tools, instructions, filesystem input or composition VM.
    pub fn agent(&self, selected: &Selection, tools: Arc<ToolSet>) -> Result<Arc<CodingAgent>> {
        selected.require_access(&self.credentials)?;
        let resolver = self
            .credentials
            .resolver(&selected.provider, selected.api_key_env.as_deref())?;
        let service = Arc::new(HttpModelService::new_with_capabilities(
            &selected.endpoint,
            selected.wire,
            resolver,
            selected.capabilities,
            selected.context_window_tokens,
        )?);
        Ok(Arc::new(
            CodingAgent::with_tool_set(service, tools, selected.identity()).with_limits(
                AgentLimits {
                    max_output_tokens: selected.max_output_tokens,
                    context_window_tokens: selected.context_window_tokens,
                    image_input: selected.image_input,
                    prompt_cache_warming: prompt_cache_warming(selected),
                    ..AgentLimits::default()
                },
            ),
        ))
    }
}

fn prompt_cache_warming(selected: &Selection) -> Option<PromptCacheWarmingPolicy> {
    let cache = selected.capabilities.prompt_cache;
    let catalog::PromptCacheLifetime::Fixed {
        default_seconds, ..
    } = cache.lifetime
    else {
        return None;
    };
    if cache.refresh != catalog::PromptCacheRefresh::ReplayOneToken {
        return None;
    }
    let pricing = cache.pricing?;
    Some(PromptCacheWarmingPolicy {
        lifetime_seconds: u64::from(default_seconds),
        cache_write_microusd_per_million: pricing.write_5m_microusd_per_million,
        cache_read_microusd_per_million: pricing.read_microusd_per_million,
        output_microusd_per_million: pricing.output_microusd_per_million,
        minimum_savings_microusd: 50_000,
    })
}

fn user_home(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf> {
    let path = if let Some(path) = get("ION_HOME") {
        ensure!(!path.is_empty(), "ION_HOME must not be empty");
        PathBuf::from(path)
    } else {
        let home = get("HOME").context("HOME or ION_HOME is required for Ion paths")?;
        ensure!(!home.is_empty(), "HOME must not be empty");
        PathBuf::from(home).join(".ion")
    };
    std::path::absolute(path).context("cannot resolve Ion home")
}

// A not-yet-created final directory still has an identity through its parent.
// This prevents a user's ~/.ion from also becoming a project configuration
// when cwd is HOME, without hiding subsequent source read/validation errors.
pub(crate) fn same_directory(left: &Path, right: &Path) -> bool {
    let identity = |path: &Path| {
        path.canonicalize()
            .ok()
            .or_else(|| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?)))
    };
    left == right
        || identity(left)
            .zip(identity(right))
            .is_some_and(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ModelRef;
    use ion_core::CodingSession;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_util::sync::CancellationToken;

    #[test]
    fn user_home_has_one_explicit_override_and_no_xdg_fallback() {
        let resolve = |pairs: &[(&str, &str)]| {
            user_home(|name| {
                assert!(matches!(name, "ION_HOME" | "HOME"));
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).into())
            })
        };
        assert_eq!(
            resolve(&[("HOME", "/user")]).unwrap(),
            PathBuf::from("/user/.ion")
        );
        assert_eq!(
            resolve(&[("ION_HOME", "/portable")]).unwrap(),
            PathBuf::from("/portable")
        );
        assert_eq!(
            resolve(&[("ION_HOME", "profile"), ("HOME", "/user")]).unwrap(),
            std::path::absolute("profile").unwrap()
        );
        for pairs in [
            &[][..],
            &[("ION_HOME", ""), ("HOME", "/user")],
            &[("HOME", "")],
        ] {
            assert!(resolve(pairs).is_err());
        }
    }

    #[test]
    fn user_home_is_not_also_an_untrusted_project_layer() {
        use std::{fs, os::unix::fs::symlink};
        let root = std::env::temp_dir().join(format!("ion-home-layers-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("home/.git")).unwrap();
        symlink(root.join("home"), root.join("alias")).unwrap();
        let home = root.join("home");
        let config = root.join("alias/.ion");
        // Also deduplicate before the user directory has been created.
        assert!(
            McpConfig::with_project(&config, &home)
                .unwrap()
                .project_path()
                .is_none()
        );
        assert!(
            CommandConfig::with_project(&config, &home)
                .unwrap()
                .project_path()
                .is_none()
        );
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("commands.json"), r#"{"commands":{"check":{"description":"user command","program":"echo","args":[],"share_with_model":false}}}"#).unwrap();
        McpConfig::new(&config)
            .add(
                "user",
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: None,
                    cwd: None,
                    env: Default::default(),
                    command: "unused".into(),
                    args: Vec::new(),
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
        let mcp = McpConfig::with_project(&config, &home)
            .unwrap()
            .list()
            .unwrap();
        assert_eq!(mcp.servers.len(), 1);
        assert!(mcp.diagnostics.is_empty());
        let commands = CommandCatalog::load(&home, &config).unwrap();
        assert_eq!(commands.iter().count(), 1);
        assert_eq!(commands.iter().next().unwrap().origin, CommandOrigin::User);
        assert!(commands.diagnostics().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn selected_agent_keeps_identity_transport_and_limits_together() {
        let root = std::env::temp_dir().join(format!("ion-bound-model-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body_start = loop {
                assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
                assert!(bytes.len() < 64 * 1024);
                if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < body_start + length {
                assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
            let reply = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
            body
        });
        let host = Host::new(root.join("config"), root.join("state"));
        let route = SavedSelection {
            provider: format!("binding-probe-{}", uuid::Uuid::now_v7()),
            model: "selected-model".into(),
            endpoint: Some(endpoint),
            wire: Some(Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: Some(50_000),
            max_output_tokens: Some(1234),
            image_input: false,
        };
        let identity = host.models().save_default(&route).unwrap().identity();
        let selected = host.models().resolve_identity(&identity).unwrap();
        let agent = host.agent(&selected, Arc::new(ToolSet::new([]))).unwrap();
        host.models()
            .save_default(&SavedSelection {
                model: "new-default".into(),
                ..route
            })
            .unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        session
            .select_model(ModelRef {
                provider: "previous".into(),
                model: "previous-model".into(),
            })
            .unwrap();
        assert_eq!(agent.model(), &selected.identity());
        assert_eq!(agent.limits().max_output_tokens, 1234);
        assert_eq!(agent.limits().context_window_tokens, Some(50_000));
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "hello".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {}
                )
                .await
                .unwrap(),
            "answer"
        );
        let body = server.await.unwrap();
        assert_eq!(body["model"], "selected-model");
        assert_eq!(body["max_completion_tokens"], 1234);
        let view = session.view().unwrap();
        assert_eq!(view.last_model, Some(selected.identity()));
        assert_eq!(view.last_effective_model, Some(selected.identity()));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}
