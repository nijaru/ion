//! Canonical facts, strict live overlay and native-publication custody.
use std::{collections::HashMap, path::PathBuf};

use ion_core::{
    AgentEvent, LiveTranscript, Session, SessionEntry, SessionView, TranscriptItem,
    TranscriptProjection, TranscriptSource,
};
use ion_host::OperationId;

#[cfg(test)]
pub(super) mod fixtures;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    path: PathBuf,
    session: uuid::Uuid,
    selection: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    Unchanged,
    Selection,
    Session,
}

struct Live {
    operation: OperationId,
    transcript: LiveTranscript,
    covered: usize,
}

struct Notice {
    after: Option<TranscriptSource>,
    text: String,
    diagnostic: bool,
}

pub(super) enum Segment<'a> {
    Item(&'a TranscriptItem),
    Notice { ordinal: usize, text: &'a str },
}

/// Coordinates certify a source prefix, not a number of fitted display rows.
#[derive(Debug)]
pub(super) struct Publication {
    scope: Option<Scope>,
    before: Option<TranscriptSource>,
    through: Option<TranscriptSource>,
    notices_before: usize,
    notices_through: usize,
    generation: u64,
}

#[derive(Default)]
pub(super) struct Conversation {
    scope: Option<Scope>,
    canonical: TranscriptProjection,
    positions: HashMap<TranscriptSource, usize>,
    live: Option<Live>,
    notices: Vec<Notice>,
    through: Option<TranscriptSource>,
    notices_published: usize,
    publication_generation: u64,
    banner: Option<String>,
    revision: u64,
}

impl Conversation {
    pub(super) fn scope(&self) -> Option<&Scope> {
        self.scope.as_ref()
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn live(&self) -> Option<&LiveTranscript> {
        self.live.as_ref().map(|live| &live.transcript)
    }
    pub(super) fn live_for(&self, operation: OperationId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.operation == operation)
    }
    pub(super) fn items(&self) -> impl Iterator<Item = &TranscriptItem> + Clone {
        self.canonical.items.iter().chain(self.live_suffix().iter())
    }
    fn live_suffix(&self) -> &[TranscriptItem] {
        self.live.as_ref().map_or(&[], |live| {
            &live.transcript.projection().items[live.covered..]
        })
    }
    pub(super) fn mutable_items(&self) -> &[TranscriptItem] {
        if self.live.is_some() {
            self.live_suffix()
        } else {
            &self.canonical.items[self.canonical.sealed_prefix()..]
        }
    }
    pub(super) fn clear_live(&mut self) {
        self.live = None;
        self.revision += 1;
    }

    /// Returns true when a canonical refresh is required. A surviving event suffix
    /// cannot reconstruct missing call/thinking prefixes or an ahead-of-stream view.
    pub(super) fn observe(&mut self, operation: OperationId, event: &AgentEvent) -> bool {
        self.revision += 1;
        if let AgentEvent::TurnAccepted { turn, .. }
        | AgentEvent::CompletionCommitted {
            turn,
            new_turn: true,
            ..
        } = event
        {
            self.live = None;
            if self
                .canonical
                .items
                .iter()
                .any(|item| super::transcript_item_turn(item) == Some(*turn))
            {
                return false;
            }
            if self.scope.is_none() || self.canonical.sealed_prefix() != self.canonical.items.len()
            {
                return true;
            }
            let mut transcript = LiveTranscript::default();
            transcript.observe(event);
            self.live = Some(Live {
                operation,
                transcript,
                covered: 0,
            });
        } else if let Some(live) = &mut self.live
            && live.operation == operation
        {
            live.transcript.observe(event);
        } else {
            return matches!(
                event,
                AgentEvent::TurnResumed { .. }
                    | AgentEvent::AssistantCommitted { .. }
                    | AgentEvent::SteeringCommitted { .. }
                    | AgentEvent::CompletionCommitted { .. }
                    | AgentEvent::ToolFinished { .. }
                    | AgentEvent::ChildToolAdmitted { .. }
                    | AgentEvent::ChildToolFinished { .. }
                    | AgentEvent::TurnEnded { .. }
            );
        }
        self.promote_sealed();
        false
    }

    fn promote_sealed(&mut self) {
        let Some(live) = &mut self.live else { return };
        let start = live.covered;
        for item in live.transcript.projection().items[start..]
            .iter()
            .take_while(|item| item.is_sealed())
        {
            let source = item
                .source()
                .expect("sealed items have committed source coordinates");
            assert!(
                self.positions
                    .insert(source, self.canonical.items.len())
                    .is_none(),
                "a source enters canonical history once"
            );
            // Core retains active-stream indexes until settlement/restart. Keep
            // sealed facts independently so an observation gap cannot discard an
            // already published prefix along with the removable live projection.
            self.canonical.items.push(item.clone());
            live.covered += 1;
        }
    }

    pub(super) fn reconcile(&mut self, session: &Session, view: &SessionView) -> Change {
        let scope = Scope {
            path: session.path().to_path_buf(),
            session: view.id,
            selection: view
                .entries
                .iter()
                .rposition(|entry| matches!(entry, SessionEntry::ContextSelected { .. }))
                .map_or(0, |index| index as u64 + 1),
        };
        let same_session = self.scope.as_ref().is_some_and(|previous| {
            previous.path == scope.path && previous.session == scope.session
        });
        let change = if self.scope.as_ref() == Some(&scope) {
            Change::Unchanged
        } else if same_session {
            Change::Selection
        } else {
            Change::Session
        };
        let canonical = TranscriptProjection::from_session(view);
        let positions = canonical
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                (
                    item.source()
                        .expect("canonical facts have source coordinates"),
                    index,
                )
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(
            positions.len(),
            canonical.items.len(),
            "canonical item sources are unique"
        );
        if change != Change::Unchanged {
            self.publication_generation += 1;
            let had_previous_session = self.scope.is_some();
            self.notices.clear();
            self.notices_published = 0;
            let tail = super::resume_history_tail(&canonical.items);
            let start = tail.start.min(canonical.sealed_prefix());
            self.through = start.checked_sub(1).map(|index| {
                canonical.items[index]
                    .source()
                    .expect("resume boundary is committed")
            });
            let label = view.id;
            self.banner = if same_session {
                Some(format!(
                    "— selected context at entry {} · alternatives retained · files unchanged —",
                    view.context_position().head_entry
                ))
            } else if !canonical.items.is_empty() {
                Some(if tail.omitted_turns > 0 {
                    format!(
                        "— resumed session {label} · {} earlier turn(s) retained —",
                        tail.omitted_turns
                    )
                } else if tail.omitted_entries > 0 {
                    format!(
                        "— resumed session {label} · {} earlier entr{} retained —",
                        tail.omitted_entries,
                        if tail.omitted_entries == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    )
                } else {
                    format!("— resumed session {label} —")
                })
            } else if had_previous_session {
                Some(format!("— session {label} —"))
            } else {
                None
            };
        } else if let Some(source) = self.through {
            assert!(
                positions.contains_key(&source),
                "a published source cannot disappear without a selection change"
            );
        }
        self.scope = Some(scope);
        self.canonical = canonical;
        self.positions = positions;
        self.live = None;
        self.revision += 1;
        change
    }

    pub(super) fn note(&mut self, text: String, diagnostic: bool) {
        let after = self
            .canonical
            .sealed_prefix()
            .checked_sub(1)
            .and_then(|index| self.canonical.items[index].source());
        if diagnostic
            && self.notices.last().is_some_and(|notice| {
                notice.diagnostic && notice.after == after && notice.text == text
            })
        {
            return;
        }
        self.notices.push(Notice {
            after,
            text,
            diagnostic,
        });
        self.revision += 1;
    }

    pub(super) fn banner(&self) -> Option<&str> {
        self.banner.as_deref()
    }
    pub(super) fn has_published_prefix(&self) -> bool {
        self.through.is_some() || self.notices_published > 0
    }

    fn boundary(&self) -> usize {
        self.through.map_or(0, |source| self.positions[&source] + 1)
    }

    pub(super) fn prepare_publication(&self) -> Publication {
        let end = self.canonical.sealed_prefix();
        assert!(self.boundary() <= end, "published facts must remain sealed");
        Publication {
            scope: self.scope.clone(),
            before: self.through,
            through: end
                .checked_sub(1)
                .and_then(|index| self.canonical.items[index].source()),
            notices_before: self.notices_published,
            notices_through: self.notices.len(),
            generation: self.publication_generation,
        }
    }

    pub(super) fn acknowledge(&mut self, batch: Publication) -> bool {
        if self.scope != batch.scope
            || self.publication_generation != batch.generation
            || self.through != batch.before
            || self.notices_published != batch.notices_before
        {
            return false;
        }
        self.publication_generation += 1;
        self.through = batch.through;
        self.notices_published = batch.notices_through;
        self.banner = None;
        true
    }

    /// Presentation consumes the same ordered document in both viewports. Local
    /// output is anchored to a committed source boundary, never provisional rows.
    pub(super) fn segments(&self, pending: bool) -> Vec<Segment<'_>> {
        let begin = if pending { self.boundary() } else { 0 };
        let end = if pending {
            self.canonical.sealed_prefix()
        } else {
            self.canonical.items.len()
        };
        let mut cursor = begin;
        let mut segments = Vec::new();
        let first_notice = if pending { self.notices_published } else { 0 };
        for (ordinal, notice) in self.notices.iter().enumerate().skip(first_notice) {
            let after = notice.after.map_or(0, |source| self.positions[&source] + 1);
            assert!(
                after >= cursor && after <= end,
                "local output follows a stable source prefix"
            );
            segments.extend(
                self.canonical.items[cursor..after]
                    .iter()
                    .map(Segment::Item),
            );
            segments.push(Segment::Notice {
                ordinal,
                text: &notice.text,
            });
            cursor = after;
        }
        segments.extend(self.canonical.items[cursor..end].iter().map(Segment::Item));
        if !pending {
            segments.extend(self.live_suffix().iter().map(Segment::Item));
        }
        segments
    }
}
