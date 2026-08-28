from pathlib import Path
import re


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new))


def regex_once(path: str, pattern: str, replacement: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(new)


path = "crates/ion-core/src/runtime/mod.rs"
replace_once(
    path,
    "use std::fmt;\n",
    "use std::collections::{BTreeMap, HashMap};\nuse std::fmt;\n",
    "runtime collection imports",
)
replace_once(
    path,
    "use crate::ids::{EffectId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId};",
    "use crate::ids::{EntryId, EffectId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId};",
    "EntryId import",
)
replace_once(
    path,
    '''    provider: Arc<P>,
    /// Live projection of the hidden `main` lane's total model config.
    /// Durable lane config is authoritative; an in-flight model step keeps
    /// its frozen [`ModelConfig`].
    selected_model_ref: String,
    tools: Arc<ToolCatalog>,
''',
    '''    provider: Arc<P>,
    tools: Arc<ToolCatalog>,
''',
    "remove selected model mirror",
)
replace_once(
    path,
    '''    cursor: RuntimeCursor,
    /// Canonical semantic records, mirroring the durable store.
    entries: Vec<EntryRecord>,
    /// Next storage-assigned entry sequence.
    next_entry_seq: u64,
    operation: Option<ActiveOperation>,
    /// Durable input reserved for the next run while `main` is busy.
    /// No operation machine exists until the lane becomes idle.
    pending_next_run: Option<crate::session::lane::NextRun>,
''',
    '''    cursor: RuntimeCursor,
    /// Full canonical conversation tree in global durable sequence order.
    tree_entries: Vec<EntryRecord>,
    /// Derived lookup index for walking parent-linked lane branches without
    /// turning context projection into an O(n²) scan. `tree_entries` remains
    /// the authority; this index is rebuilt on reopen and extended on commit.
    entry_index: HashMap<EntryId, usize>,
    /// Durable total lane projections. Public commands still address `main`
    /// until the next multi-lane residency checkpoint.
    lanes: BTreeMap<String, crate::session::lane::Lane>,
    /// Next session-global durable entry sequence.
    next_entry_seq: u64,
    operation: Option<ActiveOperation>,
''',
    "runtime tree and lane fields",
)
replace_once(
    path,
    '''        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut runtime = Self {
''',
    '''        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut lanes = BTreeMap::new();
        lanes.insert(
            crate::session::lane::MAIN.to_owned(),
            crate::session::lane::Lane {
                name: crate::session::lane::MAIN.to_owned(),
                state: crate::session::lane::State {
                    leaf: None,
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new(initial_model_ref),
            },
        );
        let mut runtime = Self {
''',
    "initialize main lane",
)
replace_once(
    path,
    '''            cwd,
            provider,
            selected_model_ref: initial_model_ref,
            tools,
''',
    '''            cwd,
            provider,
            tools,
''',
    "runtime init remove model mirror",
)
replace_once(
    path,
    '''            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            entries: Vec::new(),
            next_entry_seq: 1,
            operation: None,
            pending_next_run: None,
''',
    '''            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            tree_entries: Vec::new(),
            entry_index: HashMap::new(),
            lanes,
            next_entry_seq: 1,
            operation: None,
''',
    "runtime init tree lanes",
)

regex_once(
    path,
    r'''        let assistant_frames = loaded\.assistant_frames;\n        let Some\(main_lane\) = loaded.*?        self\.next_entry_seq = max_seq \+ 1;\n\n        for operation in loaded\.operations \{''',
    '''        let assistant_frames = loaded.assistant_frames;
        let max_seq = loaded
            .entries
            .iter()
            .map(|record| record.seq)
            .max()
            .unwrap_or(0);
        self.tree_entries = loaded.entries;
        self.entry_index = self
            .tree_entries
            .iter()
            .enumerate()
            .map(|(index, record)| (record.id, index))
            .collect();
        self.lanes = loaded
            .lanes
            .into_iter()
            .map(|lane| (lane.name.clone(), lane))
            .collect();
        if !self.lanes.contains_key(crate::session::lane::MAIN) {
            error!(session = %self.session_id, "reopened session has no main lane; fencing");
            self.closed = true;
            return;
        }
        let Some(main_branch) = self.lane_branch_records(crate::session::lane::MAIN) else {
            error!(session = %self.session_id, "main lane branch is incomplete; fencing");
            self.closed = true;
            return;
        };
        self.reopen_entry_count = Some(main_branch.len());
        self.next_entry_seq = max_seq + 1;

        for operation in loaded.operations {''',
    "restore full tree and lanes",
)

insert_before_run = '''    async fn run(mut self) {'''
helpers = '''    fn main_lane(&self) -> &crate::session::lane::Lane {
        self.lanes
            .get(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {
        self.lanes
            .get_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn lane_branch_records(&self, lane_name: &str) -> Option<Vec<&EntryRecord>> {
        let lane = self.lanes.get(lane_name)?;
        let mut branch = Vec::new();
        let mut cursor = lane.state.leaf;
        while let Some(entry_id) = cursor {
            let index = *self.entry_index.get(&entry_id)?;
            let record = self.tree_entries.get(index)?;
            branch.push(record);
            cursor = record.parent;
        }
        branch.reverse();
        Some(branch)
    }

    fn main_branch_records(&self) -> Vec<&EntryRecord> {
        self.lane_branch_records(crate::session::lane::MAIN)
            .expect("live main lane branch is complete")
    }

    fn main_leaf(&self) -> Option<EntryId> {
        self.main_lane().state.leaf
    }

    fn main_model_ref(&self) -> &str {
        &self.main_lane().config.model_ref
    }

    fn main_pending_next_run(&self) -> Option<&crate::session::lane::NextRun> {
        self.main_lane().state.pending_next_run.as_ref()
    }

    fn install_tree_entries(&mut self, entries: Vec<EntryRecord>) {
        for record in entries {
            let index = self.tree_entries.len();
            let previous = self.entry_index.insert(record.id, index);
            debug_assert!(previous.is_none(), "entry identity installed twice");
            self.tree_entries.push(record);
        }
    }

'''
replace_once(path, insert_before_run, helpers + insert_before_run, "runtime tree helpers")

replace_once(
    path,
    '''                initial_model_ref: self.selected_model_ref.clone(),
''',
    '''                initial_model_ref: self.main_model_ref().to_owned(),
''',
    "new session model projection",
)
replace_once(
    path,
    '''            && self.pending_next_run.is_some()
''',
    '''            && self.main_pending_next_run().is_some()
''',
    "startup pending next run",
)
replace_once(
    path,
    '''        if let Some(pending) = &self.pending_next_run {
''',
    '''        if let Some(pending) = self.main_pending_next_run() {
''',
    "submit pending lookup",
)
replace_once(
    path,
    '''        if let Some(pending) = &self.pending_next_run {
''',
    '''        if let Some(pending) = self.main_pending_next_run() {
''',
    "next-run pending lookup",
)
replace_once(
    path,
    '''        self.pending_next_run = Some(next_run);
''',
    '''        self.main_lane_mut().state.pending_next_run = Some(next_run);
''',
    "install pending next run",
)
replace_once(
    path,
    '''                parent: self.entries.last().map(|record| record.id),
''',
    '''                parent: self.main_leaf(),
''',
    "reserved entry parent",
)
replace_once(
    path,
    '''        self.entries.push(entry);
        self.next_entry_seq += 1;
        if reservation.is_some() {
            self.pending_next_run = None;
        }
''',
    '''        let entry_leaf = entry.id;
        self.install_tree_entries(vec![entry]);
        self.next_entry_seq += 1;
        let lane = self.main_lane_mut();
        lane.state.leaf = Some(entry_leaf);
        lane.state.current_operation = Some(operation_id);
        if reservation.is_some() {
            lane.state.pending_next_run = None;
        }
''',
    "install accepted entry in tree",
)
replace_once(
    path,
    '''        let Some(next_run) = self.pending_next_run.clone() else {
''',
    '''        let Some(next_run) = self.main_pending_next_run().cloned() else {
''',
    "promote main pending input",
)
replace_once(
    path,
    '''        let previous = self.selected_model_ref.clone();
''',
    '''        let previous = self.main_model_ref().to_owned();
''',
    "switch model previous",
)
replace_once(
    path,
    '''        self.selected_model_ref = model_ref;
''',
    '''        self.main_lane_mut().config.model_ref = model_ref;
''',
    "switch model install config",
)

replace_once(
    path,
    '''        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;

        self.next_entry_seq = new_entry_seq;
''',
    '''        self.commit_transition(request)
            .await
            .map_err(persistence_command_error)?;

        self.next_entry_seq = new_entry_seq;
''',
    "steer uses canonical commit",
)
replace_once(
    path,
    '''        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;
        self.next_entry_seq = new_entry_seq;
''',
    '''        self.commit_transition(request)
            .await
            .map_err(persistence_command_error)?;
        self.next_entry_seq = new_entry_seq;
''',
    "cancel uses canonical commit",
)

replace_once(
    path,
    '''    fn first_entry_seq(&self) -> u64 {
        self.entries
            .first()
            .map_or(self.next_entry_seq, |record| record.seq)
    }
''',
    '''    fn first_entry_seq(&self) -> u64 {
        self.main_branch_records()
            .first()
            .map_or(self.next_entry_seq, |record| record.seq)
    }
''',
    "main branch first seq",
)

regex_once(
    path,
    r'''    async fn current_model_config\(&mut self\) -> ModelConfig \{.*?\n    \}\n\n    fn current_context_manifest''',
    '''    async fn current_model_config(&mut self) -> ModelConfig {
        let selected_model_ref = self.main_model_ref().to_owned();
        if self
            .model_capabilities
            .as_ref()
            .is_none_or(|(model_ref, _)| model_ref != &selected_model_ref)
        {
            let capabilities = self.provider.capabilities_for(&selected_model_ref).await;
            self.model_capabilities = Some((selected_model_ref.clone(), capabilities));
        }
        if self.context_window.is_none() {
            self.context_window = self.provider.context_window_for(&selected_model_ref).await;
        }
        ModelConfig {
            model_ref: selected_model_ref,
            context_window: self.context_window,
            capabilities: self
                .model_capabilities
                .as_ref()
                .expect("model capabilities cached")
                .1,
        }
    }

    fn current_context_manifest''',
    "model config reads main lane",
)
replace_once(
    path,
    '''        project_with_manifest(
            self.entries.iter().map(|record| &record.entry),
            self.first_entry_seq(),
            manifest,
        )
''',
    '''        let branch = self.main_branch_records();
        project_with_manifest(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            manifest,
        )
''',
    "model plan projects main branch",
)
replace_once(
    path,
    '''        let mut plan = project_with_manifest(
            self.entries.iter().map(|record| &record.entry),
            self.first_entry_seq(),
            &manifest,
        );
''',
    '''        let branch = self.main_branch_records();
        let mut plan = project_with_manifest(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            &manifest,
        );
''',
    "compaction projects main branch",
)

replace_once(
    path,
    '''    async fn commit_transition(&mut self, mut request: CommitRequest) -> Result<(), StoreError> {
        let mut parent = self.entries.last().map(|record| record.id);
        for entry in &mut request.entries {
            entry.parent = parent;
            parent = Some(entry.id);
        }
        let entries = request.entries.clone();
        self.store.commit(request).await?;
        self.entries.extend(entries);
        Ok(())
    }

    fn stage_entry(&mut self, entry: &SessionEntry) -> EntryRecord {
        EntryRecord::provision(self.next_entry_seq, entry.clone())
            .after(self.entries.last().map(|record| record.id))
    }
''',
    '''    async fn commit_transition(&mut self, mut request: CommitRequest) -> Result<(), StoreError> {
        let operation_id = request.operation_id;
        let terminal = matches!(request.checkpoint.payload.state, OperationState::Finished(_));
        let mut parent = self.main_leaf();
        for entry in &mut request.entries {
            entry.parent = parent;
            parent = Some(entry.id);
        }
        let entries = request.entries.clone();
        let new_leaf = entries.last().map(|entry| entry.id);
        self.store.commit(request).await?;
        self.install_tree_entries(entries);
        let lane = self.main_lane_mut();
        if let Some(new_leaf) = new_leaf {
            lane.state.leaf = Some(new_leaf);
        }
        lane.state.current_operation = if terminal { None } else { Some(operation_id) };
        Ok(())
    }

    fn stage_entry(&mut self, entry: &SessionEntry) -> EntryRecord {
        EntryRecord::provision(self.next_entry_seq, entry.clone()).after(self.main_leaf())
    }
''',
    "tree-aware commit transition",
)
replace_once(
    path,
    '''            entries: self
                .entries
                .iter()
                .map(|record| record.entry.clone())
                .collect(),
            model_ref: self.selected_model_ref.clone(),
''',
    '''            entries: self
                .main_branch_records()
                .iter()
                .map(|record| record.entry.clone())
                .collect(),
            model_ref: self.main_model_ref().to_owned(),
''',
    "snapshot projects main branch",
)

# The one remaining branch projection in the effect settlement belongs to main
# until operation residency itself becomes lane-addressable.
effects = "crates/ion-core/src/runtime/effects.rs"
replace_once(
    effects,
    '''        let mut plan = project_with_manifest(
            self.entries.iter().map(|record| &record.entry),
            self.first_entry_seq(),
            &manifest,
        );
''',
    '''        let branch = self.main_branch_records();
        let mut plan = project_with_manifest(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            &manifest,
        );
''',
    "overflow compaction projects main branch",
)
