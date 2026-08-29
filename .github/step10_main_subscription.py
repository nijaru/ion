from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    assert text.count(old) == 1, f"{label} context changed"
    return text.replace(old, new, 1)


runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()

changes = [
    (
        "command variant",
        """    Subscribe {
        reply: oneshot::Sender<SubscribeReply>,
    },
    Close {
""",
        """    Subscribe {
        reply: oneshot::Sender<SubscribeReply>,
    },
    SubscribeAll {
        reply: oneshot::Sender<SubscribeReply>,
    },
    Close {
""",
    ),
    (
        "handle API",
        """    /// Snapshot plus bounded live events (DESIGN.md §21.2). A consumer
    /// that falls behind resynchronizes from a fresh snapshot; past
    /// events are never replayed.
    pub async fn subscribe(&self) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { reply })
            .map_err(command_send_error)?;
        let (snapshot, events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok((snapshot, events))
    }

    /// Close the session (DESIGN.md §9.5): lifecycle shutdown, never a
""",
        """    /// Main-lane snapshot plus main-lane bounded live events (DESIGN.md
    /// §16.1). A frontend that falls behind resynchronizes from a fresh
    /// snapshot; sibling-lane work cannot pollute or overflow this event ring.
    pub async fn subscribe(&self) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { reply })
            .map_err(command_send_error)?;
        let (snapshot, events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok((snapshot, events))
    }

    /// Session-wide event observation for the family controller. The snapshot
    /// remains the public main-lane projection; internal callers use the event
    /// stream only for exact operation-addressed waits across shared lanes.
    pub(crate) async fn subscribe_all(
        &self,
    ) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubscribeAll { reply })
            .map_err(command_send_error)?;
        let (snapshot, events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok((snapshot, events))
    }

    /// Close the session (DESIGN.md §9.5): lifecycle shutdown, never a
""",
    ),
    (
        "runtime event fields",
        """    events: broadcast::Sender<RuntimeEvent>,
    /// Persisted indeterminate outcomes that must remain visible to a
""",
        """    /// Session-wide event ring used only by operation-addressed family waits.
    events: broadcast::Sender<RuntimeEvent>,
    /// Main-lane event ring paired with the public main-lane snapshot.
    main_events: broadcast::Sender<RuntimeEvent>,
    /// Persisted indeterminate outcomes that must remain visible to a
""",
    ),
    (
        "event ring construction",
        """        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut lanes = BTreeMap::new();
""",
        """        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let (main_events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut lanes = BTreeMap::new();
""",
    ),
    (
        "event ring initialization",
        """            events,
            indeterminate_warning: None,
""",
        """            events,
            main_events,
            indeterminate_warning: None,
""",
    ),
    (
        "command dispatch",
        """            SessionCommand::Subscribe { reply } => {
                let _ = reply.send(self.subscribe());
                false
            }
            SessionCommand::Close { reply } => {
""",
        """            SessionCommand::Subscribe { reply } => {
                let _ = reply.send(self.subscribe());
                false
            }
            SessionCommand::SubscribeAll { reply } => {
                let _ = reply.send(self.subscribe_all());
                false
            }
            SessionCommand::Close { reply } => {
""",
    ),
    (
        "runtime subscriptions",
        """    fn subscribe(&mut self) -> SubscribeReply {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let snapshot = self.snapshot();
        let rx = self.events.subscribe();
        Ok((snapshot, EventSubscription { rx }))
    }

    fn snapshot(&self) -> SessionSnapshot {
""",
        """    fn subscribe(&mut self) -> SubscribeReply {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let snapshot = self.snapshot();
        let rx = self.main_events.subscribe();
        Ok((snapshot, EventSubscription { rx }))
    }

    fn subscribe_all(&mut self) -> SubscribeReply {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let snapshot = self.snapshot();
        let rx = self.events.subscribe();
        Ok((snapshot, EventSubscription { rx }))
    }

    fn snapshot(&self) -> SessionSnapshot {
""",
    ),
    (
        "event routing",
        """        // A full ring drops the oldest buffered events for that
        // receiver; the receiver detects the gap and reports lag
        // reliably (broadcast semantics, §21.4). No receivers is the
        // normal idle case.
        let _ = self.events.send(event);
""",
        """        // Frontends project main, so sibling-lane traffic must not alter or
        // overflow their bounded event ring. Family waits retain a separate
        // session-wide stream and filter by exact operation identity.
        let is_main_event = event.operation_id().map_or(true, |operation_id| {
            self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN)
        });
        if is_main_event {
            let _ = self.main_events.send(event.clone());
        }
        // A full ring drops the oldest buffered events for that receiver; the
        // receiver detects the gap reliably. No receivers is the normal idle
        // case for either ring.
        let _ = self.events.send(event);
""",
    ),
]
for label, old, new in changes:
    text = replace_once(text, old, new, label)
runtime.write_text(text)

agent = Path("crates/ion-core/src/agent.rs")
text = agent.read_text()
count = text.count(".subscribe().await?")
assert count == 4, f"expected four family subscriptions, found {count}"
agent.write_text(text.replace(".subscribe().await?", ".subscribe_all().await?"))

multi = Path("crates/ion-core/src/tests/multi_lane.rs")
text = multi.read_text()
old = 'let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");'
text = replace_once(
    text,
    old,
    'let (_snapshot, mut events) = session.subscribe_all().await.expect("subscribe all");',
    "multi-lane observer",
)
text += r'''

#[tokio::test]
async fn frontend_subscription_projects_only_the_main_lane() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(250),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let session = runtime.session();

    session.create_lane("worker").await.expect("worker lane");
    let (snapshot, mut frontend_events) = session.subscribe().await.expect("frontend subscribe");
    assert!(matches!(snapshot.operation, OperationStatus::Idle));
    let (_snapshot, mut all_events) = session.subscribe_all().await.expect("family subscribe");

    let worker_operation = session
        .submit_if_idle_on_lane("worker", "worker prompt")
        .await
        .expect("worker operation");
    let observed_worker = timeout(Duration::from_secs(1), async {
        loop {
            if let RuntimeEvent::OperationStarted { operation_id, .. } =
                all_events.recv().await.expect("all-lane event")
            {
                break operation_id;
            }
        }
    })
    .await
    .expect("family observer sees worker start");
    assert_eq!(observed_worker, worker_operation);

    let snapshot = session.snapshot().await.expect("main snapshot");
    assert!(matches!(snapshot.operation, OperationStatus::Idle));
    let main_operation = session
        .submit_if_idle("main prompt")
        .await
        .expect("main operation");
    let observed_main = timeout(Duration::from_secs(1), async {
        loop {
            if let RuntimeEvent::OperationStarted { operation_id, .. } =
                frontend_events.recv().await.expect("frontend event")
            {
                break operation_id;
            }
        }
    })
    .await
    .expect("frontend observer sees main start");
    assert_eq!(observed_main, main_operation);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
multi.write_text(text)

design = Path("DESIGN.md")
text = design.read_text()
old = "The execution host/session writer is authoritative. TUI, print/JSON, ACP, and future remote or multi-client protocols project an initial snapshot plus ordered runtime/session events/actions and send commands carrying stable semantic IDs. A client disconnect must not implicitly cancel durable work. Settle this boundary before redesigning the TUI so frontend ownership never leaks into execution semantics."
new = "The execution host/session writer is authoritative. TUI, print/JSON, ACP, and future remote or multi-client protocols project an initial snapshot plus ordered runtime/session events/actions and send commands carrying stable semantic IDs. The public frontend subscription is a coherent `main`-lane projection: its snapshot and bounded event ring describe the same lane, so sibling-lane agents cannot mutate or overflow foreground presentation. Family control has a separate internal all-lane event observation path and filters it by stable operation identity. A client disconnect must not implicitly cancel durable work. Settle this boundary before redesigning the TUI so frontend ownership never leaks into execution semantics."
text = replace_once(text, old, new, "DESIGN §16.1")
old = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition; after terminal acquisition, explicitly restore the terminal before startup diagnostics and unwind acquired store/catalog/runtime ownership on failure. Keep `SessionHandle` as the only runtime mutation path and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
new = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition; after terminal acquisition, explicitly restore the terminal before startup diagnostics and unwind acquired store/catalog/runtime ownership on failure. Keep the public frontend snapshot/event subscription coherent on `main` while family waits observe all lanes internally. Keep `SessionHandle` as the only runtime mutation path and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
text = replace_once(text, old, new, "Step 10")
design.write_text(text)
