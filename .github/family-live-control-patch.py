from pathlib import Path

# Family owns live session addressability for separately hosted agents. The
# child manager still owns provider/catalog/runtime resources and capacity.
p = Path("crates/ion-core/src/agent.rs")
text = p.read_text()

old = '''    #[error("agent {0} has no running operation")]
    NotRunning(AgentId),
    #[error("agent wait set cannot be empty")]
'''
new = '''    #[error("agent {0} has no running operation")]
    NotRunning(AgentId),
    #[error("agent {0} has no live hosted-session residency")]
    NotResident(AgentId),
    #[error("agent wait set cannot be empty")]
'''
if old not in text:
    raise SystemExit("agent error anchor missing")
text = text.replace(old, new, 1)

old = '''    retained: Mutex<HashMap<AgentId, RetainedAgent>>,
    executions: Mutex<HashMap<AgentId, Execution>>,
    permits: Arc<Semaphore>,
'''
new = '''    retained: Mutex<HashMap<AgentId, RetainedAgent>>,
    executions: Mutex<HashMap<AgentId, Execution>>,
    hosted_sessions: Mutex<HashMap<SessionId, SessionHandle>>,
    permits: Arc<Semaphore>,
'''
if old not in text:
    raise SystemExit("family field anchor missing")
text = text.replace(old, new, 1)

old = '''            retained: Mutex::new(retained),
            executions: Mutex::new(executions),
            permits,
'''
new = '''            retained: Mutex::new(retained),
            executions: Mutex::new(executions),
            hosted_sessions: Mutex::new(HashMap::new()),
            permits,
'''
if old not in text:
    raise SystemExit("family init anchor missing")
text = text.replace(old, new, 1)

anchor = '''    /// Admit a retained shared-history agent from the control parent's current
    /// lane boundary. Identity + lane publication is one durable transaction;
'''
insert = '''    /// Register one process-local runtime incarnation for a separately hosted
    /// family session. Durable identity/topology remains store-owned; this map
    /// exists only so family control can route live wait/cancel operations.
    pub(crate) fn register_hosted_session(
        &self,
        session_id: SessionId,
        session: SessionHandle,
    ) {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .insert(session_id, session);
    }

    /// Forget one hosted runtime incarnation without changing durable agent
    /// identity or completion state.
    pub(crate) fn unregister_hosted_session(&self, session_id: SessionId) {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .remove(&session_id);
    }

    fn hosted_session(&self, session_id: SessionId) -> Option<SessionHandle> {
        self.hosted_sessions
            .lock()
            .expect("agent family poisoned")
            .get(&session_id)
            .cloned()
    }

'''
if anchor not in text:
    raise SystemExit("family hosted-session insertion anchor missing")
text = text.replace(anchor, insert + anchor, 1)

old = '''    pub async fn wait_one(
        &self,
        agent_id: AgentId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        let mut completed = self
            .wait_set(&[agent_id], WaitMode::All, cancel, deadline)
            .await?;
        Ok(completed
            .pop()
            .expect("one-agent wait returns exactly one result")
            .1)
    }

    /// Return when any execution in `agent_ids` reaches a durable terminal or
'''
new = '''    pub async fn wait_one(
        &self,
        agent_id: AgentId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        match self.target(agent_id).await? {
            AgentTarget::SharedHistory { .. } => {
                let mut completed = self
                    .wait_set(&[agent_id], WaitMode::All, cancel, deadline)
                    .await?;
                Ok(completed
                    .pop()
                    .expect("one-agent wait returns exactly one result")
                    .1)
            }
            AgentTarget::SeparateSession { session_id } => {
                self.wait_hosted_one(agent_id, session_id, cancel, deadline)
                    .await
            }
        }
    }

    async fn wait_hosted_one(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Status, Error> {
        let initial = self.status(agent_id).await?;
        let operation_id = match initial {
            Status::Admitted => return Err(Error::NotRunning(agent_id)),
            Status::Active { operation_id, .. } => operation_id,
            terminal @ (Status::Suspended { .. } | Status::Finished { .. }) => {
                return Ok(terminal);
            }
        };

        let Some(session) = self.hosted_session(session_id) else {
            let status = self.observe_operation(agent_id, operation_id).await?.status;
            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                return Ok(status);
            }
            return Err(Error::NotResident(agent_id));
        };

        // Subscribe before the second durable read. A completion racing the
        // first read is therefore either present in the store or retained in
        // this subscriber's event ring.
        let (_snapshot, mut events) = session.subscribe().await?;
        let status = self.observe_operation(agent_id, operation_id).await?.status;
        if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
            return Ok(status);
        }

        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err(Error::WaitCancelled),
                () = wait_until(deadline) => return Err(Error::WaitDeadlineElapsed),
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            if matches!(event, RuntimeEvent::SessionClosed { .. }) {
                                let status = self.observe_operation(agent_id, operation_id).await?.status;
                                if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                    return Ok(status);
                                }
                                return Err(RuntimeError::SubscriptionClosed.into());
                            }
                            if event.operation_id() != Some(operation_id) || !event_is_terminal(&event) {
                                continue;
                            }
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                        }
                        Err(RuntimeError::SubscriptionLagged) => {
                            let (_snapshot, replacement) = session.subscribe().await?;
                            events = replacement;
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                        }
                        Err(RuntimeError::SubscriptionClosed) => {
                            let status = self.observe_operation(agent_id, operation_id).await?.status;
                            if matches!(status, Status::Suspended { .. } | Status::Finished { .. }) {
                                return Ok(status);
                            }
                            return Err(RuntimeError::SubscriptionClosed.into());
                        }
                        Err(other) => return Err(other.into()),
                    }
                }
            }
        }
    }

    /// Return when any execution in `agent_ids` reaches a durable terminal or
'''
if old not in text:
    raise SystemExit("wait_one anchor missing")
text = text.replace(old, new, 1)

old = '''    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {
        if !matches!(
            self.target(agent_id).await?,
            AgentTarget::SharedHistory { .. }
        ) {
            return Err(Error::UnknownAgent(agent_id));
        }
        let status = self.status(agent_id).await?;
        let operation_id = match status {
            Status::Active { operation_id, .. } => operation_id,
            Status::Admitted | Status::Suspended { .. } | Status::Finished { .. } => {
                return Err(Error::NotRunning(agent_id));
            }
        };
        self.session.cancel(operation_id).await?;
        Ok(())
    }
'''
new = '''    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {
        let target = self.target(agent_id).await?;
        let status = self.status(agent_id).await?;
        let operation_id = match status {
            Status::Active { operation_id, .. } => operation_id,
            Status::Admitted | Status::Suspended { .. } | Status::Finished { .. } => {
                return Err(Error::NotRunning(agent_id));
            }
        };
        let session = match target {
            AgentTarget::SharedHistory { .. } => self.session.clone(),
            AgentTarget::SeparateSession { session_id } => {
                let Some(session) = self.hosted_session(session_id) else {
                    let latest = self.observe_operation(agent_id, operation_id).await?.status;
                    if !matches!(latest, Status::Active { .. }) {
                        return Err(Error::NotRunning(agent_id));
                    }
                    return Err(Error::NotResident(agent_id));
                };
                session
            }
        };
        session.cancel(operation_id).await?;
        Ok(())
    }
'''
if old not in text:
    raise SystemExit("family cancel anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)

# ChildManager remains the owner of hosted runtime resources, but registers
# each live SessionHandle with Family. Unified wait/cancel then route through
# semantic AgentId rather than child handles.
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()

old = '''pub struct ChildManager<P> {
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
    children: Mutex<HashMap<SessionId, ManagedChild>>,
}
'''
new = '''pub struct ChildManager<P> {
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
    children: Mutex<HashMap<SessionId, ManagedChild>>,
    family: Mutex<Option<Arc<crate::agent::Family>>>,
}
'''
if old not in text:
    raise SystemExit("ChildManager field anchor missing")
text = text.replace(old, new, 1)

old = '''        Arc::new(Self {
            config,
            parent_id,
            children: Mutex::new(HashMap::new()),
        })
    }

    fn live_session(&self, session_id: SessionId) -> Option<crate::runtime::SessionHandle> {
'''
new = '''        Arc::new(Self {
            config,
            parent_id,
            children: Mutex::new(HashMap::new()),
            family: Mutex::new(None),
        })
    }

    fn bind_family(&self, family: Arc<crate::agent::Family>) {
        debug_assert_eq!(
            SessionId::from_uuid(family.root().as_uuid()),
            self.parent_id,
            "child runtime host must bind to its durable root family",
        );
        *self.family.lock().expect("child manager poisoned") = Some(Arc::clone(&family));
        let live = self
            .children
            .lock()
            .expect("child manager poisoned")
            .iter()
            .map(|(session_id, child)| (*session_id, child.session.clone()))
            .collect::<Vec<_>>();
        for (session_id, session) in live {
            family.register_hosted_session(session_id, session);
        }
    }

    fn family(&self) -> Option<Arc<crate::agent::Family>> {
        self.family
            .lock()
            .expect("child manager poisoned")
            .clone()
    }

    fn live_session(&self, session_id: SessionId) -> Option<crate::runtime::SessionHandle> {
'''
if old not in text:
    raise SystemExit("ChildManager constructor anchor missing")
text = text.replace(old, new, 1)

old = '''        child.cancel.cancel();
        let mut first_error = None;
'''
new = '''        if let Some(family) = self.family() {
            family.unregister_hosted_session(session_id);
        }
        child.cancel.cancel();
        let mut first_error = None;
'''
if old not in text:
    raise SystemExit("release unregister anchor missing")
text = text.replace(old, new, 1)

old = '''        };
        let session = runtime.session();
        let operation_id = match session.submit_if_idle(prompt).await {
'''
new = '''        };
        let session = runtime.session();
        if let Some(family) = self.family() {
            family.register_hosted_session(session_id, session.clone());
        }
        let operation_id = match session.submit_if_idle(prompt).await {
'''
if old not in text:
    raise SystemExit("spawn register anchor missing")
text = text.replace(old, new, 1)

old = '''            Err(err) => {
                let _ = session.close().await;
                let _ = runtime.join().await;
                let _ = catalog.close().await;
                return Err(format!(
                    "child agent {agent_id} was admitted, but start failed: {err}"
                ));
            }
'''
new = '''            Err(err) => {
                if let Some(family) = self.family() {
                    family.unregister_hosted_session(session_id);
                }
                let _ = session.close().await;
                let _ = runtime.join().await;
                let _ = catalog.close().await;
                return Err(format!(
                    "child agent {agent_id} was admitted, but start failed: {err}"
                ));
            }
'''
if old not in text:
    raise SystemExit("spawn failure unregister anchor missing")
text = text.replace(old, new, 1)

# The second Runtime::open_child site is resume; register the reopened handle.
old = '''        .await
        .map_err(|err| format!("child {} resume failed: {err}", handle.session_id))?;
        let session = runtime.session();
        let cancel = parent_cancel.child_token();
'''
new = '''        .await
        .map_err(|err| format!("child {} resume failed: {err}", handle.session_id))?;
        let session = runtime.session();
        if let Some(family) = self.family() {
            family.register_hosted_session(handle.session_id, session.clone());
        }
        let cancel = parent_cancel.child_token();
'''
if old not in text:
    raise SystemExit("resume register anchor missing")
text = text.replace(old, new, 1)

old = '''pub fn agent_host_tools<P: Provider + 'static>(
    family: Arc<crate::agent::Family>,
    children: Arc<ChildManager<P>>,
) -> Vec<Arc<dyn Tool>> {
    [
'''
new = '''pub fn agent_host_tools<P: Provider + 'static>(
    family: Arc<crate::agent::Family>,
    children: Arc<ChildManager<P>>,
) -> Vec<Arc<dyn Tool>> {
    children.bind_family(Arc::clone(&family));
    [
'''
if old not in text:
    raise SystemExit("agent_host_tools bind anchor missing")
text = text.replace(old, new, 1)

old = '''                HostAgentToolKind::Wait => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.target(agent_id).await {
                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => {
                            let status = match self.family.wait_one(agent_id, cancel, None).await {
                                Ok(status) => status,
                                Err(err) => return ToolOutcome::error(err.to_string()),
                            };
                            let operation_id = host_status_operation_id(&status)
                                .expect("family wait rejects admitted agents");
                            match self.family.observe_operation(agent_id, operation_id).await {
                                Ok(observation) => {
                                    ToolOutcome::text(render_family_observation(&observation))
                                }
                                Err(err) => ToolOutcome::error(err.to_string()),
                            }
                        }
                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => {
                            let handle =
                                child_handle_for_session(session_id, self.children.parent_id);
                            match self.children.wait(handle, cancel, progress.as_ref()).await {
                                Ok(observation) => match observation.operation_id() {
                                    Some(operation_id) => match self
                                        .family
                                        .observe_operation(agent_id, operation_id)
                                        .await
                                    {
                                        Ok(observation) => ToolOutcome::text(
                                            render_family_observation(&observation),
                                        ),
                                        Err(err) => ToolOutcome::error(err.to_string()),
                                    },
                                    None => ToolOutcome::text(render_child_as_agent(
                                        agent_id,
                                        &observation,
                                    )),
                                },
                                Err(err) => ToolOutcome::error(err),
                            }
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
'''
new = '''                HostAgentToolKind::Wait => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let target = match self.family.target(agent_id).await {
                        Ok(target) => target,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    let status = match self.family.wait_one(agent_id, cancel, None).await {
                        Ok(status) => status,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    let operation_id = host_status_operation_id(&status)
                        .expect("family wait rejects admitted agents");
                    let observation = match self
                        .family
                        .observe_operation(agent_id, operation_id)
                        .await
                    {
                        Ok(observation) => observation,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    if let crate::agent::AgentTarget::SeparateSession { session_id } = target
                        && matches!(status, crate::agent::Status::Finished { .. })
                    {
                        if let Err(err) = self.children.release_live_child(session_id).await {
                            return ToolOutcome::error(err);
                        }
                        report_progress(progress.as_ref(), format!("agent {agent_id} finished")).await;
                    }
                    ToolOutcome::text(render_family_observation(&observation))
                }
'''
if old not in text:
    raise SystemExit("unified wait anchor missing")
text = text.replace(old, new, 1)

old = '''                HostAgentToolKind::Cancel => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.target(agent_id).await {
                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => {
                            match self.family.cancel(agent_id).await {
                                Ok(()) => ToolOutcome::text(format!(
                                    "cancellation accepted for agent {agent_id}"
                                )),
                                Err(err) => ToolOutcome::error(err.to_string()),
                            }
                        }
                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => {
                            let handle =
                                child_handle_for_session(session_id, self.children.parent_id);
                            match self.children.cancel(handle).await {
                                Ok(()) => ToolOutcome::text(format!(
                                    "cancellation accepted for agent {agent_id}"
                                )),
                                Err(err) => ToolOutcome::error(err),
                            }
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
'''
new = '''                HostAgentToolKind::Cancel => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.cancel(agent_id).await {
                        Ok(()) => ToolOutcome::text(format!(
                            "cancellation accepted for agent {agent_id}"
                        )),
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
'''
if old not in text:
    raise SystemExit("unified cancel anchor missing")
text = text.replace(old, new, 1)

old = '''fn render_child_as_agent(agent_id: crate::ids::AgentId, observation: &ChildObservation) -> String {
    let status = match &observation.status {
        ChildStatus::Starting => "starting".to_owned(),
        ChildStatus::Active {
            operation_id,
            state,
        } => {
            format!("active ({operation_id}, {state:?})")
        }
        ChildStatus::Suspended { operation_id } => format!("suspended ({operation_id})"),
        ChildStatus::Finished {
            operation_id,
            outcome,
        } => {
            format!("finished ({operation_id}, {outcome:?})")
        }
    };
    match &observation.result {
        Some(result) => format!("agent {agent_id}: {status}\\n\\n{result}"),
        None => format!("agent {agent_id}: {status}"),
    }
}

'''
if old not in text:
    raise SystemExit("obsolete render_child_as_agent anchor missing")
text = text.replace(old, "", 1)
p.write_text(text)

# Focused proof: a fresh agent's delayed operation can be waited directly by
# Family after the runtime host registers its SessionHandle.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
append = r'''

#[tokio::test]
async fn family_wait_routes_to_live_separate_session_by_agent_address() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        SharedLogProvider::default(),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    let child_provider = SharedLogProvider {
        settle_delay: Duration::from_millis(250),
        ..SharedLogProvider::default()
    };
    let child_provider_factory = child_provider.clone();
    let (children, _legacy_child_tools) = crate::child_tools(
        crate::DelegateConfig {
            store: store.clone(),
            make_provider: Arc::new(move || child_provider_factory.clone()),
            make_provider_for_model: None,
            max_active_children: 1,
            child_budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&children));
    let spawn = tools
        .iter()
        .find(|tool| tool.spec().name == "spawn_agent")
        .expect("spawn tool");
    let spawned = spawn
        .call(
            json!({"objective": "hosted wait", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!spawned.is_error, "fresh spawn failed: {spawned:?}");
    let raw = spawned
        .output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("agent handle");
    let agent_id = crate::AgentId::parse(raw.strip_prefix("agent-").expect("agent prefix"))
        .expect("agent id");

    timeout(Duration::from_secs(2), async {
        loop {
            if !child_provider.requests().is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hosted provider started");
    assert!(matches!(
        family.status(agent_id).await.expect("active hosted status"),
        crate::AgentStatus::Active { .. }
    ));

    let status = timeout(
        Duration::from_secs(2),
        family.wait_one(agent_id, CancellationToken::new(), None),
    )
    .await
    .expect("family hosted wait timed out")
    .expect("family hosted wait");
    let operation_id = match status {
        crate::AgentStatus::Finished { operation_id, .. } => operation_id,
        other => panic!("expected hosted completion, got {other:?}"),
    };
    let observation = family
        .observe_operation(agent_id, operation_id)
        .await
        .expect("hosted observation");
    assert_eq!(observation.result.as_deref(), Some("working"));

    children.close().await.expect("close hosted runtimes");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}
'''
if "family_wait_routes_to_live_separate_session_by_agent_address" in text:
    raise SystemExit("hosted wait test already exists")
p.write_text(text + append)
