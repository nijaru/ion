//! Persistent ancestry and sparse selected-context metadata; no raw payload cache.
use super::*;

#[derive(Default)]
pub(super) struct ContextIndex {
    nodes: Vec<Node>,
    contexts: Vec<ContextRefs>,
    head: u64,
    pending: usize,
    shell: bool,
    admitted: BTreeMap<uuid::Uuid, crate::DelegationSite>,
    backgrounds: BTreeMap<uuid::Uuid, crate::DelegationSite>,
}

struct Node {
    parent: u64,
    context: usize,
    projection: ProjectionKind,
    selectable: bool,
    settled: bool,
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
        self.nodes.push(Node {
            parent,
            context,
            projection,
            selectable: selection.is_none()
                && detached.is_none()
                && self.pending == 0
                && !self.shell,
            settled,
        });
        if let Some(head) = selection {
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
