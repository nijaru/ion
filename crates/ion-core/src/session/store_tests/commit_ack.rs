//! Exercise the real keyed admission boundary with SQLite's WAL commit hook.
use super::*;
use crate::{Agent, AgentError};
use ion_ai::{ModelResponse, ModelStreamEvent, Script, ScriptedModelService};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(5);

async fn submit(
    agent: &Agent,
    session: Arc<Session>,
    request: SubmissionRequest,
    observe: impl FnMut(crate::AgentEvent) + Send + 'static,
) -> Result<crate::RequestSubmission, AgentError> {
    agent
        .submit_request::<_, ()>(
            session,
            request,
            String::new(),
            CancellationToken::new(),
            None,
            observe,
        )
        .await
}

#[tokio::test(flavor = "current_thread")]
async fn no_live_admission_or_dispatch_until_acknowledged_even_if_the_wal_is_committed() {
    for (lose_ack, stop_while_pending) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new();
        let session = Arc::new(fixture.session());
        let key = RequestKey::new("commit-fixture".into(), "once".into()).unwrap();
        let request = SubmissionRequest {
            key: key.clone(),
            input: Message::user_input("run once".into(), []),
        };
        let provider = Arc::new(ScriptedModelService::new([Script::Stream(vec![
            ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: None,
            }),
        ])]));
        let agent = Agent::with_tool_set(
            provider.clone(),
            Arc::new(crate::ToolSet::new([])),
            model("fixture"),
        );
        let history = session.history().unwrap();
        let (entered, committed) = tokio::sync::oneshot::channel();
        let (release, held) = mpsc::channel();
        let mut entered = Some(entered);
        session
            .store()
            .lock()
            .unwrap()
            .connection
            .wal_hook(Some(
                move |_: &mut rusqlite::hooks::Wal, _: std::ffi::c_int| {
                    // SQLite invokes this after committing to WAL and releasing
                    // its write lock, but before COMMIT returns to the Rust caller.
                    if let Some(entered) = entered.take() {
                        let _ = entered.send(());
                        if held.recv_timeout(TIMEOUT).is_err() || lose_ack {
                            return Err(rusqlite::Error::SqliteFailure(
                                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                                Some("commit acknowledgment lost".into()),
                            ));
                        }
                    }
                    Ok(())
                },
            ))
            .unwrap();

        let accepted_event = Arc::new(AtomicBool::new(false));
        let waiting = tokio::spawn({
            let agent = agent.clone();
            let session = session.clone();
            let request = request.clone();
            let accepted_event = accepted_event.clone();
            async move {
                submit(&agent, session, request, move |event| {
                    if matches!(event, crate::AgentEvent::TurnAccepted { turn: 1, .. }) {
                        accepted_event.store(true, Ordering::SeqCst);
                    }
                })
                .await
            }
        });
        tokio::time::timeout(TIMEOUT, committed)
            .await
            .unwrap()
            .unwrap();
        // Passive inspection reads the real committed WAL with a separate
        // connection. It cannot publish this owner's tentative projection.
        let persisted = tokio::time::timeout(TIMEOUT, history.request_receipt(&key))
            .await
            .unwrap()
            .unwrap()
            .expect("WAL was already committed");
        assert_eq!(persisted.turn, 1);
        assert_eq!(persisted.end, None);
        assert!(!waiting.is_finished());
        assert!(!accepted_event.load(Ordering::SeqCst));
        assert!(provider.requests().is_empty());
        assert!(matches!(
            session.reserve_idle(),
            Err(SessionError::OperationActive)
        ));
        let mut shutdown = Box::pin(session.shutdown());
        if stop_while_pending {
            tokio::select! {
                biased;
                result = &mut shutdown => panic!("shutdown returned before admission finished: {result:?}"),
                () = std::future::ready(()) => {}
            }
        }
        release.send(()).unwrap();

        let result = tokio::time::timeout(TIMEOUT, waiting)
            .await
            .unwrap()
            .unwrap();
        if !lose_ack {
            let started = result.unwrap();
            assert_eq!(started.receipt, persisted);
            let result = tokio::time::timeout(TIMEOUT, started.operation.unwrap())
                .await
                .unwrap();
            assert!(accepted_event.load(Ordering::SeqCst));
            if stop_while_pending {
                assert!(matches!(result, Err(AgentError::Cancelled)));
                assert!(provider.requests().is_empty());
                assert_eq!(
                    history.request_receipt(&key).await.unwrap().unwrap().end,
                    Some(crate::TurnEndReason::Cancelled)
                );
            } else {
                assert_eq!(result.unwrap(), "done");
                assert_eq!(provider.requests().len(), 1);
            }
            tokio::time::timeout(TIMEOUT, &mut shutdown)
                .await
                .unwrap()
                .unwrap();
            continue;
        }

        assert!(matches!(result, Err(AgentError::Session(error)) if error.is_commit_uncertain()));
        assert!(!accepted_event.load(Ordering::SeqCst));
        assert!(provider.requests().is_empty());
        {
            let store = session.store().lock().unwrap();
            assert_eq!(store.state.sequence, 0, "commit was never acknowledged");
            assert_eq!(store.state.active, None);
        }
        assert_faulted(session.view());
        shutdown.await.unwrap();
        drop(session);

        let reopened = Arc::new(Session::open(fixture.path()).unwrap());
        let duplicate = submit(&agent, reopened.clone(), request, |_| {
            panic!("redelivery published activity")
        })
        .await
        .unwrap();
        assert_eq!(duplicate.receipt, persisted);
        assert!(duplicate.operation.is_none());
        assert_eq!(reopened.view().unwrap().entries.len(), 1);
        assert!(provider.requests().is_empty());
        reopened.shutdown().await.unwrap();
    }
}
