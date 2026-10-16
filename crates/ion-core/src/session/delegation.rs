//! Relationship facts are part of Session history, not another task ledger.
use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{
    DelegationContext, DelegationOutcome, DelegationOwnership, DelegationReceipt, DelegationRecord,
    DelegationSite, ForegroundSpec,
};

#[derive(Default, Clone)]
pub(super) struct PendingDelegations {
    active: BTreeMap<Option<usize>, uuid::Uuid>,
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
            .map(|child| SessionEntry::DelegationSettled {
                child: *child,
                outcome: DelegationOutcome::Unknown,
            })
    }
}

impl State {
    pub(super) fn apply_delegation(&mut self, entry: &SessionEntry) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ChildSessionOf { record } => {
                if self.sequence != 0 || !valid_record(record) {
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
                parent.delegations.active.insert(key, record.child);
            }
            SessionEntry::DelegationSettled { child, outcome } => {
                let (parent, key) = self
                    .pending
                    .iter_mut()
                    .find_map(|pending| {
                        let key = pending
                            .delegations
                            .active
                            .iter()
                            .find_map(|(key, id)| (id == child).then_some(*key))?;
                        Some((pending, key))
                    })
                    .ok_or(SessionError::InvalidHistory)?;
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
                parent.delegations.active.remove(&key);
                parent.delegations.finished.insert(key);
            }
            _ => unreachable!("delegation variants only"),
        }
        Ok(())
    }
}

fn valid_record(record: &DelegationRecord) -> bool {
    !record.child.is_nil()
        && !record.site.session.is_nil()
        && record.child != record.site.session
        && record.site.turn != 0
        && record.site.tool.assistant_entry != 0
        && record.spec.valid()
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
        spec: ForegroundSpec,
    ) -> Result<DelegationRecord, SessionError> {
        if site.session != self.id() {
            return Err(SessionError::InvalidHistory);
        }
        let child = uuid::Uuid::new_v4();
        let record = DelegationRecord {
            site,
            child,
            path: spec.directory.join(format!("{child}.sqlite")),
            context: DelegationContext::Fresh,
            ownership: DelegationOwnership::Foreground,
            spec,
        };
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::DelegationAdmitted {
                record: record.clone(),
            }],
        )?;
        Ok(record)
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
    pub(crate) fn materialize_delegation(record: &DelegationRecord) -> Result<Self, SessionError> {
        let child = if record.path.try_exists()? {
            Self::open(&record.path)?
        } else {
            Self::create_initial(
                &record.path,
                record.spec.cwd.as_deref(),
                record.child,
                vec![SessionEntry::ChildSessionOf {
                    record: record.clone(),
                }],
            )?
        };
        if child.id() != record.child
            || child.cwd() != record.spec.cwd.as_deref()
            || child.view()?.parent_delegation() != Some(record)
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
                        outcome: None,
                    });
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
        self.entries.first().and_then(|entry| match entry {
            SessionEntry::ChildSessionOf { record } if record.child == self.id => Some(record),
            _ => None,
        })
    }
}
