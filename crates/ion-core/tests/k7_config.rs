//! R7: durable conversation configuration.
//!
//! Configuration is committed as one complete typed replacement and inspected
//! with the commit that installed it, so a client can replace it without
//! silently discarding a change it never read. Absence is a real answer: an
//! unconfigured conversation is inspectable, and nothing here invents a model
//! for it.
//!
//! The suite drives the writer, the store, the reopen path and the refusal
//! rules; assembly and dispatch are the next slice and are not claimed here.

use ion_ai::{Content, GenerationControls, Message, ModelRef, Reasoning, ToolChoice};
use ion_core::builtin::{Builtins, ToolCatalog, WORKER};
use ion_core::{
    ContextPolicy, ConversationConfig, ConversationId, DriveOutcome, RunLimits, Session,
    SessionError, TaskDriver, TaskKindName, TaskRegistry, TaskRequest,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn sample(model: &str) -> ConversationConfig {
    ConversationConfig {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: model.to_owned(),
        },
        instructions: "answer with the smallest correct change".to_owned(),
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
        project_context: vec![Message {
            role: ion_ai::Role::User,
            content: vec![Content::Text("the project rule".to_owned())],
            provider_replay: None,
        }],
        instruction_revision: "ion-instructions-v1".to_owned(),
        tool_names: vec!["read".to_owned()],
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
fn an_unconfigured_conversation_has_no_configuration() {
    let session = Session::new().expect("session");
    assert_eq!(
        session.conversation_config(session.root_conversation()),
        None
    );
}

#[test]
fn configuration_is_readable_with_the_commit_that_installed_it() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let config = sample("test-model");

    let commit = session
        .configure_conversation(root, None, config.clone())
        .expect("configure");
    let installed = session
        .conversation_config(root)
        .expect("a configuration is installed");

    assert_eq!(installed.revision, commit);
    assert_eq!(installed.config, config);
}

#[test]
fn the_installed_configuration_survives_a_reopen() {
    let db = TempDb::new("config-reopen");
    let commit = {
        let mut session = Session::create(db.path()).expect("create");
        let root = session.root_conversation();
        session
            .configure_conversation(root, None, sample("before-restart"))
            .expect("configure")
    };

    let session = Session::open(db.path()).expect("reopen");
    let root = session.root_conversation();
    let installed = session
        .conversation_config(root)
        .expect("the configuration is durable");
    assert_eq!(installed.revision, commit);
    assert_eq!(installed.config.model.model, "before-restart");
    assert_eq!(
        installed.config.project_context.len(),
        1,
        "resolved project context is stored, not reread"
    );
    assert_eq!(installed.config.instructions, sample("x").instructions);
}

#[test]
fn reconfiguration_is_fenced_by_the_revision_the_caller_read() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let first = session
        .configure_conversation(root, None, sample("first"))
        .expect("configure");

    // A caller that never saw the first configuration must not clobber it by
    // asserting that nothing is installed.
    let stale = session.configure_conversation(root, None, sample("clobber"));
    assert!(matches!(
        stale,
        Err(SessionError::StaleConfiguration {
            conversation,
            expected: None,
            current: Some(found),
        }) if conversation == root && found == first
    ));
    assert_eq!(
        session
            .conversation_config(root)
            .expect("still configured")
            .config
            .model
            .model,
        "first"
    );

    // A caller that read the second revision replaces it and learns the third.
    let second = session
        .configure_conversation(root, Some(first), sample("second"))
        .expect("replace");
    assert!(second > first);
    assert_eq!(
        session
            .conversation_config(root)
            .expect("replaced")
            .config
            .model
            .model,
        "second"
    );

    // The revision it just read is stale even though it is the newest in the
    // history: a second replacement needs the newest revision, not any old one.
    assert!(matches!(
        session.configure_conversation(root, Some(first), sample("third")),
        Err(SessionError::StaleConfiguration {
            expected: Some(_),
            current: Some(current),
            ..
        }) if current == second
    ));
}

#[test]
fn an_invalid_configuration_is_refused_without_touching_the_installed_one() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let first = session
        .configure_conversation(root, None, sample("good"))
        .expect("configure");

    let mut invalid = sample("bad");
    invalid.tool_names = vec!["read".to_owned(), "read".to_owned()];
    let refused = session.configure_conversation(root, Some(first), invalid);
    assert!(
        matches!(refused, Err(SessionError::InvalidConfiguration(ref message))
            if message.contains("read")),
        "a duplicated tool name must be refused by name: {refused:?}"
    );

    // Refusal is total: neither the content nor the revision moved, so the
    // configuration the caller read is still exactly what is installed.
    let installed = session
        .conversation_config(root)
        .expect("the previous configuration remains installed");
    assert_eq!(installed.revision, first);
    assert_eq!(installed.config.model.model, "good");
}

#[test]
fn a_configuration_the_caller_did_not_send_is_not_kept() {
    // A refused command must leave nothing behind: the journal rolls back the
    // writes it prepared, so the resident state is exactly as it was.
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let first = session
        .configure_conversation(root, None, sample("kept"))
        .expect("configure");

    let mut invalid = sample("discarded");
    invalid.context.compact_at_tokens = invalid.context.max_input_tokens + 1;
    assert!(
        session
            .configure_conversation(root, Some(first), invalid)
            .is_err()
    );

    let installed = session.conversation_config(root).expect("installed");
    assert_eq!(installed.config.model.model, "kept");
    assert_eq!(installed.revision, first);
}

#[test]
fn an_unknown_conversation_cannot_be_configured() {
    let mut session = Session::new().expect("session");
    let unknown = ConversationId::new(4_242).expect("conversation id");
    assert!(matches!(
        session.configure_conversation(unknown, None, sample("nowhere")),
        Err(SessionError::UnknownConversation(id)) if id == unknown
    ));
}

#[tokio::test]
async fn a_retired_conversation_refuses_configuration() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let spawn = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "do the thing"}),
            dependencies: Vec::new(),
        })
        .expect("spawn task");
    let driver = TaskDriver::new(session, builtins());
    assert!(matches!(
        driver.drive_task(spawn.task_id).await.expect("spawn"),
        DriveOutcome::Settled(_)
    ));
    let worker = driver
        .owned_conversations(spawn.task_id)
        .await
        .expect("spawn task")
        .pop()
        .expect("one owned worker");

    // A live worker can be configured before it is retired.
    driver
        .configure_conversation(worker, None, sample("before-retirement"))
        .await
        .expect("configure a live worker");

    driver
        .retire_conversation(worker)
        .await
        .expect("retire a quiescent worker");

    let refused = driver
        .configure_conversation(worker, None, sample("after-retirement"))
        .await;
    assert!(
        matches!(refused, Err(ion_core::TaskDriverError::Session(ref error))
            if matches!(error, SessionError::ConversationRetired(id) if *id == worker)),
        "a retired conversation accepts no new configuration: {refused:?}"
    );
    assert_eq!(
        driver
            .conversation_config(worker)
            .await
            .expect("the retired worker keeps its configuration")
            .config
            .model
            .model,
        "before-retirement"
    );
}

fn builtins() -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        service: std::sync::Arc::new(ion_ai::ScriptedModelService::new([ion_ai::Script::Stream(
            vec![ion_ai::ModelStreamEvent::Completed(ion_ai::ModelResponse {
                message: Message {
                    role: ion_ai::Role::Assistant,
                    content: vec![Content::Text("worker answer".to_owned())],
                    provider_replay: None,
                },
                usage: ion_ai::Usage::known(1, 1),
                termination: ion_ai::ResponseTermination::Completed,
            })],
        )])),
        tools: std::sync::Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    registry
}
