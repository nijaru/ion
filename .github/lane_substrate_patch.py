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


store_mod = Path("crates/ion-core/src/store/mod.rs")
store_sql = Path("crates/ion-core/src/store/sql.rs")
runtime = Path("crates/ion-core/src/runtime/mod.rs")
recovery = Path("crates/ion-core/src/runtime/recovery.rs")

# ---------------------------------------------------------------------------
# Store: lane topology creation is an explicit durable transition.
# ---------------------------------------------------------------------------
text = store_mod.read_text()
text = replace_once(
    text,
    '''    BeginOperation {
        session_id: SessionId,
        lane_name: String,
        operation_id: OperationId,
''',
    '''    CreateLane {
        session_id: SessionId,
        lane: crate::session::lane::Lane,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    BeginOperation {
        session_id: SessionId,
        lane_name: String,
        operation_id: OperationId,
''',
    "store create-lane command",
)
text = replace_once(
    text,
    '''    /// Durably accept an operation: the operation row, its root inbox
    /// item, the initial total state, and the user entry commit as one
    /// transaction before the caller is acknowledged (DESIGN.md §9.1).
    pub async fn begin_operation(''',
    '''    /// Create one durable lane anchored at an existing conversation leaf.
    /// The lane must be idle at creation; operations and pending input are
    /// admitted by their own transactions after topology exists.
    pub async fn create_lane(
        &self,
        session_id: SessionId,
        lane: crate::session::lane::Lane,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::CreateLane {
            session_id,
            lane,
            reply,
        })
        .await
    }

    /// Durably accept an operation: the operation row, its root inbox
    /// item, the initial total state, and the user entry commit as one
    /// transaction before the caller is acknowledged (DESIGN.md §9.1).
    pub async fn begin_operation(''',
    "store create-lane api",
)
store_mod.write_text(text)

text = store_sql.read_text()
text = replace_once(
    text,
    '''        StoreCommand::BeginOperation {
            session_id,
            lane_name,
''',
    '''        StoreCommand::CreateLane {
            session_id,
            lane,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                create_lane(connection, session_id, &lane).map_err(StoreError::from)
            }));
        }
        StoreCommand::BeginOperation {
            session_id,
            lane_name,
''',
    "handle create-lane command",
)
text = replace_once(
    text,
    '''fn set_lane_config(
    connection: &mut Connection,
''',
    '''fn create_lane(
    connection: &mut Connection,
    session_id: SessionId,
    lane: &crate::session::lane::Lane,
) -> Result<(), rusqlite::Error> {
    if lane.name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "lane name cannot be empty".to_owned(),
        ));
    }
    if lane.state.current_operation.is_some() || lane.state.pending_next_run.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(
            "a new lane must be idle and have no pending input".to_owned(),
        ));
    }
    let tx = connection.transaction()?;
    let config = serde_json::to_string(&lane.config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?5, ?5)",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane.name,
            lane.state.leaf.map(|id| id.as_uuid().to_string()),
            config,
            now,
        ],
    )?;
    let changed = tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "lane session is missing".to_owned(),
        ));
    }
    tx.commit()
}

fn set_lane_config(
    connection: &mut Connection,
''',
    "create-lane sql",
)
# Add direct coverage near the SQL tests. This also replaces the old hand-seeded
# topology in the branched-tree test with the production transition.
old_worker = '''        let worker_config =
            serde_json::to_string(&crate::session::lane::Config::new("model-b")).expect("config");
        connection
            .execute(
                "INSERT INTO lanes
                    (session_id, name, leaf_id, current_operation_id,
                     pending_entry_id, pending_prompt, config, created_at, updated_at)
                 VALUES (?1, 'worker', ?2, NULL, NULL, NULL, ?3, 0, 0)",
                rusqlite::params![
                    session_id.as_uuid().to_string(),
                    root_id.as_uuid().to_string(),
                    worker_config,
                ],
            )
            .expect("worker lane");'''
new_worker = '''        create_lane(
            &mut connection,
            session_id,
            &crate::session::lane::Lane {
                name: "worker".to_owned(),
                state: crate::session::lane::State {
                    leaf: Some(root_id),
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new("model-b"),
            },
        )
        .expect("worker lane");'''
if old_worker not in text:
    raise SystemExit("branched-tree worker lane seed not found")
text = text.replace(old_worker, new_worker, 1)
store_sql.write_text(text)

# ---------------------------------------------------------------------------
# Runtime: all admission/promotion primitives are lane-parameterized. Existing
# public commands still project main; no non-main execution is exposed yet.
# ---------------------------------------------------------------------------
text = runtime.read_text()
text = replace_once(
    text,
    '''    fn main_lane(&self) -> &crate::session::lane::Lane {
        &self
            .lanes
            .get(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
            .durable
    }

    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {
        &mut self
            .lanes
            .get_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
            .durable
    }
''',
    '''    fn lane(&self, lane_name: &str) -> Option<&crate::session::lane::Lane> {
        self.lanes.get(lane_name).map(|lane| &lane.durable)
    }

    fn lane_mut(&mut self, lane_name: &str) -> Option<&mut crate::session::lane::Lane> {
        self.lanes.get_mut(lane_name).map(|lane| &mut lane.durable)
    }

    fn main_lane(&self) -> &crate::session::lane::Lane {
        self.lane(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {
        self.lane_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }
''',
    "generic lane accessors",
)
text = replace_once(
    text,
    '''    fn main_leaf(&self) -> Option<EntryId> {
        self.main_lane().state.leaf
    }

    fn main_model_ref(&self) -> &str {
''',
    '''    fn lane_leaf(&self, lane_name: &str) -> Option<EntryId> {
        self.lane(lane_name).and_then(|lane| lane.state.leaf)
    }

    fn lane_pending_next_run(&self, lane_name: &str) -> Option<&crate::session::lane::NextRun> {
        self.lane(lane_name)?.state.pending_next_run.as_ref()
    }

    fn main_leaf(&self) -> Option<EntryId> {
        self.lane_leaf(crate::session::lane::MAIN)
    }

    fn main_model_ref(&self) -> &str {
''',
    "lane leaf and pending accessors",
)
text = replace_once(
    text,
    '''    fn main_pending_next_run(&self) -> Option<&crate::session::lane::NextRun> {
        self.main_lane().state.pending_next_run.as_ref()
    }
''',
    '''    fn main_pending_next_run(&self) -> Option<&crate::session::lane::NextRun> {
        self.lane_pending_next_run(crate::session::lane::MAIN)
    }
''',
    "main pending projection",
)

# Startup recovery/pending promotion scans every durable lane. Slow continuations
# are spawned as effects, so this loop does not serialize provider/tool work.
old_startup = '''        if self.main_active().is_some() || !self.suspended_operations.is_empty() {
            self.recover_open_operation().await;
        }
        if !self.closed
            && self.main_active().is_none()
            && self.main_pending_next_run().is_some()
            && self.promote_pending_next_run().await
            && let Some(operation_id) = self.main_resident_id()
        {
            self.advance(operation_id).await;
        }
'''
new_startup = '''        if !self.operations.is_empty() || !self.suspended_operations.is_empty() {
            self.recover_open_operation().await;
        }
        if !self.closed {
            let pending_lanes = self
                .lanes
                .iter()
                .filter(|(_, lane)| {
                    lane.durable.state.current_operation.is_none()
                        && lane.durable.state.pending_next_run.is_some()
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            for lane_name in pending_lanes {
                if let Some(operation_id) = self.promote_pending_next_run(&lane_name).await {
                    self.advance(operation_id).await;
                }
                if self.closed {
                    break;
                }
            }
        }
'''
text = replace_once(text, old_startup, new_startup, "startup all-lane recovery")

# Main commands are projections onto generic admission machinery.
text = text.replace(
    "self.accept_operation_record(prompt, None).await?",
    "self.accept_operation_record(crate::session::lane::MAIN, prompt, None).await?",
)

# Acceptance itself takes the lane name and binds entry parent/store admission/
# live lane state to that exact durable lane.
def rewrite_accept(block: str) -> str:
    block = block.replace(
        '''    async fn accept_operation_record(
        &mut self,
        prompt: String,''',
        '''    async fn accept_operation_record(
        &mut self,
        lane_name: &str,
        prompt: String,''',
    )
    block = block.replace("parent: self.main_leaf(),", "parent: self.lane_leaf(lane_name),")
    block = block.replace(
        "            None => self.stage_entry(&applied.entries[0]),",
        '''            None => EntryRecord::provision(self.next_entry_seq, applied.entries[0].clone())
                .after(self.lane_leaf(lane_name)),''',
    )
    block = block.replace(
        '''                self.session_id,
                crate::session::lane::MAIN,
                operation_id,''',
        '''                self.session_id,
                lane_name,
                operation_id,''',
    )
    block = block.replace(
        '''        let lane = self.main_lane_mut();
        lane.state.leaf = Some(entry_leaf);''',
        '''        let lane = self
            .lane_mut(lane_name)
            .expect("accepted operation lane remains resident");
        lane.state.leaf = Some(entry_leaf);''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn accept_operation_record(",
    "    fn start_active",
    "lane operation acceptance",
    rewrite_accept,
)

# Residency installation preserves the lane supplied at acceptance.
def rewrite_start(block: str) -> str:
    block = block.replace(
        "    fn start_active(&mut self, active: ActiveOperation) {",
        "    fn start_active(&mut self, lane_name: &str, active: ActiveOperation) {",
    )
    block = block.replace(
        "ResidentOperation::new(crate::session::lane::MAIN, active)",
        "ResidentOperation::new(lane_name, active)",
    )
    return block

text = rewrite_between(text, "    fn start_active", "    async fn promote_pending_next_run", "lane residency start", rewrite_start)

# All current callers start main operations; make that projection explicit.
text = text.replace("self.start_active(active);", "self.start_active(crate::session::lane::MAIN, active);")

# Pending promotion is generic and returns the newly accepted identity directly.
def rewrite_promote(block: str) -> str:
    return '''    async fn promote_pending_next_run(&mut self, lane_name: &str) -> Option<OperationId> {
        if self.lane_resident_id(lane_name).is_some() {
            return None;
        }
        let next_run = self.lane_pending_next_run(lane_name)?.clone();
        match self
            .accept_operation_record(lane_name, next_run.prompt.clone(), Some(next_run.clone()))
            .await
        {
            Ok((active, _)) => {
                let operation_id = active.machine.operation_id();
                self.start_active(lane_name, active);
                Some(operation_id)
            }
            Err(err) => {
                error!(
                    session = %self.session_id,
                    lane = lane_name,
                    entry = %next_run.entry_id,
                    %err,
                    "could not promote the durable next run; fencing until reopen"
                );
                self.closed = true;
                None
            }
        }
    }

'''

text = rewrite_between(
    text,
    "    async fn promote_pending_next_run",
    "    async fn switch_model",
    "generic pending promotion",
    rewrite_promote,
)

# Terminal continuation promotes the same owning lane, not only main.
def rewrite_advance(block: str) -> str:
    old = '''                OperationState::Finished(_) => {
                    let lane_name = self.operation_lane_name(operation_id).map(str::to_owned);
                    self.remove_operation(operation_id);
                    if lane_name.as_deref() == Some(crate::session::lane::MAIN)
                        && self.promote_pending_next_run().await
                        && let Some(next_operation_id) = self.main_resident_id()
                    {
                        operation_id = next_operation_id;
                        continue;
                    }
                    return;
                }'''
    new = '''                OperationState::Finished(_) => {
                    let lane_name = self.operation_lane_name(operation_id).map(str::to_owned);
                    self.remove_operation(operation_id);
                    if let Some(lane_name) = lane_name
                        && let Some(next_operation_id) = self.promote_pending_next_run(&lane_name).await
                    {
                        operation_id = next_operation_id;
                        continue;
                    }
                    return;
                }'''
    if old not in block:
        raise SystemExit("advance terminal promotion block not found")
    return block.replace(old, new, 1)

text = rewrite_between(text, "    async fn advance(", "    /// Drain queued steers", "advance same-lane promotion", rewrite_advance)

# The old helper hard-coded main and is no longer needed.
old_stage = '''    fn stage_entry(&mut self, entry: &SessionEntry) -> EntryRecord {
        EntryRecord::provision(self.next_entry_seq, entry.clone()).after(self.main_leaf())
    }

'''
if old_stage not in text:
    raise SystemExit("main-only stage_entry helper not found")
text = text.replace(old_stage, "", 1)

# Fix explicit main start calls inside generic promotion replacement if global
# caller rewrite touched the newly generated block.
text = text.replace(
    "self.start_active(crate::session::lane::MAIN, active);\n                Some(operation_id)",
    "self.start_active(lane_name, active);\n                Some(operation_id)",
)

runtime.write_text(text)

# ---------------------------------------------------------------------------
# Recovery: every resident open operation is recovered independently.
# ---------------------------------------------------------------------------
text = recovery.read_text()
old_head = '''        let Some(state) = self
            .main_active()
            .map(|active| active.machine.state().clone())
        else {
            return;
        };
        match state {'''
new_head = '''        let operation_ids = self.operations.keys().copied().collect::<Vec<_>>();
        for operation_id in operation_ids {
            if self.closed {
                return;
            }
            let Some(state) = self
                .active(operation_id)
                .map(|active| active.machine.state().clone())
            else {
                continue;
            };
            match state {'''
text = replace_once(text, old_head, new_head, "recovery loop head")
# Add the for-loop close before the existing function/impl closes.
old_tail = '''            OperationState::Finished(_) => {}
        }
    }
}'''
new_tail = '''            OperationState::Finished(_) => {}
            }
        }
    }
}'''
text = replace_once(text, old_tail, new_tail, "recovery loop tail")

# Stable operation identity is now the recovery selector everywhere.
text = text.replace("self.main_active()", "self.active(operation_id)")
text = text.replace("self.main_live_mut()", "self.live_mut(operation_id)")
text = text.replace("self.main_live()", "self.live(operation_id)")
text = text.replace(
    "self.fail_operation_on_persistence(err).await;",
    "self.fail_operation_on_persistence_for(operation_id, err).await;",
)
text = text.replace(
    "self.emit_terminal_state(&applied.state);",
    "self.emit_terminal_state_for(operation_id, &applied.state);",
)
text = text.replace("self.remove_main_operation();", "self.remove_operation(operation_id);")
# Recovery continuations no longer rediscover main.
text = text.replace(
    '''if let Some(operation_id) = self.main_resident_id() {
                            self.advance(operation_id).await;
                        }''',
    '''self.advance(operation_id).await;''',
)
text = text.replace(
    '''if let Some(operation_id) = self.main_resident_id() {
                    self.advance(operation_id).await;
                }''',
    '''self.advance(operation_id).await;''',
)

# Exact persistence failure routing should include formatting variants.
text = text.replace(
    '''self.fail_operation_on_persistence(err)
                        .await;''',
    '''self.fail_operation_on_persistence_for(operation_id, err)
                        .await;''',
)

for forbidden in (
    "main_active()",
    "main_live_mut()",
    "main_live()",
    "main_resident_id()",
    "remove_main_operation()",
    "fail_operation_on_persistence(err)",
    "emit_terminal_state(&applied.state)",
):
    if forbidden in text:
        raise SystemExit(f"recovery still contains main-only path: {forbidden}")
recovery.write_text(text)

# ---------------------------------------------------------------------------
# Guardrails: generic substrate should be exercised by main paths now, while no
# public non-main command has been introduced prematurely.
# ---------------------------------------------------------------------------
rt = runtime.read_text()
for required in (
    "accept_operation_record(crate::session::lane::MAIN, prompt, None)",
    "async fn accept_operation_record(\n        &mut self,\n        lane_name: &str,",
    "fn start_active(&mut self, lane_name: &str, active: ActiveOperation)",
    "async fn promote_pending_next_run(&mut self, lane_name: &str) -> Option<OperationId>",
):
    if required not in rt:
        raise SystemExit(f"missing generic lane substrate: {required}")
