//! Credential-free, owner-retained service embedding. No network/auth framework.
//! Run: cargo run -p ion --example service -- STATE_DIRECTORY
use anyhow::{Context, Result, ensure};
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelRequest, ModelResponse, ModelService, ModelStream,
    ModelStreamEvent, ProviderError, ResponseTermination, Role, Script, ScriptedModelService,
    Usage,
};
use ion_core::{CodingAgent, CodingSession, ToolSet, TurnEndReason};
use ion_host::{
    Application, ApplicationRuntime, ControlObserver, ControlSnapshot, ControlUpdate, Delivery,
    Resources, SessionCatalog, SessionControl, SessionSetup, UpdateSnapshot, UserInput,
};
use serde::Serialize;
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// Deliberately not Clone/Sync/Deserialize. Snapshots project, never reconstruct it.
#[derive(Serialize)]
struct Metadata {
    correlation: String,
    marker: Cell<u8>,
}
fn metadata(correlation: &str) -> Metadata {
    Metadata {
        correlation: correlation.into(),
        marker: Cell::new(7),
    }
}
fn input(text: &str) -> UserInput {
    UserInput::Message(Message::user_input(text.into(), []))
}
fn identity() -> ModelRef {
    ModelRef {
        provider: "synthetic".into(),
        model: "service".into(),
    }
}

struct Model {
    calls: AtomicUsize,
    permits: Semaphore,
}
impl ModelService for Model {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.permits
                .acquire()
                .await
                .expect("fixture gate stays open")
                .forget();
            let text = "x".repeat(300);
            let mut events = vec![ModelStreamEvent::TextDelta("x".into()); 300];
            events.push(ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text(text)],
                    provider_replay: None,
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: None,
            }));
            ScriptedModelService::new([Script::Stream(events)])
                .stream(request)
                .await
        })
    }
}
struct App(Arc<Model>);
impl Application for App {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        ensure!(cwd.is_none(), "workspace-free application");
        Ok(Resources::new(
            "Synthetic service qualification, no tools.".into(),
        ))
    }
    fn runtime(
        &self,
        _cwd: Option<&Path>,
        selected: Option<&ModelRef>,
        _questions: Option<Arc<ion_host::QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        ensure!(
            selected.is_none_or(|model| model == &identity()),
            "unknown model"
        );
        Ok(ApplicationRuntime::new(Arc::new(
            CodingAgent::with_tool_set(self.0.clone(), Arc::new(ToolSet::new([])), identity()),
        )))
    }
}

// A grant supplied by the trusted embedding AFTER authenticating its caller.
// It is not accepted/deserialized from client payloads. This fixture tests scope
// enforcement, not network authentication. Readers get no control capability.
#[derive(Clone, Copy)]
struct ReadGrant {
    session: uuid::Uuid,
    allowed: bool,
}
struct Query {
    grant: ReadGrant,
    reply: oneshot::Sender<Reply>,
    credit: OwnedSemaphorePermit,
}
struct Reply {
    result: Result<(ControlSnapshot<String>, ControlObserver)>,
    _credit: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct Connection {
    grant: ReadGrant,
    requests: mpsc::Sender<Query>,
    credits: Arc<Semaphore>,
}
impl Connection {
    fn snapshot(&self) -> Result<oneshot::Receiver<Reply>> {
        // One full view may be queued, materializing or held by a stalled reader.
        // No task is spawned per query, and no deferred view/reply list exists.
        let credit = self.credits.clone().try_acquire_owned()?;
        let (reply, receive) = oneshot::channel();
        self.requests
            .try_send(Query {
                grant: self.grant,
                reply,
                credit,
            })
            .map_err(|_| anyhow::anyhow!("inspection queue unavailable"))?;
        Ok(receive)
    }
}

async fn serve(
    mut control: SessionControl<Metadata>,
    mut queries: mpsc::Receiver<Query>,
    stop: CancellationToken,
    shutdown: CancellationToken,
    finish: CancellationToken,
) -> SessionControl<Metadata> {
    loop {
        let state = control.state();
        tokio::select! {
            // Transport output is NEVER awaited here. Stop/closure are separate
            // supervisor capabilities, not queued behind inspection or publication.
            _ = shutdown.cancelled(), if !state.closed => control.request_shutdown(),
            _ = stop.cancelled(), if !state.stop_requested => control.request_stop(),
            _ = finish.cancelled(), if state.shutdown_complete => return control,
            Some(query) = queries.recv() => {
                let result = if query.grant.allowed && query.grant.session == control.session().id() {
                    // Subscribe before the read. Activity remains a lossy hint;
                    // callers replace snapshots, never splice it onto this cut.
                    control.attach(|m| m.correlation.clone())
                } else { Err(anyhow::anyhow!("inspection not authorized")) };
                let _ = query.reply.send(Reply { result, _credit: query.credit });
            }
            _ = async {
                if state.closed { control.join_shutdown().await; }
                else { control.maintain().await; }
            }, if state.busy || (state.closed && !state.shutdown_complete) => {}
        }
    }
}

async fn until(mut ready: impl FnMut() -> bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("fixture did not reach expected boundary")
}

async fn qualify(
    connection: &Connection,
    session: &CodingSession,
    model: &Model,
    stop: &CancellationToken,
    shutdown: &CancellationToken,
) -> Result<()> {
    let mut denied = connection.clone();
    denied.grant.allowed = false;
    ensure!(
        denied.snapshot()?.await?.result.is_err(),
        "unauthorized disclosure"
    );
    let mut early = connection.snapshot()?.await?;
    let (initial, observer) = early
        .result
        .as_mut()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let first = initial.cursor;
    ensure!(
        initial.state.busy && initial.inputs.len() == 3,
        "missing live custody"
    );
    ensure!(
        matches!(
            observer.next().await,
            Some(ion_host::Observation::Gap { skipped: 0 })
        ),
        "initial resync missing"
    );
    ensure!(
        connection.snapshot().is_err(),
        "stalled reader did not retain inspection credit"
    );
    model.permits.add_permits(1);
    // An unread reply and observer cannot prevent Host follow-up dispatch.
    until(|| model.calls.load(Ordering::SeqCst) == 2).await?;
    ensure!(
        session.view()?.last_end == Some((1, TurnEndReason::Completed)),
        "first Turn failed"
    );
    ensure!(
        matches!(observer.next().await, Some(ion_host::Observation::Gap { skipped }) if skipped > 0),
        "slow observation did not report its gap"
    );
    drop(early); // Detach all external observers while the second Turn is live.
    let reconnected = connection.snapshot()?.await?;
    let (live, _) = reconnected
        .result
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    ensure!(
        live.cursor.owner == first.owner
            && live.session.unfinished_turn == Some(2)
            && live.state.busy,
        "disconnect changed execution ownership"
    );
    stop.cancel();
    until(|| {
        session
            .view()
            .is_ok_and(|view| matches!(view.last_end, Some((2, TurnEndReason::Cancelled))))
    })
    .await?;
    drop(reconnected); // Read credit stayed occupied throughout stop/settlement.
    let late = loop {
        let reply = connection.snapshot()?.await?;
        if reply
            .result
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .0
            .state
            .busy
        {
            drop(reply);
            tokio::task::yield_now().await;
        } else {
            break reply;
        }
    };
    let (view, _) = late
        .result
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    ensure!(
        view.cursor.owner == first.owner && view.cursor.read > first.read,
        "reconnect ordering changed"
    );
    ensure!(
        view.cursor.committed as usize == view.session.entries.len(),
        "invalid committed cut"
    );
    ensure!(view.updates.iter().any(|u| matches!(u, UpdateSnapshot::Returned { input: UserInput::Message(message), metadata, .. } if message == &Message::user_input("literal $ third".into(), []) && metadata == "third")), "returned original missing");
    // Joined shutdown is also independent of blocked snapshot/transport output.
    shutdown.cancel();
    until(|| {
        matches!(
            session.reserve_idle(),
            Err(ion_core::CodingSessionError::Operation(
                ion_core::OperationError::Closed
            ))
        )
    })
    .await?;
    drop(late);
    // Shutdown completion, not just Core idle, is exposed by a replacement read.
    loop {
        let reply = connection.snapshot()?.await?;
        let (view, _) = reply
            .result
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        if view.state.shutdown_complete {
            ensure!(
                view.state.closed && !view.state.busy && view.cleanup_errors.is_empty(),
                "incomplete shutdown"
            );
            ensure!(
                view.updates
                    .iter()
                    .filter(|u| matches!(u, UpdateSnapshot::Settled { .. }))
                    .count()
                    == 2,
                "settlements not retained"
            );
            break;
        }
        drop(reply);
        tokio::task::yield_now().await;
    }
    ensure!(
        model.calls.load(Ordering::SeqCst) == 2,
        "stopped follow-up dispatched"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("usage: service STATE_DIRECTORY")?,
    );
    let model = Arc::new(Model {
        calls: AtomicUsize::new(0),
        permits: Semaphore::new(0),
    });
    let app = Arc::new(App(model.clone()));
    let catalog = SessionCatalog::new(root.clone(), None);
    let path = catalog.new_path()?;
    let session = Arc::new(CodingSession::create(&path, None)?);
    let mut control =
        SessionSetup::new(session.clone(), catalog, app.clone(), None, None, None)?.into_control();
    control
        .submit(Delivery::Start, input("first"), metadata("first"))
        .map_err(|e| e.reason)?;
    control
        .submit(Delivery::FollowUp, input("second"), metadata("second"))
        .map_err(|e| e.reason)?;
    control
        .submit(
            Delivery::FollowUp,
            input("literal $ third"),
            metadata("third"),
        )
        .map_err(|e| e.reason)?;
    let (requests, queries) = mpsc::channel(1);
    let connection = Connection {
        grant: ReadGrant {
            session: session.id(),
            allowed: true,
        },
        requests,
        credits: Arc::new(Semaphore::new(1)),
    };
    let stop = CancellationToken::new();
    let shutdown = CancellationToken::new();
    let finish = CancellationToken::new();
    let task = tokio::spawn(serve(
        control,
        queries,
        stop.clone(),
        shutdown.clone(),
        finish.clone(),
    ));
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        qualify(&connection, &session, &model, &stop, &shutdown),
    )
    .await;
    // Even a failed/timed-out qualification explicitly stops and joins its owner.
    shutdown.cancel();
    finish.cancel();
    let mut control = task.await?;
    let before = control.snapshot(|_| ())?.cursor;
    let report = control.shutdown().await;
    result.context("service qualification timed out")??;
    ensure!(report.cleanup_errors.is_empty(), "cleanup failed");
    ensure!(report.updates.iter().any(|u| matches!(u, ControlUpdate::Returned(input) if input.metadata.correlation == "third" && input.metadata.marker.get() == 7)), "original metadata lost");
    drop(control);
    drop(session);
    let reopened = Arc::new(CodingSession::open(path)?);
    let mut replacement = SessionSetup::new(
        reopened,
        SessionCatalog::new(root, None),
        app,
        None,
        None,
        None,
    )?
    .into_control::<Metadata>();
    let after = replacement.snapshot(|_| ())?.cursor;
    ensure!(
        after.session == before.session
            && after.owner != before.owner
            && after.committed == before.committed,
        "new owner reused an attachment epoch"
    );
    replacement.shutdown().await;
    println!("service attachment smoke passed");
    Ok(())
}
