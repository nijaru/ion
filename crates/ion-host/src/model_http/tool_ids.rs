//! Destination-only aliases; committed identities and retained replay stay intact.
use std::{borrow::Cow, collections::BTreeMap};

use super::{ProviderError, invalid, unsupported, valid_identifier};

#[derive(Default)]
pub(super) struct ToolIds<'a> {
    unsigned: BTreeMap<&'a str, Cow<'a, str>>,
    owners: BTreeMap<Cow<'a, str>, &'a str>,
    next: u64,
}

impl<'a> ToolIds<'a> {
    pub fn for_call(&mut self, id: &'a str, retained: bool) -> Result<Cow<'a, str>, ProviderError> {
        let available = self.owners.get(id).is_none_or(|owner| *owner == id);
        if retained {
            // Native replay identities are opaque, not migration candidates.
            // Never rename one to resolve a collision with an earlier alias.
            if !available {
                return Err(unsupported(
                    "Anthropic tool ID conflicts with retained replay; explicitly compact or start a new Session",
                ));
            }
            self.owners.insert(Cow::Borrowed(id), id);
            return Ok(Cow::Borrowed(id));
        }
        if let Some(assigned) = self.unsigned.get(id) {
            return Ok(assigned.clone());
        }
        let wire = if valid_identifier(id) && available {
            Cow::Borrowed(id)
        } else {
            // Allocate in transcript order. Reserving future IDs could change
            // an already-encoded prefix when history grows, invalidating later
            // native thinking signatures. Later unsigned collisions get aliases.
            loop {
                let candidate = format!("ion_{}", self.next);
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| invalid("Anthropic tool ID namespace exhausted"))?;
                if !self.owners.contains_key(candidate.as_str()) {
                    break Cow::Owned(candidate);
                }
            }
        };
        self.owners.insert(wire.clone(), id);
        self.unsigned.insert(id, wire.clone());
        Ok(wire)
    }
}
