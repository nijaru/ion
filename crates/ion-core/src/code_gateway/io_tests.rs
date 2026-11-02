use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ion_ai::{Content, Message, ModelRef, Role, ToolSpec, Usage};
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::{SessionEntry, ToolDefinition, ToolExecutor, ToolRegistration, ToolSet, ToolSource};

struct Native {
    calls: AtomicUsize,
    entered: Notify,
    finish: Notify,
}

impl ToolSource for Native {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "native".into(),
                description: "fixture".into(),
                input_schema: json!({"type":"object"}),
            }),
            self,
        )]
    }
}

impl ToolExecutor for Native {
    fn execute<'a>(&'a self, _: crate::ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.finish.notified().await;
            ToolOutput {
                value: json!({"raw":"retained"}),
                images: vec![],
                artifacts: vec![],
                is_error: false,
            }
        })
    }
}

struct Guest {
    abandon_reply: bool,
}

impl CodeRuntime for Guest {
    fn start(
        &self,
        _: String,
        _: String,
        _: CodeLimits,
        _: CancellationToken,
    ) -> Result<CodeTask, String> {
        let (sender, requests) = mpsc::channel(1);
        let abandon_reply = self.abandon_reply;
        let result = tokio::spawn(async move {
            let (reply, receive) = oneshot::channel();
            sender
                .send(CodeRequest {
                    kind: CodeRequestKind::Call {
                        name: "native".into(),
                        args_json: "{}".into(),
                    },
                    reply,
                })
                .await
                .unwrap();
            let result = if abandon_reply {
                drop(receive);
                Ok("null".into())
            } else {
                receive.await.unwrap()
            };
            CodeResult {
                result,
                output: vec![],
            }
        });
        Ok(CodeTask {
            requests,
            result: Box::pin(async move { result.await.map_err(|error| error.to_string()) }),
        })
    }
}

struct Fixture {
    root: std::path::PathBuf,
    session: Arc<Session>,
    turn: u64,
    call: ToolCall,
    parent: ToolOccurrence,
    native: Arc<Native>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-code-io-{}", uuid::Uuid::new_v4()));
        let session = Arc::new(Session::create(root.join("session.sqlite"), None).unwrap());
        let (turn, _) = session
            .begin_turn(
                "code".into(),
                ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
            )
            .unwrap();
        let call = ToolCall {
            id: "code".into(),
            name: "code_mode".into(),
            arguments: json!({"code":"fixture"}),
            raw_arguments: None,
        };
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(call.clone())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let parent = session.tool_occurrence(turn, &call.id).unwrap();
        Self {
            root,
            session,
            turn,
            call,
            parent,
            native: Arc::new(Native {
                calls: AtomicUsize::new(0),
                entered: Notify::new(),
                finish: Notify::new(),
            }),
        }
    }

    fn run(
        &self,
        abandon_reply: bool,
        stop: CancellationToken,
        finished: Arc<AtomicUsize>,
    ) -> tokio::task::JoinHandle<Result<ToolOutput, AgentError>> {
        let session = self.session.clone();
        let turn = self.turn;
        let parent = self.parent;
        let call = self.call.clone();
        let catalog = ToolSet::new([self.native.clone() as Arc<dyn ToolSource>]).snapshot();
        tokio::spawn(async move {
            let guest: Arc<dyn CodeRuntime> = Arc::new(Guest { abandon_reply });
            run(
                &guest,
                CodeLimits::default(),
                &session,
                turn,
                &catalog,
                &call,
                parent,
                Reasoning::ProviderDefault,
                &[],
                &stop,
                &mut |event| {
                    if matches!(event, AgentEvent::ChildToolFinished { .. }) {
                        finished.fetch_add(1, Ordering::SeqCst);
                    }
                },
            )
            .await
        })
    }

    fn writer(&self) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open(self.session.path()).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection
    }

    fn cleanup(self) {
        drop(self.session);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_intent_keeps_executor_live_and_stop_prevents_dispatch() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let stop = CancellationToken::new();
    let finished = Arc::new(AtomicUsize::new(0));
    let joined = fixture.run(false, stop.clone(), finished.clone());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !joined.is_finished(),
        "intent must wait for the actual writer"
    );
    assert_eq!(fixture.native.calls.load(Ordering::SeqCst), 0);
    stop.cancel();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    let output = joined.await.unwrap().unwrap();
    assert_eq!(output.value["calls"], 1);
    assert_eq!(output.value["skipped_calls"], 1);
    assert_eq!(fixture.native.calls.load(Ordering::SeqCst), 0);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionEntry::ChildToolResult {
                    outcome: ChildOutcome::NotDispatched { .. },
                    ..
                }
            ))
    );
    fixture.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_guest_keeps_raw_observation_through_delayed_commit_and_stop() {
    let fixture = Fixture::new();
    let stop = CancellationToken::new();
    let finished = Arc::new(AtomicUsize::new(0));
    let joined = fixture.run(true, stop.clone(), finished.clone());
    fixture.native.entered.notified().await;
    let writer = fixture.writer();
    fixture.native.finish.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !joined.is_finished(),
        "guest departure must not release settlement"
    );
    assert_eq!(
        finished.load(Ordering::SeqCst),
        0,
        "publication must follow commit"
    );
    stop.cancel();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    joined.await.unwrap().unwrap();
    assert_eq!(fixture.native.calls.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry,
                SessionEntry::ChildToolResult { outcome: ChildOutcome::Observed { output }, .. }
                    if output.value == json!({"raw":"retained"})
            ))
    );
    fixture.cleanup();
}
