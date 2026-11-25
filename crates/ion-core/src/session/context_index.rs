//! Persistent ancestry and sparse selected-context metadata; no raw payload cache.
use super::*;

#[derive(Default)]
pub(super) struct ContextIndex {
    nodes: Vec<Node>,
    contexts: Vec<ContextRefs>,
    head: u64,
    pub(super) selection: u64,
    pub(super) prompt_recall: super::prompt_recall::PromptRecall,
    pub(super) usage: crate::UsageTotals,
    display_dependents: BTreeMap<u64, Vec<DisplayDependent>>,
    tool_batches: BTreeMap<u64, ToolBatch>,
    pending: usize,
    shell: bool,
    admitted: BTreeMap<uuid::Uuid, crate::DelegationSite>,
    backgrounds: BTreeMap<uuid::Uuid, crate::DelegationSite>,
}

struct Node {
    parent: u64,
    transcript_parent: u64,
    display_parent: u64,
    tool_head: u64,
    context: usize,
    transcript_fact: bool,
    transcript_boundary: bool,
    display_root: bool,
    projection: ProjectionKind,
    selectable: bool,
    settled: bool,
}

pub(super) struct DisplayDependent {
    pub(super) entry: u64,
    pub(super) ordinal: Option<usize>,
}

#[derive(Clone)]
pub(super) struct ToolBatch {
    pub(super) previous: u64,
    pub(super) first: usize,
    // Zero denotes an admitted call without a committed terminal result.
    pub(super) results: Vec<u64>,
}

#[derive(Clone, Copy)]
enum ProjectionKind {
    Message,
    ModelContext,
    Other,
}

/// Sparse persistent metadata: adjacent facts share a snapshot of coordinates,
/// never copies of the original model definitions, summaries or tool payloads.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ContextRefs {
    model: u64,
    reasoning: u64,
    effective: u64,
    context: u64,
    compaction: u64,
    assistant: u64,
}

impl ContextIndex {
    pub(super) fn parent(&self, entry: u64) -> u64 {
        self.nodes[(entry - 1) as usize].parent
    }

    pub(super) fn head(&self) -> u64 {
        self.head
    }
    pub(super) fn can_select(&self, head: u64) -> bool {
        head == 0
            || self
                .nodes
                .get((head - 1) as usize)
                .is_some_and(|node| node.selectable)
    }

    pub(super) fn path(&self, mut head: u64) -> Vec<u64> {
        let mut path = Vec::new();
        while head != 0 {
            path.push(head);
            head = self.nodes[(head - 1) as usize].parent;
        }
        path.reverse();
        path
    }

    pub(super) fn transcript_head(&self, head: u64) -> u64 {
        if head == 0 {
            return 0;
        }
        let node = &self.nodes[(head - 1) as usize];
        if node.transcript_fact {
            head
        } else {
            node.transcript_parent
        }
    }

    pub(super) fn transcript_checkpoint(&self) -> Option<u64> {
        let head = self.transcript_head(self.head);
        self.is_transcript_boundary(head).then_some(head)
    }

    pub(super) fn is_transcript_boundary(&self, entry: u64) -> bool {
        entry == 0 || self.nodes[(entry - 1) as usize].transcript_boundary
    }

    pub(super) fn tools_through(&self, entry: u64) -> usize {
        self.tool_count(self.tool_ancestor(entry))
    }

    pub(super) fn transcript_after(&self, boundary: u64) -> Option<Vec<u64>> {
        let mut head = self.transcript_head(self.head);
        let mut path = Vec::new();
        while head > boundary {
            path.push(head);
            head = self.transcript_parent(head);
        }
        if head != boundary {
            return None;
        }
        path.reverse();
        Some(path)
    }

    pub(super) fn transcript_parent(&self, entry: u64) -> u64 {
        self.nodes[(entry - 1) as usize].transcript_parent
    }

    pub(super) fn display_bundle(&self, entry: u64, cut: u64) -> &[DisplayDependent] {
        let dependents = self
            .display_dependents
            .get(&entry)
            .map_or(&[][..], Vec::as_slice);
        &dependents[..dependents.partition_point(|fact| fact.entry <= cut)]
    }

    fn tool_ancestor(&self, entry: u64) -> u64 {
        if entry == 0 {
            0
        } else {
            self.nodes[(entry - 1) as usize].tool_head
        }
    }

    pub(super) fn tool_head(&self) -> u64 {
        self.tool_ancestor(self.head)
    }

    pub(super) fn tool_count(&self, head: u64) -> usize {
        self.tool_batches
            .get(&head)
            .map_or(0, |batch| batch.first - 1 + batch.results.len())
    }

    pub(super) fn first_tool_number(&self, assistant: u64) -> Option<usize> {
        self.tool_batches.get(&assistant).map(|batch| batch.first)
    }

    pub(super) fn tool_batch(&self, head: u64) -> &ToolBatch {
        &self.tool_batches[&head]
    }

    pub(super) fn tool_at(&self, mut head: u64, number: usize) -> Option<crate::ToolOccurrence> {
        while head != 0 && number != 0 {
            let batch = &self.tool_batches[&head];
            if let Some(ordinal) = number.checked_sub(batch.first) {
                return (ordinal < batch.results.len()).then_some(crate::ToolOccurrence {
                    assistant_entry: head,
                    ordinal,
                });
            }
            head = batch.previous;
        }
        None
    }

    pub(super) fn tool_number(
        &self,
        mut head: u64,
        occurrence: crate::ToolOccurrence,
    ) -> Option<usize> {
        while head > occurrence.assistant_entry {
            head = self.tool_batches[&head].previous;
        }
        let batch = self.tool_batches.get(&head)?;
        (head == occurrence.assistant_entry && occurrence.ordinal < batch.results.len())
            .then(|| batch.first + occurrence.ordinal)
    }

    pub(super) fn display_head(&self) -> u64 {
        self.display_ancestor(self.head)
    }

    fn display_ancestor(&self, entry: u64) -> u64 {
        if entry == 0 {
            return 0;
        }
        let node = &self.nodes[(entry - 1) as usize];
        if node.display_root {
            entry
        } else {
            node.display_parent
        }
    }

    pub(super) fn display_parent(&self, entry: u64) -> u64 {
        self.nodes[(entry - 1) as usize].display_parent
    }

    // Branch navigation cannot select a point with pending tool or shell
    // effects. Thus a selected admission with a dependent below the cut
    // necessarily selects that dependent too.
    pub(super) fn link_display(
        &mut self,
        entry: &SessionEntry,
        resolved: Option<crate::ToolOccurrence>,
    ) {
        let (root, ordinal) = match entry {
            SessionEntry::ToolResult { .. } => {
                let tool = resolved.expect("validated tool result");
                let batch = self
                    .tool_batches
                    .get_mut(&tool.assistant_entry)
                    .expect("validated tool admission");
                batch.results[tool.ordinal] = self.nodes.len() as u64;
                (tool.assistant_entry, Some(tool.ordinal))
            }
            SessionEntry::ChildToolAdmitted { intent, .. } => {
                (intent.parent.assistant_entry, Some(intent.parent.ordinal))
            }
            SessionEntry::ChildToolResult { parent, .. } => {
                (parent.assistant_entry, Some(parent.ordinal))
            }
            SessionEntry::UserShellSettled {
                admission_entry, ..
            } => (*admission_entry, None),
            _ => return,
        };
        self.display_dependents
            .entry(root)
            .or_default()
            .push(DisplayDependent {
                entry: self.nodes.len() as u64,
                ordinal,
            });
    }

    pub(super) fn is_settled(&self, target: u64) -> bool {
        if target == 0
            || !self
                .nodes
                .get((target - 1) as usize)
                .is_some_and(|node| node.settled)
        {
            return false;
        }
        let mut head = self.head;
        while head > target {
            head = self.nodes[(head - 1) as usize].parent;
        }
        head == target
    }

    fn context_key(&self, head: u64) -> usize {
        if head == 0 {
            0
        } else {
            self.nodes[(head - 1) as usize].context
        }
    }

    fn context_refs(&self, key: usize) -> ContextRefs {
        if key == 0 {
            ContextRefs::default()
        } else {
            self.contexts[key - 1]
        }
    }

    pub(super) fn read_context(
        &self,
        head: u64,
        read: &impl Fn(u64) -> Result<SessionEntry, SessionError>,
    ) -> Result<State, SessionError> {
        if head > self.nodes.len() as u64 {
            return Err(SessionError::InvalidContextPoint);
        }
        let refs = self.context_refs(self.context_key(head));
        let mut coordinates = [
            refs.model,
            refs.reasoning,
            refs.effective,
            refs.context,
            refs.compaction,
            refs.assistant,
        ];
        coordinates.sort_unstable();
        let mut state = State::default();
        let mut previous = 0;
        for coordinate in coordinates {
            if coordinate != previous {
                state.sequence = coordinate - 1;
                state.apply_context(&read(coordinate)?);
                previous = coordinate;
            }
        }
        Ok(state)
    }

    // Called only after execution validation (and after durable commit for a
    // writer). Detached outcomes retain causal affinity without moving the head.
    pub(super) fn observe(&mut self, entry: &SessionEntry) {
        let coordinate = self.nodes.len() as u64 + 1;
        let mut detached = None;
        let mut settled = false;
        match entry {
            SessionEntry::Assistant { message, .. } => {
                self.pending = message
                    .content
                    .iter()
                    .filter(|part| matches!(part, Content::ToolCall(_)))
                    .count();
                settled = self.pending == 0;
            }
            SessionEntry::ToolResult { .. } => {
                self.pending -= 1;
                settled = self.pending == 0;
            }
            SessionEntry::UserShellAdmitted { .. } => self.shell = true,
            SessionEntry::UserShellSettled { .. } => {
                self.shell = false;
                settled = true;
            }
            SessionEntry::TurnEnded { .. } => settled = true,
            SessionEntry::DelegationAdmitted { record } => {
                self.admitted.insert(record.run, record.site);
            }
            SessionEntry::DelegationTransferred { run, .. } => {
                if let Some(site) = self.admitted.remove(run) {
                    self.backgrounds.insert(*run, site);
                }
            }
            SessionEntry::DelegationSettled { run, .. } => {
                detached = self
                    .backgrounds
                    .remove(run)
                    .map(|site| site.tool.assistant_entry);
                self.admitted.remove(run);
            }
            _ => {}
        }
        let selection = if let SessionEntry::ContextSelected { head_entry } = entry {
            Some(*head_entry)
        } else {
            None
        };
        let parent = detached.or(selection).unwrap_or(self.head);
        let previous = self.tool_ancestor(parent);
        let tool_head = if let SessionEntry::Assistant {
            tool_activities, ..
        } = entry
            && !tool_activities.is_empty()
        {
            self.tool_batches.insert(
                coordinate,
                ToolBatch {
                    previous,
                    first: self.tool_count(previous) + 1,
                    results: vec![0; tool_activities.len()],
                },
            );
            coordinate
        } else {
            previous
        };
        let prior_context = self.context_key(parent);
        let mut refs = self.context_refs(prior_context);
        match entry {
            SessionEntry::ModelSelected { .. } | SessionEntry::TurnStarted { .. } => {
                refs.model = coordinate
            }
            SessionEntry::ReasoningSelected { .. } => refs.reasoning = coordinate,
            SessionEntry::EffectiveModelChanged { .. } => refs.effective = coordinate,
            SessionEntry::ModelContextChanged { .. } => refs.context = coordinate,
            SessionEntry::Compacted { .. } => refs.compaction = coordinate,
            SessionEntry::Assistant { .. } => {
                refs.assistant = coordinate;
                if refs.effective == 0 {
                    refs.effective = coordinate;
                }
            }
            _ => {}
        }
        let context = if refs == self.context_refs(prior_context) {
            prior_context
        } else {
            self.contexts.push(refs);
            self.contexts.len()
        };
        let projection = match entry {
            SessionEntry::TurnStarted { .. }
            | SessionEntry::Steering { .. }
            | SessionEntry::Completion { .. }
            | SessionEntry::Assistant { .. }
            | SessionEntry::ToolResult { .. }
            | SessionEntry::UserShellAdmitted { .. }
            | SessionEntry::UserShellSettled { .. } => ProjectionKind::Message,
            SessionEntry::ModelContextChanged { .. } => ProjectionKind::ModelContext,
            _ => ProjectionKind::Other,
        };
        let transcript_fact = matches!(
            entry,
            SessionEntry::TurnStarted { .. }
                | SessionEntry::Steering { .. }
                | SessionEntry::Completion { .. }
                | SessionEntry::Assistant { .. }
                | SessionEntry::ChildToolAdmitted { .. }
                | SessionEntry::ChildToolResult { .. }
                | SessionEntry::ToolResult { .. }
                | SessionEntry::UserShellAdmitted { .. }
                | SessionEntry::UserShellSettled { .. }
                | SessionEntry::TurnEnded { .. }
        );
        self.prompt_recall.observe(entry);
        self.usage.observe(entry);
        self.nodes.push(Node {
            parent,
            transcript_parent: self.transcript_head(parent),
            display_parent: self.display_ancestor(parent),
            tool_head,
            transcript_fact,
            transcript_boundary: matches!(
                entry,
                SessionEntry::TurnEnded { .. } | SessionEntry::UserShellSettled { .. }
            ),
            display_root: transcript_fact
                && !matches!(
                    entry,
                    SessionEntry::ToolResult { .. }
                        | SessionEntry::ChildToolAdmitted { .. }
                        | SessionEntry::ChildToolResult { .. }
                        | SessionEntry::UserShellSettled { .. }
                        | SessionEntry::TurnEnded {
                            reason: TurnEndReason::Completed,
                            ..
                        }
                ),
            context,
            projection,
            selectable: selection.is_none()
                && detached.is_none()
                && self.pending == 0
                && !self.shell,
            settled,
        });
        if let Some(head) = selection {
            self.selection = coordinate;
            self.head = head;
        } else if detached.is_none() {
            self.head = coordinate;
        }
    }
}

pub(super) struct WorkingContext {
    pub(super) messages: Vec<ProjectedMessage>,
    pub(super) settled: BTreeSet<u64>,
    pub(super) changes: Vec<ModelContextChange>,
}

pub(super) fn working_context(
    index: &ContextIndex,
    head: u64,
    state: &State,
    read: &impl Fn(u64) -> Result<SessionEntry, SessionError>,
) -> Result<WorkingContext, SessionError> {
    let mut shell = None;
    let mut restored = WorkingContext {
        messages: Vec::new(),
        settled: BTreeSet::new(),
        changes: Vec::new(),
    };
    let through = state.compaction.as_ref().map_or(0, |(through, _)| *through);
    let timeline = state.compaction.is_none() && state.replay_epoch_start == 0;
    for coordinate in index.path(head) {
        if coordinate <= through
            && state.compaction_inputs.binary_search(&coordinate).is_err()
            && state.active_inputs.binary_search(&coordinate).is_err()
        {
            continue;
        }
        let node = &index.nodes[(coordinate - 1) as usize];
        if coordinate > through && node.settled {
            restored.settled.insert(coordinate);
        }
        match node.projection {
            ProjectionKind::Other => continue,
            ProjectionKind::ModelContext if !timeline => continue,
            _ => {}
        }
        let entry = read(coordinate)?;
        let closed_shell = match &entry {
            SessionEntry::UserShellAdmitted {
                command,
                exclude_from_context,
            } => {
                shell = Some(PendingShell {
                    admission_entry: coordinate,
                    command: command.clone(),
                    exclude_from_context: *exclude_from_context,
                });
                None
            }
            SessionEntry::UserShellSettled { .. } => {
                Some(shell.take().ok_or(SessionError::InvalidHistory)?)
            }
            _ => None,
        };
        projection::append_message(
            &mut restored.messages,
            coordinate,
            &entry,
            closed_shell.as_ref(),
        );
        if let SessionEntry::ModelContextChanged { context, .. } = &entry {
            restored.changes.push(ModelContextChange {
                after_message: restored.messages.len(),
                context: model_context_state(context),
            });
        }
    }
    Ok(restored)
}
