//! Relationship facts are part of Session history, not another task ledger.
use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{
    DelegationOutcome, DelegationOwnership, DelegationReceipt, DelegationRecord, DelegationSite,
    DelegationSpec, DelegationTarget, delegation::ChildMaterialization,
};

#[derive(Clone, Copy)]
pub(super) struct DelegationLink {
    pub run: uuid::Uuid,
    pub child: uuid::Uuid,
    pub ownership: DelegationOwnership,
    pub site: DelegationSite,
}

#[derive(Default, Clone)]
pub(super) struct PendingDelegations {
    active: BTreeMap<Option<usize>, DelegationLink>,
    finished: BTreeSet<Option<usize>>,
}

impl PendingDelegations {
    pub(super) fn unresolved(&self, child: Option<usize>) -> bool {
        self.active.contains_key(&child)
    }

    pub(super) fn empty(&self) -> bool {
        self.active.is_empty()
    }

    pub(super) fn ever_admitted(&self) -> bool {
        !self.active.is_empty() || !self.finished.is_empty()
    }

    pub(super) fn unknowns(&self) -> impl Iterator<Item = SessionEntry> + '_ {
        self.active
            .values()
            .map(|link| SessionEntry::DelegationSettled {
                run: link.run,
                outcome: DelegationOutcome::Unknown,
            })
    }
}

impl State {
    pub(super) fn apply_delegation(&mut self, entry: &SessionEntry) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ChildSessionOf { record } => {
                let boundary = match &record.spec.target {
                    DelegationTarget::Fresh { .. } => self.sequence == 0,
                    DelegationTarget::Fork {
                        point: ForkPoint::BeforeTurn(turn),
                        ..
                    } => self.last_id.checked_add(1) == Some(*turn),
                    DelegationTarget::Fork {
                        point: ForkPoint::AfterTurn(turn),
                        ..
                    } => self.last_id == *turn,
                    DelegationTarget::Resume { .. } => true,
                };
                if !boundary
                    || self.active.is_some()
                    || self.pending_shell.is_some()
                    || !valid_record(record)
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::DelegationAdmitted { record } => {
                if self.active != Some(record.site.turn) || !valid_record(record) {
                    return Err(SessionError::InvalidHistory);
                }
                let parent = self
                    .pending
                    .iter_mut()
                    .find(|pending| pending.occurrence == record.site.tool)
                    .ok_or(SessionError::InvalidHistory)?;
                let key = record.site.composed_child;
                if key.is_some_and(|child| !parent.children.contains(&child))
                    || parent.delegations.active.contains_key(&key)
                    || parent.delegations.finished.contains(&key)
                {
                    return Err(SessionError::InvalidHistory);
                }
                parent.delegations.active.insert(
                    key,
                    DelegationLink {
                        run: record.run,
                        child: record.child,
                        ownership: record.ownership,
                        site: record.site,
                    },
                );
            }
            SessionEntry::DelegationTransferred { run, owner } => {
                let (parent, key) = pending_site(&mut self.pending, *run)?;
                let link = parent.delegations.active[&key];
                if link.ownership != (DelegationOwnership::Background { owner: *owner }) {
                    return Err(SessionError::InvalidHistory);
                }
                parent.delegations.active.remove(&key);
                parent.delegations.finished.insert(key);
                if self.backgrounds.insert(*run, link).is_some() {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::DelegationSettled { run, outcome } => {
                if let DelegationOutcome::Joined {
                    execution_timed_out,
                    receipt,
                    error,
                    answer,
                    partial_output,
                    ..
                } = outcome
                    && ((partial_output.as_ref().is_some_and(|output| {
                        !output.valid() || error.is_none() || answer.is_some()
                    })) || (*execution_timed_out && error.is_none())
                        || (answer.is_some()
                            && (*execution_timed_out
                                || error.is_some()
                                || receipt.as_ref().is_none_or(|receipt| {
                                    receipt.end != Some(TurnEndReason::Completed)
                                }))))
                {
                    return Err(SessionError::InvalidHistory);
                }
                let link = if let Some(link) = self.backgrounds.remove(run) {
                    link
                } else {
                    let (parent, key) = pending_site(&mut self.pending, *run)?;
                    parent.delegations.finished.insert(key);
                    parent
                        .delegations
                        .active
                        .remove(&key)
                        .expect("pending_site found this run")
                };
                if let DelegationOutcome::Joined {
                    receipt: Some(receipt),
                    ..
                } = outcome
                    && (receipt.session != link.child
                        || receipt.turn == 0
                        || receipt.key != crate::delegation::request_key(*run))
                {
                    return Err(SessionError::InvalidHistory);
                }
            }
            _ => unreachable!("delegation variants only"),
        }
        Ok(())
    }
}

fn pending_site(
    pending: &mut [PendingTool],
    run: uuid::Uuid,
) -> Result<(&mut PendingTool, Option<usize>), SessionError> {
    pending
        .iter_mut()
        .find_map(|pending| {
            let key = pending
                .delegations
                .active
                .iter()
                .find_map(|(key, link)| (link.run == run).then_some(*key))?;
            Some((pending, key))
        })
        .ok_or(SessionError::InvalidHistory)
}

fn valid_record(record: &DelegationRecord) -> bool {
    !record.run.is_nil()
        && !record.child.is_nil()
        && !record.site.session.is_nil()
        && record.run != record.child
        && record.child != record.site.session
        && record.site.turn != 0
        && record.site.tool.assistant_entry != 0
        && record.spec.valid()
        && !matches!(record.ownership, DelegationOwnership::Background { owner } if owner.is_nil())
        && record.path == record.spec.target.path(record.child)
        && !matches!(&record.spec.target, DelegationTarget::Resume { session, .. } if *session != record.child)
}

impl Session {
    pub(crate) fn admit_delegation(
        &self,
        site: DelegationSite,
        spec: DelegationSpec,
        ownership: DelegationOwnership,
    ) -> Result<(DelegationRecord, ChildMaterialization), SessionError> {
        if site.session != self.id() {
            return Err(SessionError::InvalidHistory);
        }
        // Acquire a resume writer before intent. Failed locators, live writers,
        // workspace mismatches and unfinished history are ordinary refusal, not
        // corruption of this parent's admission ledger. No target facts change.
        let resumed = if let DelegationTarget::Resume { path, session } = &spec.target {
            let prepare = || {
                if *session == self.id() {
                    return Err("cannot resume the active parent Session as its child".to_owned());
                }
                let child = Self::open(path).map_err(|error| error.to_string())?;
                if child.id() != *session || child.cwd() != spec.cwd.as_deref() {
                    return Err(
                        "resume target identity or workspace changed; use its original workspace"
                            .into(),
                    );
                }
                let store = child
                    .store
                    .lock()
                    .map_err(|_| SessionError::Poisoned.to_string())?;
                if store.state.active.is_some()
                    || store.state.pending_shell.is_some()
                    || store
                        .state
                        .backgrounds
                        .values()
                        .any(|link| link.site.session == child.id())
                {
                    return Err("resume target has unfinished work; reconcile it explicitly before a new request".into());
                }
                drop(store);
                Ok(Arc::new(child))
            };
            Some(prepare().map_err(SessionError::DelegationTargetUnavailable)?)
        } else {
            None
        };
        let child = resumed
            .as_ref()
            .map_or_else(uuid::Uuid::new_v4, |child| child.id());
        let record = DelegationRecord {
            run: uuid::Uuid::new_v4(),
            site,
            child,
            path: spec.target.path(child),
            ownership,
            spec,
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let context = match &record.spec.target {
            DelegationTarget::Fresh { .. } | DelegationTarget::Resume { .. } => Vec::new(),
            DelegationTarget::Fork { point, .. } => {
                without_delivery_keys(fork_prefix(&store.connection, *point)?.0)
            }
        };
        let projection = project(context.iter().map(Ok), ProjectionMode::Metadata, &|entry| {
            Ok(context[(entry - 1) as usize].clone())
        })?;
        if projection.state.active.is_some() || projection.state.pending_shell.is_some() {
            return Err(SessionError::InvalidForkPoint);
        }
        store.append(&[SessionEntry::DelegationAdmitted {
            record: record.clone(),
        }])?;
        Ok((
            record,
            match resumed {
                Some(child) => ChildMaterialization::Resume(child),
                None => ChildMaterialization::New(context),
            },
        ))
    }

    pub(crate) fn transfer_delegation(
        &self,
        record: &DelegationRecord,
        lease: &Arc<()>,
    ) -> Result<(), SessionError> {
        let DelegationOwnership::Background { owner } = record.ownership else {
            return Err(SessionError::InvalidHistory);
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        store.append(&[SessionEntry::DelegationTransferred {
            run: record.run,
            owner,
        }])?;
        store
            .live_backgrounds
            .insert(record.run, Arc::downgrade(lease));
        Ok(())
    }

    pub(crate) fn settle_delegation(
        &self,
        record: &DelegationRecord,
        outcome: DelegationOutcome,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        store.append(&[SessionEntry::DelegationSettled {
            run: record.run,
            outcome,
        }])
    }

    /// Only the retained writer can materialize a resume. A new relationship
    /// records a new request binding, never authority reconstructed from ancestry.
    pub(crate) fn materialize_delegation(
        record: &DelegationRecord,
        prepared: ChildMaterialization,
    ) -> Result<Arc<Self>, SessionError> {
        let binding = [
            SessionEntry::ChildSessionOf {
                record: record.clone(),
            },
            SessionEntry::ModelSelected {
                model: record.spec.model.clone(),
            },
            SessionEntry::ReasoningSelected {
                reasoning: record.spec.reasoning,
            },
        ];
        match prepared {
            ChildMaterialization::Resume(child) => {
                if !matches!(record.spec.target, DelegationTarget::Resume { .. })
                    || child.id() != record.child
                    || child.cwd() != record.spec.cwd.as_deref()
                {
                    return Err(SessionError::InvalidHistory);
                }
                let mut store = child.store.lock().map_err(|_| SessionError::Poisoned)?;
                store.append(&binding)?;
                drop(store);
                Ok(child)
            }
            ChildMaterialization::New(mut context) => {
                if matches!(record.spec.target, DelegationTarget::Resume { .. }) {
                    return Err(SessionError::InvalidHistory);
                }
                context.extend(binding);
                let child = if record.path.try_exists()? {
                    Self::open(&record.path)?
                } else {
                    Self::create_initial(
                        &record.path,
                        record.spec.cwd.as_deref(),
                        record.child,
                        &context,
                    )?
                };
                let view = child.view()?;
                if child.id() != record.child
                    || child.cwd() != record.spec.cwd.as_deref()
                    || view.parent_delegation() != Some(record)
                    || !view.entries.starts_with(&context)
                {
                    return Err(SessionError::InvalidHistory);
                }
                Ok(Arc::new(child))
            }
        }
    }
}

impl SessionView {
    /// Each run keeps its own report even when several reuse one conversation.
    /// Context copies retain history, not ownership of the source's relationships.
    pub fn delegations(&self) -> Vec<DelegationReceipt> {
        let mut receipts: Vec<DelegationReceipt> = Vec::new();
        let mut positions = BTreeMap::new();
        for entry in &self.entries {
            match entry {
                SessionEntry::DelegationAdmitted { record } if record.site.session == self.id => {
                    positions.insert(record.run, receipts.len());
                    receipts.push(DelegationReceipt {
                        record: record.clone(),
                        transferred: false,
                        outcome: None,
                    });
                }
                SessionEntry::DelegationTransferred { run, .. } => {
                    if let Some(&index) = positions.get(run) {
                        receipts[index].transferred = true;
                    }
                }
                SessionEntry::DelegationSettled { run, outcome } => {
                    if let Some(&index) = positions.get(run) {
                        receipts[index].outcome = Some(outcome.clone());
                    }
                }
                _ => {}
            }
        }
        receipts
    }

    /// Latest managed request binding of this Session, not an inherited copy's
    /// parentage or a grant to continue any earlier execution.
    pub fn parent_delegation(&self) -> Option<&DelegationRecord> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::ChildSessionOf { record } if record.child == self.id => Some(record),
            _ => None,
        })
    }
}
