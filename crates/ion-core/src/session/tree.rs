use std::fmt;

use uuid::Uuid;

/// Stable identity of one append-only conversation entry.
///
/// IDs are provisioned before persistence so operation/effect state can refer
/// to an entry that will be committed atomically with that state. Storage
/// sequence numbers are ordering metadata, not identity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct EntryId(Uuid);

impl EntryId {
    #[must_use]
    pub(crate) fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    #[must_use]
    pub(crate) const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    #[must_use]
    pub(crate) const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entry-{}", self.0)
    }
}

/// One node in the passive session conversation tree.
///
/// `T` is deliberately generic while Ion migrates its existing semantic entry
/// enum. Tree topology should not depend on provider, tool, or operation
/// bookkeeping. Multiple entries may have `parent = None`; that represents
/// branches from the session's virtual root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Entry<T> {
    pub(crate) id: EntryId,
    pub(crate) parent: Option<EntryId>,
    pub(crate) value: T,
}

impl<T> Entry<T> {
    #[must_use]
    pub(crate) fn new(parent: Option<EntryId>, value: T) -> Self {
        Self {
            id: EntryId::generate(),
            parent,
            value,
        }
    }

    #[must_use]
    pub(crate) const fn with_id(id: EntryId, parent: Option<EntryId>, value: T) -> Self {
        Self { id, parent, value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_share_identity_without_copying_the_prefix() {
        let root = Entry::new(None, "root");
        let left = Entry::new(Some(root.id), "left");
        let right = Entry::new(Some(root.id), "right");

        assert_eq!(left.parent, Some(root.id));
        assert_eq!(right.parent, Some(root.id));
        assert_ne!(left.id, right.id);
    }

    #[test]
    fn provisioned_identity_survives_construction() {
        let id = EntryId::generate();
        let entry = Entry::with_id(id, None, "message");
        assert_eq!(entry.id, id);
        assert_eq!(EntryId::from_uuid(id.as_uuid()), id);
    }
}
