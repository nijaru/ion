from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


def rewrite_between(text: str, start: str, end: str, label: str, transform) -> str:
    start_i = text.find(start)
    if start_i < 0:
        raise SystemExit(f"{label}: start not found")
    end_i = text.find(end, start_i)
    if end_i < 0:
        raise SystemExit(f"{label}: end not found")
    block = text[start_i:end_i]
    rewritten = transform(block)
    if rewritten == block:
        raise SystemExit(f"{label}: transform made no changes")
    return text[:start_i] + rewritten + text[end_i:]


error = Path("crates/ion-core/src/error.rs")
runtime = Path("crates/ion-core/src/runtime/mod.rs")
tests_rs = Path("crates/ion-core/src/tests.rs")
multi_lane = Path("crates/ion-core/src/tests/multi_lane.rs")

# Typed topology errors stay above persistence failures.
text = error.read_text()
text = replace_once(
    text,
    '''    #[error("the lane already has a pending next run ({entry_id})")]
    NextRunQueued { entry_id: EntryId },
''',
    '''    #[error("the lane already has a pending next run ({entry_id})")]
    NextRunQueued { entry_id: EntryId },
    #[error("lane {0:?} does not exist")]
    LaneNotFound(String),
    #[error("lane {0:?} already exists")]
    LaneExists(String),
    #[error("lane name cannot be empty")]
    InvalidLaneName,
''',
    "lane command errors",
)
error.write_text(text)

text = runtime.read_text()

# Commands carry lane address explicitly. Existing SessionHandle methods remain
# main-lane conveniences; the writer never infers the target from call shape.
text = replace_once(
    text,
    '''enum SessionCommand {
    SubmitIfIdle {
        prompt: String,
''',
    '''enum SessionCommand {
    CreateLane {
        lane_name: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    SubmitIfIdle {
        lane_name: String,
        prompt: String,
''',
    "create-lane and submit command",
)
text = replace_once(
    text,
    '''    NextRun {
        prompt: String,
''',
    '''    NextRun {
        lane_name: String,
        prompt: String,
''',
    "next-run lane command",
)
text = replace_once(
    text,
    '''    SwitchModel {
        model_ref: String,
''',
    '''    SwitchModel {
        lane_name: String,
        model_ref: String,
''',
    "switch-model lane command",
)

# Replace the SessionHandle prompt/next-run/model methods with main convenience
# wrappers plus explicit lane-targeted forms.
def rewrite_handle_submit(block: str) -> str:
    return '''    /// Create a shared-history lane at the main lane's current durable leaf.
    /// The new lane inherits the main lane's current model configuration.
    pub async fn create_lane(&self, lane_name: impl Into<String>) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::CreateLane {
                lane_name: lane_name.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Accept a prompt durably on main and open a new operation only when idle.
    pub async fn submit_if_idle(
        &self,
        prompt: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        self.submit_if_idle_on_lane(crate::session::lane::MAIN, prompt)
            .await
    }

    /// Accept a prompt durably on one named lane only when that lane is idle.
    pub async fn submit_if_idle_on_lane(
        &self,
        lane_name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubmitIfIdle {
                lane_name: lane_name.into(),
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

'''

text = rewrite_between(
    text,
    "    /// Accept a prompt durably and open a new operation only when idle.",
    "    /// Persist the lane's next-run input.",
    "session handle lane submit",
    rewrite_handle_submit,
)

def rewrite_handle_next(block: str) -> str:
    return '''    /// Persist main's next-run input. If main is idle it is accepted
    /// immediately; otherwise only its semantic entry identity is reserved.
    pub async fn next_run(
        &self,
        prompt: impl Into<String>,
    ) -> Result<crate::ids::EntryId, CommandError> {
        self.next_run_on_lane(crate::session::lane::MAIN, prompt).await
    }

    /// Persist one named lane's next-run input. Operation identity is created
    /// only when that lane actually accepts the run.
    pub async fn next_run_on_lane(
        &self,
        lane_name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<crate::ids::EntryId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::NextRun {
                lane_name: lane_name.into(),
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

'''

text = rewrite_between(
    text,
    "    /// Persist the lane's next-run input.",
    "    /// Join the active operation",
    "session handle lane next-run",
    rewrite_handle_next,
)

def rewrite_handle_switch(block: str) -> str:
    return '''    /// Durably select the model used by future main-lane model steps.
    /// A running step keeps its frozen model snapshot. Returns the previous id.
    pub async fn switch_model(&self, model_ref: impl Into<String>) -> Result<String, CommandError> {
        self.switch_model_on_lane(crate::session::lane::MAIN, model_ref)
            .await
    }

    /// Durably select the model used by future steps on one named lane.
    pub async fn switch_model_on_lane(
        &self,
        lane_name: impl Into<String>,
        model_ref: impl Into<String>,
    ) -> Result<String, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SwitchModel {
                lane_name: lane_name.into(),
                model_ref: model_ref.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

'''

text = rewrite_between(
    text,
    "    /// Durably select the model used by future model steps.",
    "    pub async fn cancel",
    "session handle lane model",
    rewrite_handle_switch,
)

# Writer dispatch uses the explicit lane address.
text = replace_once(
    text,
    '''        match command {
            SessionCommand::SubmitIfIdle { prompt, reply } => {
                let _ = reply.send(self.submit_if_idle(prompt).await);
                false
            }
            SessionCommand::NextRun { prompt, reply } => {
                let _ = reply.send(self.next_run(prompt).await);
                false
            }
''',
    '''        match command {
            SessionCommand::CreateLane { lane_name, reply } => {
                let _ = reply.send(self.create_lane(lane_name).await);
                false
            }
            SessionCommand::SubmitIfIdle {
                lane_name,
                prompt,
                reply,
            } => {
                let _ = reply.send(self.submit_if_idle_on_lane(lane_name, prompt).await);
                false
            }
            SessionCommand::NextRun {
                lane_name,
                prompt,
                reply,
            } => {
                let _ = reply.send(self.next_run_on_lane(lane_name, prompt).await);
                false
            }
''',
    "dispatch lane commands",
)
text = replace_once(
    text,
    '''            SessionCommand::SwitchModel { model_ref, reply } => {
                let _ = reply.send(self.switch_model(model_ref).await);
                false
            }
''',
    '''            SessionCommand::SwitchModel {
                lane_name,
                model_ref,
                reply,
            } => {
                let _ = reply.send(self.switch_model_on_lane(lane_name, model_ref).await);
                false
            }
''',
    "dispatch lane model",
)

# Runtime lane creation is committed before live topology is installed.
insert_before = "    async fn submit_if_idle(&mut self, prompt: String) -> Result<OperationId, CommandError> {"
idx = text.find(insert_before)
if idx < 0:
    raise SystemExit("runtime submit function anchor not found")
create_runtime = '''    async fn create_lane(&mut self, lane_name: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let lane_name = lane_name.trim().to_owned();
        if lane_name.is_empty() {
            return Err(CommandError::InvalidLaneName);
        }
        if self.lanes.contains_key(&lane_name) {
            return Err(CommandError::LaneExists(lane_name));
        }
        let source_leaf = self.main_lane().state.leaf;
        let model_ref = self.main_model_ref().to_owned();
        self.store
            .create_lane(
                self.session_id,
                lane_name.clone(),
                source_leaf,
                model_ref.clone(),
            )
            .await
            .map_err(persistence_command_error)?;
        let previous = self.lanes.insert(
            lane_name.clone(),
            ResidentLane::new(crate::session::lane::Lane {
                name: lane_name,
                state: crate::session::lane::State {
                    leaf: source_leaf,
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new(model_ref),
            }),
        );
        debug_assert!(previous.is_none(), "lane topology identity is unique");
        Ok(())
    }

'''
text = text[:idx] + create_runtime + text[idx:]

# Generic submit path.
def rewrite_submit(block: str) -> str:
    return '''    async fn submit_if_idle_on_lane(
        &mut self,
        lane_name: String,
        prompt: String,
    ) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        if let Some(operation_id) = self.lane_resident_id(&lane_name) {
            return Err(CommandError::Busy { operation_id });
        }
        if let Some(pending) = self.lane_pending_next_run(&lane_name) {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        let (active, _) = self.accept_operation_record(&lane_name, prompt, None).await?;
        let operation_id = active.machine.operation_id();
        self.start_active(&lane_name, active);
        self.advance(operation_id).await;
        Ok(operation_id)
    }

'''

text = rewrite_between(
    text,
    "    async fn submit_if_idle(&mut self, prompt: String) -> Result<OperationId, CommandError> {",
    "    /// Persist one next-run input durably.",
    "generic lane submit",
    rewrite_submit,
)

# Generic next-run path.
def rewrite_next(block: str) -> str:
    return '''    async fn next_run_on_lane(
        &mut self,
        lane_name: String,
        prompt: String,
    ) -> Result<crate::ids::EntryId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        if let Some(pending) = self.lane_pending_next_run(&lane_name) {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        if self.lane_resident_id(&lane_name).is_none() {
            let (active, entry_id) = self
                .accept_operation_record(&lane_name, prompt, None)
                .await?;
            let operation_id = active.machine.operation_id();
            self.start_active(&lane_name, active);
            self.advance(operation_id).await;
            return Ok(entry_id);
        }

        let next_run = crate::session::lane::NextRun::reserve(prompt);
        let entry_id = next_run.entry_id;
        self.store
            .queue_next_run(self.session_id, &lane_name, next_run.clone())
            .await
            .map_err(persistence_command_error)?;
        self.wait_effect_boundary(EffectBoundary::PendingNextRunCommit)
            .await;
        self.lane_mut(&lane_name)
            .expect("queued lane remains resident")
            .state
            .pending_next_run = Some(next_run);
        Ok(entry_id)
    }

'''
text = rewrite_between(
    text,
    "    async fn next_run(&mut self, prompt: String) -> Result<crate::ids::EntryId, CommandError> {",
    "    /// Create the durable operation only when the lane is free.",
    "generic lane next-run",
    rewrite_next,
)

# Generic lane config path.
def rewrite_switch(block: str) -> str:
    return '''    async fn switch_model_on_lane(
        &mut self,
        lane_name: String,
        model_ref: String,
    ) -> Result<String, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        let model_ref = model_ref.trim().to_owned();
        if model_ref.is_empty() || !self.provider.supports_model(&model_ref) {
            return Err(CommandError::UnsupportedModel(model_ref));
        }
        let previous = self
            .lane(&lane_name)
            .expect("checked lane")
            .config
            .model_ref
            .clone();
        if model_ref == previous {
            return Ok(previous);
        }
        self.store
            .set_lane_config(
                self.session_id,
                &lane_name,
                crate::session::lane::Config::new(model_ref.clone()),
            )
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(&lane_name)
            .expect("configured lane remains resident")
            .config
            .model_ref = model_ref;
        let live = self
            .lane_live_mut(&lane_name)
            .expect("configured lane residency remains live");
        live.context_window = None;
        live.model_capabilities = None;
        live.last_prefix_fingerprint = None;
        Ok(previous)
    }

'''
text = rewrite_between(
    text,
    "    async fn switch_model(&mut self, model_ref: String) -> Result<String, CommandError> {",
    "    async fn enqueue_steer",
    "generic lane model config",
    rewrite_switch,
)

runtime.write_text(text)

# Register and write the multi-lane concurrency test.
text = tests_rs.read_text()
text = replace_once(text, "mod mcp;\n", "mod mcp;\nmod multi_lane;\n", "register multi-lane tests")
tests_rs.write_text(text)

multi_lane.write_text(r'''use super::support::*;

#[tokio::test]
async fn two_lanes_run_slow_provider_effects_concurrently_under_one_writer() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");

    let main_operation = session
        .submit_if_idle("main prompt")
        .await
        .expect("main operation");
    timeout(Duration::from_millis(200), async {
        loop {
            if provider.requests().len() == 1 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("main provider request started");

    session.create_lane("worker").await.expect("worker lane");
    let worker_operation = session
        .submit_if_idle_on_lane("worker", "worker prompt")
        .await
        .expect("worker operation");
    assert_ne!(main_operation, worker_operation);

    // The first provider effect cannot settle before 500 ms. Requiring the
    // second request within 200 ms proves the session writer admitted another
    // lane while the first slow effect remained in flight.
    timeout(Duration::from_millis(200), async {
        loop {
            if provider.requests().len() == 2 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("worker provider request started concurrently");

    let requests = provider.requests();
    assert!(requests.iter().any(|request| request.operation_id == main_operation));
    assert!(requests
        .iter()
        .any(|request| request.operation_id == worker_operation));

    let worker_request = requests
        .iter()
        .find(|request| request.operation_id == worker_operation)
        .expect("worker request");
    let worker_user_messages = worker_request
        .plan
        .messages
        .iter()
        .filter_map(|message| match message {
            ContextMessage::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(worker_user_messages.contains(&"main prompt"));
    assert!(worker_user_messages.contains(&"worker prompt"));

    let mut finished = Vec::new();
    timeout(Duration::from_secs(2), async {
        while finished.len() < 2 {
            match events.recv().await.expect("runtime event") {
                RuntimeEvent::OperationFinished { operation_id, .. }
                    if operation_id == main_operation || operation_id == worker_operation =>
                {
                    if !finished.contains(&operation_id) {
                        finished.push(operation_id);
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("both lanes finish");

    let loaded = store.load(session_id).await.expect("load session");
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane");
    let worker = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "worker")
        .expect("worker lane");
    assert!(main.state.current_operation.is_none());
    assert!(worker.state.current_operation.is_none());
    assert_ne!(main.state.leaf, worker.state.leaf);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
''')
