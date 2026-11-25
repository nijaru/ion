//! Canonical facts, strict live overlay and native-publication custody.
use std::{collections::HashMap, path::PathBuf};

use ion_app::OperationId;
use ion_core::{
    AgentEvent, LiveTranscript, PresentationCursor, Session, TranscriptItem, TranscriptPart,
    TranscriptProjection, TranscriptSource,
};

#[cfg(test)]
use ion_core::{SessionEntry, SessionView};

fn source_positions(canonical: &TranscriptProjection) -> HashMap<TranscriptSource, usize> {
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
    positions
}

// Summarize the sealed prefix so the inline tail never has to visit it.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Visibility {
    #[default]
    Empty,
    Thinking,
    Content,
}

impl Visibility {
    fn of(item: &TranscriptItem) -> Self {
        let TranscriptItem::Assistant(message) = item else {
            return Self::Content;
        };
        let mut visible = Self::Empty;
        for part in &message.parts {
            match part {
                TranscriptPart::Thinking(_) => visible = Self::Thinking,
                TranscriptPart::Text(text) if text.trim().is_empty() => {}
                TranscriptPart::Text(_) | TranscriptPart::Image { .. } => return Self::Content,
            }
        }
        visible
    }

    fn visible(self, thinking: bool) -> bool {
        self == Self::Content || (thinking && self == Self::Thinking)
    }
}

#[derive(Clone, Copy, Default)]
struct SealedPrefix {
    len: usize,
    visibility: Visibility,
}

impl SealedPrefix {
    fn extend(&mut self, items: &[TranscriptItem]) {
        for item in items.iter().take_while(|item| item.is_sealed()) {
            self.push(item);
        }
    }

    fn push(&mut self, item: &TranscriptItem) {
        self.len += 1;
        if self.visibility != Visibility::Content {
            self.visibility = self.visibility.max(Visibility::of(item));
        }
    }
}

pub(super) struct Prepared {
    canonical: TranscriptProjection,
    sealed: SealedPrefix,
    positions: HashMap<TranscriptSource, usize>,
    session: uuid::Uuid,
    selection: u64,
    head_entry: u64,
    prefix: PreparedPrefix,
    checkpoint: Option<PresentationCursor>,
}

enum PreparedPrefix {
    Rebuild(super::ResumeHistoryTail),
    Retain(PresentationCursor),
}

impl Prepared {
    #[cfg(test)]
    pub(super) fn new(view: &SessionView) -> Self {
        Self::committed(
            TranscriptProjection::from_session(view),
            view.id,
            view.entries
                .iter()
                .rposition(|entry| matches!(entry, SessionEntry::ContextSelected { .. }))
                .map_or(0, |index| index as u64 + 1),
            view.context_position().head_entry,
            None,
            None,
        )
    }

    pub(super) fn committed(
        canonical: TranscriptProjection,
        session: uuid::Uuid,
        selection: u64,
        head_entry: u64,
        after: Option<PresentationCursor>,
        checkpoint: Option<PresentationCursor>,
    ) -> Self {
        let positions = source_positions(&canonical);
        let mut sealed = SealedPrefix::default();
        sealed.extend(&canonical.items);
        let prefix = after.map_or_else(
            || PreparedPrefix::Rebuild(super::resume_history_tail(&canonical.items)),
            PreparedPrefix::Retain,
        );
        Self {
            canonical,
            sealed,
            positions,
            session,
            selection,
            head_entry,
            prefix,
            checkpoint,
        }
    }
}

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
    admission: u64,
    completed: Option<u64>,
}

struct Notice {
    after: Option<TranscriptSource>,
    text: String,
    diagnostic: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Rows {
    Pending,
    Sealed,
    All,
    Mutable,
}

pub(super) enum Segment<'a> {
    Item(&'a TranscriptItem),
    Notice { ordinal: usize, text: &'a str },
}

impl Segment<'_> {
    pub(super) fn visible(&self, thinking: bool) -> bool {
        match self {
            Self::Item(item) => Visibility::of(item).visible(thinking),
            Self::Notice { text, .. } => !text.is_empty(),
        }
    }
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
    // Item count at the idle cut, before any live promotion or replayed
    // unfinished suffix. Native publication has its own, later frontier.
    checkpoint: Option<(PresentationCursor, SealedPrefix)>,
    sealed: SealedPrefix,
    live: Option<Live>,
    notices: Vec<Notice>,
    visible_notice: bool,
    through: Option<TranscriptSource>,
    notices_published: usize,
    publication_generation: u64,
    banner: Option<String>,
    revision: u64,
}

impl Conversation {
    /// A read page has no publication custody or local notices.
    pub(super) fn history_page(canonical: TranscriptProjection) -> Self {
        let mut sealed = SealedPrefix::default();
        sealed.extend(&canonical.items);
        Self {
            positions: source_positions(&canonical),
            canonical,
            sealed,
            ..Self::default()
        }
    }

    pub(super) fn scope(&self) -> Option<&Scope> {
        self.scope.as_ref()
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn checkpoint(&self, session: &Session) -> Option<PresentationCursor> {
        let scope = self.scope.as_ref()?;
        if scope.path == session.path() && scope.session == session.id() {
            self.checkpoint.map(|(cursor, _)| cursor)
        } else {
            None
        }
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
            &self.canonical.items[self.sealed.len..]
        }
    }
    pub(super) fn clear_live(&mut self) {
        if self.live.take().is_some() {
            self.revision += 1;
        }
    }

    /// Returns true when a canonical refresh is required. A surviving event suffix
    /// cannot reconstruct missing call/thinking prefixes or an ahead-of-stream view.
    pub(super) fn observe(&mut self, operation: OperationId, event: &AgentEvent) -> bool {
        self.revision += 1;
        if let AgentEvent::TurnAccepted { turn, entry, .. }
        | AgentEvent::CompletionCommitted {
            turn,
            entry,
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
            if self.scope.is_none() || self.sealed.len != self.canonical.items.len() {
                return true;
            }
            let mut transcript = LiveTranscript::default();
            transcript.observe(event);
            self.live = Some(Live {
                operation,
                transcript,
                covered: 0,
                admission: *entry,
                completed: None,
            });
        } else if let Some(live) = &mut self.live
            && live.operation == operation
        {
            live.transcript.observe(event);
            if let AgentEvent::TurnEnded {
                entry,
                reason: ion_core::TurnEndReason::Completed,
                ..
            } = event
            {
                live.completed = Some(*entry);
            }
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

    /// Settlement follows the joined publisher's ready tail. A gap clears
    /// `live`; a resumed or hidden predecessor cannot advance the idle cut.
    pub(super) fn accept_observed_turn(
        &mut self,
        session: &Session,
        operation: OperationId,
    ) -> Result<bool, ion_core::SessionError> {
        let Some(prefix) = self.checkpoint(session) else {
            return Ok(false);
        };
        let Some(live) = self
            .live
            .as_ref()
            .filter(|live| live.operation == operation)
        else {
            return Ok(false);
        };
        let Some(closure) = live.completed else {
            return Ok(false);
        };
        if live.covered != live.transcript.projection().items.len() {
            return Ok(false);
        }
        let Some(checkpoint) = session.observed_turn_checkpoint(prefix, live.admission, closure)?
        else {
            return Ok(false);
        };
        self.checkpoint = Some((checkpoint, self.sealed));
        Ok(true)
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
            self.sealed.push(item);
            live.covered += 1;
        }
    }

    #[cfg(test)]
    pub(super) fn reconcile(&mut self, session: &Session, view: &SessionView) -> Change {
        self.reconcile_prepared(session.path(), Prepared::new(view))
    }

    pub(super) fn reconcile_prepared(
        &mut self,
        path: &std::path::Path,
        prepared: Prepared,
    ) -> Change {
        let Prepared {
            canonical,
            sealed,
            positions,
            session,
            selection,
            head_entry,
            prefix,
            checkpoint,
        } = prepared;
        let scope = Scope {
            path: path.to_path_buf(),
            session,
            selection,
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
        if change != Change::Unchanged {
            let PreparedPrefix::Rebuild(tail) = prefix else {
                unreachable!("new selection cannot reuse a transcript prefix");
            };
            self.publication_generation += 1;
            let had_previous_session = self.scope.is_some();
            self.notices.clear();
            self.visible_notice = false;
            self.notices_published = 0;
            let start = tail.start.min(sealed.len);
            self.through = start.checked_sub(1).map(|index| {
                canonical.items[index]
                    .source()
                    .expect("resume boundary is committed")
            });
            let label = session;
            self.banner = if same_session {
                Some(format!(
                    "— selected context at entry {} · alternatives retained · files unchanged —",
                    head_entry
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
        }
        match prefix {
            PreparedPrefix::Rebuild(_) => {
                self.sealed = sealed;
                self.canonical = canonical;
                self.positions = positions;
                self.checkpoint = None;
            }
            PreparedPrefix::Retain(after) => {
                let (previous, retained) =
                    self.checkpoint.expect("delta requires a retained prefix");
                assert_eq!(after, previous, "delta must start at the retained idle cut");
                self.sealed = SealedPrefix {
                    len: retained.len + sealed.len,
                    visibility: retained.visibility.max(sealed.visibility),
                };
                for item in self.canonical.items.drain(retained.len..) {
                    self.positions.remove(
                        &item
                            .source()
                            .expect("canonical facts have source coordinates"),
                    );
                }
                for (source, offset) in positions {
                    assert!(
                        self.positions
                            .insert(source, retained.len + offset)
                            .is_none(),
                        "a committed suffix cannot duplicate an earlier source"
                    );
                }
                self.canonical.items.extend(canonical.items);
            }
        }
        if let Some(source) = self.through {
            assert!(
                self.positions.contains_key(&source),
                "a published source cannot disappear without a selection change"
            );
        }
        if let Some(checkpoint) = checkpoint {
            assert_eq!(
                self.sealed.len,
                self.canonical.items.len(),
                "idle cut must be sealed"
            );
            self.checkpoint = Some((checkpoint, self.sealed));
        }
        self.scope = Some(scope);
        self.live = None;
        self.revision += 1;
        change
    }

    pub(super) fn note(&mut self, text: String, diagnostic: bool) {
        let after = self
            .sealed
            .len
            .checked_sub(1)
            .and_then(|index| self.canonical.items[index].source());
        if diagnostic
            && self.notices.last().is_some_and(|notice| {
                notice.diagnostic && notice.after == after && notice.text == text
            })
        {
            return;
        }
        self.visible_notice |= !text.is_empty();
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

    pub(super) fn before_mutable(&self, thinking: bool) -> (Option<TranscriptSource>, bool) {
        let previous = self
            .sealed
            .len
            .checked_sub(1)
            .and_then(|index| self.canonical.items[index].source());
        (
            previous,
            self.visible_notice || self.sealed.visibility.visible(thinking),
        )
    }

    fn boundary(&self) -> usize {
        self.through.map_or(0, |source| self.positions[&source] + 1)
    }

    pub(super) fn prepare_publication(&self) -> Publication {
        let end = self.sealed.len;
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
    pub(super) fn segments(&self, rows: Rows) -> Vec<Segment<'_>> {
        if rows == Rows::Mutable {
            return self.mutable_items().iter().map(Segment::Item).collect();
        }
        let pending = rows == Rows::Pending;
        let begin = if pending { self.boundary() } else { 0 };
        let end = if rows != Rows::All {
            self.sealed.len
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
        if rows == Rows::All {
            segments.extend(self.live_suffix().iter().map(Segment::Item));
        }
        segments
    }
}
