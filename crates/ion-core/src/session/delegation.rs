//! Relationship facts are part of Session history, not another task ledger.
use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{
    DelegationContext, DelegationOutcome, DelegationOwnership, DelegationReceipt, DelegationRecord,
    DelegationSite, DelegationSpec,
};

#[derive(Default, Clone)]
pub(super) struct PendingDelegations {
    active: BTreeMap<Option<usize>, (uuid::Uuid, DelegationOwnership, DelegationSite)>,
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
            .map(|(child, _, _)| SessionEntry::DelegationSettled {
                child: *child,
                outcome: DelegationOutcome::Unknown,
            })
    }
}

impl State {
    pub(super) fn apply_delegation(&mut self, entry: &SessionEntry) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ChildSessionOf { record } => {
                let boundary = match record.spec.context {
                    DelegationContext::Fresh => self.sequence == 0,
                    DelegationContext::Fork {
                        point: ForkPoint::BeforeTurn(turn),
                    } => self.last_id.checked_add(1) == Some(turn),
                    DelegationContext::Fork {
                        point: ForkPoint::AfterTurn(turn),
                    } => self.last_id == turn,
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
                parent
                    .delegations
                    .active
                    .insert(key, (record.child, record.ownership, record.site));
            }
            SessionEntry::DelegationTransferred { child, owner } => {
                let (parent, key) = pending_site(&mut self.pending, *child)?;
                let (_, ownership, site) = parent.delegations.active[&key];
                if ownership != (DelegationOwnership::Background { owner: *owner }) {
                    return Err(SessionError::InvalidHistory);
                }
                parent.delegations.active.remove(&key);
                parent.delegations.finished.insert(key);
                if self.backgrounds.insert(*child, site).is_some() {
                    return Err(SessionError::InvalidHistory);
                }
            }
            SessionEntry::DelegationSettled { child, outcome } => {
                if let DelegationOutcome::Joined {
                    receipt,
                    error,
                    answer: Some(_),
                } = outcome
                    && (error.is_some()
                        || receipt
                            .as_ref()
                            .is_none_or(|receipt| receipt.end != Some(TurnEndReason::Completed)))
                {
                    return Err(SessionError::InvalidHistory);
                }
                if let DelegationOutcome::Joined {
                    receipt: Some(receipt),
                    ..
                } = outcome
                    && (receipt.session != *child
                        || receipt.turn == 0
                        || receipt.key != crate::delegation::request_key(*child))
                {
                    return Err(SessionError::InvalidHistory);
                }
                if self.backgrounds.remove(child).is_none() {
                    let (parent, key) = pending_site(&mut self.pending, *child)?;
                    parent.delegations.active.remove(&key);
                    parent.delegations.finished.insert(key);
                }
            }
            _ => unreachable!("delegation variants only"),
        }
        Ok(())
    }
}

fn pending_site(
    pending: &mut [PendingTool],
    child: uuid::Uuid,
) -> Result<(&mut PendingTool, Option<usize>), SessionError> {
    pending
        .iter_mut()
        .find_map(|pending| {
            let key = pending
                .delegations
                .active
                .iter()
                .find_map(|(key, (id, _, _))| (*id == child).then_some(*key))?;
            Some((pending, key))
        })
        .ok_or(SessionError::InvalidHistory)
}

fn valid_record(record: &DelegationRecord) -> bool {
    !record.child.is_nil()
        && !record.site.session.is_nil()
        && record.child != record.site.session
        && record.site.turn != 0
        && record.site.tool.assistant_entry != 0
        && record.spec.valid()
        && !matches!(record.ownership, DelegationOwnership::Background { owner } if owner.is_nil())
        && record.path
            == record
                .spec
                .directory
                .join(format!("{}.sqlite", record.child))
}

impl Session {
    pub(crate) fn admit_delegation(
        &self,
        site: DelegationSite,
        spec: DelegationSpec,
        ownership: DelegationOwnership,
    ) -> Result<(DelegationRecord, Vec<SessionEntry>), SessionError> {
        if site.session != self.id() {
            return Err(SessionError::InvalidHistory);
        }
        let child = uuid::Uuid::new_v4();
        let record = DelegationRecord {
            site,
            child,
            path: spec.directory.join(format!("{child}.sqlite")),
            ownership,
            spec,
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let context = match record.spec.context {
            DelegationContext::Fresh => Vec::new(),
            DelegationContext::Fork { point } => {
                without_delivery_keys(fork_prefix(&store.connection, point)?.0)
            }
        };
        // Resolve/validate the chosen cut before committing intent or creating
        // any child file. Keep the captured prefix outside tool-waiter custody.
        let projection = project(context.iter().map(Ok), ProjectionMode::Metadata, &|entry| {
            Ok(context[(entry - 1) as usize].clone())
        })?;
        if projection.state.active.is_some() || projection.state.pending_shell.is_some() {
            return Err(SessionError::InvalidForkPoint);
        }
        append(
            &mut store,
            &[SessionEntry::DelegationAdmitted {
                record: record.clone(),
            }],
        )?;
        Ok((record, context))
    }

    pub(crate) fn transfer_delegation(
        &self,
        record: &DelegationRecord,
        lease: &std::sync::Arc<()>,
    ) -> Result<(), SessionError> {
        let DelegationOwnership::Background { owner } = record.ownership else {
            return Err(SessionError::InvalidHistory);
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::DelegationTransferred {
                child: record.child,
                owner,
            }],
        )?;
        store
            .live_backgrounds
            .insert(record.child, std::sync::Arc::downgrade(lease));
        Ok(())
    }

    pub(crate) fn settle_delegation(
        &self,
        record: &DelegationRecord,
        outcome: DelegationOutcome,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::DelegationSettled {
                child: record.child,
                outcome,
            }],
        )
    }

    /// Same recorded identity/path only. An existing partial/mismatched database
    /// fails closed; neither initialization nor reopening authorizes dispatch.
    pub(crate) fn materialize_delegation(
        record: &DelegationRecord,
        mut context: Vec<SessionEntry>,
    ) -> Result<Self, SessionError> {
        // Append, never prepend: inherited tool/shell/compaction coordinates
        // continue to identify their original entries. Preferences are explicit
        // child configuration, not authority inherited with the context.
        context.extend([
            SessionEntry::ChildSessionOf {
                record: record.clone(),
            },
            SessionEntry::ModelSelected {
                model: record.spec.model.clone(),
            },
            SessionEntry::ReasoningSelected {
                reasoning: record.spec.reasoning,
            },
        ]);
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
        Ok(child)
    }
}

impl SessionView {
    /// Owned relationships only. Conversation copies retain historical facts,
    /// but do not become the parent or acquire the source's children.
    pub fn delegations(&self) -> Vec<DelegationReceipt> {
        let mut receipts: Vec<DelegationReceipt> = Vec::new();
        let mut positions = BTreeMap::new();
        for entry in &self.entries {
            match entry {
                SessionEntry::DelegationAdmitted { record } if record.site.session == self.id => {
                    positions.insert(record.child, receipts.len());
                    receipts.push(DelegationReceipt {
                        record: record.clone(),
                        transferred: false,
                        outcome: None,
                    });
                }
                SessionEntry::DelegationTransferred { child, .. } => {
                    if let Some(&index) = positions.get(child) {
                        receipts[index].transferred = true;
                    }
                }
                SessionEntry::DelegationSettled { child, outcome } => {
                    if let Some(&index) = positions.get(child) {
                        receipts[index].outcome = Some(outcome.clone());
                    }
                }
                _ => {}
            }
        }
        receipts
    }

    /// The binding of this logical child, not an inherited copy's parentage.
    pub fn parent_delegation(&self) -> Option<&DelegationRecord> {
        self.entries.iter().find_map(|entry| match entry {
            SessionEntry::ChildSessionOf { record } if record.child == self.id => Some(record),
            _ => None,
        })
    }
}
