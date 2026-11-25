//! Display snapshot. Unlike SessionView, this does not hydrate model messages or
//! decode inactive branches. Editor recall is indexed separately at commit.
use super::*;
use crate::{
    ToolOccurrence, TranscriptActivity, TranscriptActivityIdentity, TranscriptItem,
    TranscriptProjection, TranscriptSource, UsageTotals,
};
use std::num::NonZeroUsize;

const PAGE_RECORDS: usize = 96;
const PAGE_BYTES: usize = 256 * 1024;
const EVIDENCE_RECORDS: usize = 256;
const EVIDENCE_BYTES: usize = 16 * 1024 * 1024;

/// An older page in the same committed selection. Appends do not change its
/// cut, and switching away and back creates a new selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptCursor {
    session: uuid::Uuid,
    selection: u64,
    cut: u64,
    root: u64,
}

/// A stable tool ordering and journal cut for one selected conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolEvidenceCursor {
    session: uuid::Uuid,
    selection: u64,
    cut: u64,
    head: u64,
}

pub enum ToolEvidenceTarget {
    Latest,
    Number(NonZeroUsize),
    Occurrence(ToolOccurrence),
}

pub struct ToolEvidence {
    pub cursor: ToolEvidenceCursor,
    pub number: NonZeroUsize,
    pub total: usize,
    pub activity: TranscriptActivity,
}

pub struct TranscriptPage {
    pub cursor: TranscriptCursor,
    pub projection: TranscriptProjection,
    pub older: Option<TranscriptCursor>,
    /// Global number of the first tool in each included assistant response.
    /// Includes omitted bundles so opening them does not renumber their calls.
    pub tool_starts: BTreeMap<u64, usize>,
}

struct DisplayBundle {
    root: u64,
    parent: u64,
    records: Vec<u64>,
    first_tool: Option<usize>,
}

fn encoded_len(size: &mut rusqlite::Statement<'_>, record: u64) -> Result<usize, SessionError> {
    let key = i64::try_from(record).map_err(|_| SessionError::InvalidHistory)?;
    let len: i64 = size.query_row([key], |row| row.get(0))?;
    usize::try_from(len).map_err(|_| SessionError::InvalidHistory)
}

fn read_display_bundle(
    snapshot: &Connection,
    records: Vec<u64>,
) -> Result<TranscriptProjection, SessionError> {
    let mut projection = TranscriptProjection::from_selected(
        records
            .into_iter()
            .map(|entry| Ok::<_, SessionError>((entry, read_entry(snapshot, entry)?))),
    )?;
    // Historical bundles are immutable views of this cut, not live runs.
    for item in &mut projection.items {
        if let TranscriptItem::ActivityGroup(group) = item {
            group.open = false;
        }
    }
    Ok(projection)
}

fn read_bounded_evidence(
    snapshot: &Connection,
    records: Vec<u64>,
) -> Result<TranscriptProjection, SessionError> {
    let mut remaining = EVIDENCE_BYTES;
    let mut size = snapshot.prepare_cached("SELECT length(body) FROM entries WHERE seq=?1")?;
    for &record in &records {
        remaining = remaining
            .checked_sub(encoded_len(&mut size, record)?)
            .ok_or(SessionError::EntryTooLarge)?;
    }
    read_display_bundle(snapshot, records)
}

/// A committed cut at an idle boundary. No transcript group or unresolved
/// effect can extend from this cut into a later suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationCursor {
    session: uuid::Uuid,
    selection: u64,
    revision: u64,
    through: u64,
}

/// The selected transcript (or a suffix) and display metadata at one committed cut.
/// Recent editor prompts and cumulative usage deliberately include inactive branches.
pub struct SessionPresentation {
    pub id: uuid::Uuid,
    pub revision: u64,
    pub selection: u64,
    pub head_entry: u64,
    pub name: Option<String>,
    pub reasoning: Reasoning,
    pub unfinished: bool,
    pub after: Option<PresentationCursor>,
    pub checkpoint: Option<PresentationCursor>,
    /// Number of top-level tools in an intentionally omitted startup prefix.
    /// `Some(0)` still indicates older history is available in the pager.
    pub omitted_prefix: Option<usize>,
    pub transcript: TranscriptProjection,
    pub prompts: Vec<String>,
    pub usage: UsageTotals,
}

enum DisplayRead {
    Reconcile(Option<PresentationCursor>),
    Seed,
}

struct TailBoundary {
    entry: u64,
    tools: usize,
}

struct TailFact {
    entry: u64,
    previous: Option<TailBoundary>,
}

enum DisplayPlan {
    Selected(Vec<u64>),
    Tail {
        newest: TailBoundary,
        facts: Vec<TailFact>,
    },
}

impl DisplayPlan {
    fn finish(self, snapshot: &Connection) -> Result<(Vec<u64>, Option<usize>), SessionError> {
        let Self::Tail { newest, facts } = self else {
            let Self::Selected(entries) = self else {
                unreachable!()
            };
            return Ok((entries, None));
        };
        let mut remaining = PAGE_BYTES;
        let mut keep = 0;
        let mut boundary = newest;
        let mut size = snapshot.prepare_cached("SELECT length(body) FROM entries WHERE seq=?1")?;
        for (index, fact) in facts.iter().enumerate() {
            let Some(bytes) = remaining.checked_sub(encoded_len(&mut size, fact.entry)?) else {
                break;
            };
            remaining = bytes;
            if let Some(previous) = &fact.previous {
                keep = index + 1;
                boundary = TailBoundary {
                    entry: previous.entry,
                    tools: previous.tools,
                };
            }
        }
        let selected = facts
            .into_iter()
            .take(keep)
            .rev()
            .map(|fact| fact.entry)
            .collect();
        Ok((selected, (boundary.entry != 0).then_some(boundary.tools)))
    }
}

impl Session {
    fn check_display_snapshot(
        &self,
        snapshot: &Connection,
        expected: u64,
    ) -> Result<Header, SessionError> {
        let header = read_header(snapshot)?;
        // Separate scalar subqueries let SQLite seek either end. A row at
        // zero is not an empty journal.
        let (first, last): (Option<i64>, Option<i64>) = snapshot.query_row(
            "SELECT (SELECT MIN(seq) FROM entries), (SELECT MAX(seq) FROM entries)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let revision = match (first, last) {
            (None, None) => 0,
            (Some(1), Some(last)) if last > 0 => last as u64,
            _ => return Err(SessionError::InvalidHistory),
        };
        if header.id != self.id()
            || header.provider_session_id != self.provider_session_id()
            || revision != expected
        {
            return Err(SessionError::InvalidHistory);
        }
        Ok(header)
    }

    /// Read one bounded page of committed history; `None` means the cursor's
    /// selection is no longer current, not that the page is empty. Causal
    /// bundles can split an activity run between assistant responses, but
    /// never show a result under another response's call with the same ID.
    pub fn transcript_page(
        &self,
        before: Option<TranscriptCursor>,
    ) -> Result<Option<TranscriptPage>, SessionError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let (snapshot, cursor, bundles) = {
            let store = self.lock_store()?;
            let index = &store.context_index;
            let cursor = before.unwrap_or_else(|| TranscriptCursor {
                session: self.id(),
                selection: index.selection,
                cut: store.state.sequence,
                root: index.display_head(),
            });
            if cursor.session != self.id()
                || cursor.selection != index.selection
                || cursor.cut > store.state.sequence
            {
                return Ok(None);
            }
            let snapshot = connection.unchecked_transaction()?;
            self.check_display_snapshot(&snapshot, store.state.sequence)?;
            let mut bundles = Vec::new();
            let mut root = cursor.root;
            let mut available = PAGE_RECORDS;
            while root != 0 && available != 0 {
                let dependents = index.display_bundle(root, cursor.cut);
                let mut records = Vec::new();
                // An empty list marks an indivisible bundle above the count
                // limit. Do not copy a potentially huge list out of the index.
                let count = if dependents.len() >= PAGE_RECORDS {
                    1
                } else {
                    1 + dependents.len()
                };
                if count > available {
                    break;
                }
                if dependents.len() < PAGE_RECORDS {
                    records.push(root);
                    records.extend(dependents.iter().map(|fact| fact.entry));
                }
                available -= count;
                let parent = index.display_parent(root);
                bundles.push(DisplayBundle {
                    root,
                    parent,
                    records,
                    first_tool: index.first_tool_number(root),
                });
                root = parent;
            }
            (snapshot, cursor, bundles)
        };
        let mut budget = PAGE_BYTES;
        let mut older = cursor.root;
        let mut projected = Vec::new();
        let mut tool_starts = BTreeMap::new();
        let mut size = snapshot.prepare_cached("SELECT length(body) FROM entries WHERE seq=?1")?;
        for bundle in bundles {
            let mut bytes = 0usize;
            for &record in &bundle.records {
                bytes = bytes.saturating_add(encoded_len(&mut size, record)?);
                if bytes > PAGE_BYTES {
                    break;
                }
            }
            let items = if bundle.records.is_empty() || bytes > PAGE_BYTES {
                vec![TranscriptItem::Oversized {
                    source: TranscriptSource {
                        entry: bundle.root,
                        part: 0,
                    },
                }]
            } else {
                if bytes > budget {
                    break;
                }
                budget -= bytes;
                read_display_bundle(&snapshot, bundle.records)?.items
            };
            older = bundle.parent;
            if let Some(first) = bundle.first_tool {
                tool_starts.insert(bundle.root, first);
            }
            projected.extend(items.into_iter().rev());
        }
        projected.reverse();
        Ok(Some(TranscriptPage {
            cursor,
            projection: TranscriptProjection { items: projected },
            tool_starts,
            older: (older != 0).then_some(TranscriptCursor {
                root: older,
                ..cursor
            }),
        }))
    }

    /// Expand one source omitted by a transcript page. This uses the same
    /// committed cut as the page, with a larger, separate inspection budget.
    /// `None` means the cursor or source no longer belongs to the selection.
    pub fn transcript_evidence(
        &self,
        page: TranscriptCursor,
        source: TranscriptSource,
    ) -> Result<Option<TranscriptProjection>, SessionError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let (snapshot, records) = {
            let store = self.lock_store()?;
            let index = &store.context_index;
            if page.session != self.id()
                || page.selection != index.selection
                || page.cut > store.state.sequence
                || source.entry == 0
            {
                return Ok(None);
            }
            // A page can contain at most this many roots. Never traverse an
            // arbitrary prefix in response to a source supplied by a reader.
            let mut root = page.root;
            for _ in 1..PAGE_RECORDS {
                if root <= source.entry {
                    break;
                }
                root = index.display_parent(root);
            }
            if root != source.entry {
                return Ok(None);
            }
            let dependents = index.display_bundle(root, page.cut);
            if dependents.len() >= EVIDENCE_RECORDS {
                return Err(SessionError::EntryTooLarge);
            }
            let snapshot = connection.unchecked_transaction()?;
            self.check_display_snapshot(&snapshot, store.state.sequence)?;
            (
                snapshot,
                std::iter::once(root)
                    .chain(dependents.iter().map(|fact| fact.entry))
                    .collect::<Vec<_>>(),
            )
        };
        Ok(Some(read_bounded_evidence(&snapshot, records)?))
    }

    /// Read one top-level tool (including its children), using global numbering
    /// on the selected branch. Adjacent reads can reuse the same committed cut.
    pub fn tool_evidence(
        &self,
        cursor: Option<ToolEvidenceCursor>,
        target: ToolEvidenceTarget,
    ) -> Result<Option<ToolEvidence>, SessionError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let (snapshot, cursor, number, total, occurrence, records) = {
            let store = self.lock_store()?;
            let index = &store.context_index;
            let cursor = cursor.unwrap_or_else(|| ToolEvidenceCursor {
                session: self.id(),
                selection: index.selection,
                cut: store.state.sequence,
                head: index.tool_head(),
            });
            if cursor.session != self.id()
                || cursor.selection != index.selection
                || cursor.cut > store.state.sequence
            {
                return Ok(None);
            }
            let total = index.tool_count(cursor.head);
            let number = match target {
                ToolEvidenceTarget::Latest => NonZeroUsize::new(total),
                ToolEvidenceTarget::Number(number) => Some(number),
                ToolEvidenceTarget::Occurrence(occurrence) => index
                    .tool_number(cursor.head, occurrence)
                    .and_then(NonZeroUsize::new),
            };
            let Some(number) = number else {
                return Ok(None);
            };
            let Some(occurrence) = index.tool_at(cursor.head, number.get()) else {
                return Ok(None);
            };
            let mut records = vec![occurrence.assistant_entry];
            for fact in index
                .display_bundle(occurrence.assistant_entry, cursor.cut)
                .iter()
                .filter(|fact| fact.ordinal == Some(occurrence.ordinal))
            {
                if records.len() == EVIDENCE_RECORDS {
                    return Err(SessionError::EntryTooLarge);
                }
                records.push(fact.entry);
            }
            let snapshot = connection.unchecked_transaction()?;
            self.check_display_snapshot(&snapshot, store.state.sequence)?;
            (snapshot, cursor, number, total, occurrence, records)
        };
        let activity = read_bounded_evidence(&snapshot, records)?
            .items
            .into_iter()
            .flat_map(|item| match item {
                TranscriptItem::ActivityGroup(group) => group.activities.into_iter(),
                _ => Vec::new().into_iter(),
            })
            .find(|activity| activity.identity == TranscriptActivityIdentity::Tool(occurrence))
            .ok_or(SessionError::InvalidHistory)?;
        Ok(Some(ToolEvidence {
            cursor,
            number,
            total,
            activity,
        }))
    }

    /// Visit top-level tool summaries and saved results, newest first, at one
    /// committed cut. Each callback borrows one result; there is no transcript
    /// projection or raw capture/child scan. The callback runs outside Store
    /// locks and should retain only the data its caller needs.
    pub fn visit_committed_tools(
        &self,
        mut visit: impl FnMut(NonZeroUsize, &TranscriptActivity),
    ) -> Result<(), SessionError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let (snapshot, cut, mut head) = {
            let store = self.lock_store()?;
            let cut = store.state.sequence;
            let snapshot = connection.unchecked_transaction()?;
            self.check_display_snapshot(&snapshot, cut)?;
            (snapshot, cut, store.context_index.tool_head())
        };
        while head != 0 {
            // Batch links and already-committed results are immutable. An
            // append after the pinned WAL cut can fill a zero result slot;
            // never read that newer coordinate from this historical walk.
            let batch = self.lock_store()?.context_index.tool_batch(head).clone();
            let SessionEntry::Assistant {
                turn,
                message,
                tool_activities,
                termination,
                ..
            } = read_entry(&snapshot, head)?
            else {
                return Err(SessionError::InvalidHistory);
            };
            if tool_activities.len() != batch.results.len() {
                return Err(SessionError::InvalidHistory);
            }
            let mut calls = message.content.iter().rev().filter_map(|part| match part {
                Content::ToolCall(call) => Some(call),
                _ => None,
            });
            for (ordinal, (&entry, stored)) in
                batch.results.iter().zip(&tool_activities).enumerate().rev()
            {
                let call = calls.next().ok_or(SessionError::InvalidHistory)?;
                if call.id != stored.call_id {
                    return Err(SessionError::InvalidHistory);
                }
                let mut activity = TranscriptActivity::admitted(
                    ToolOccurrence {
                        assistant_entry: head,
                        ordinal,
                    },
                    call,
                    stored,
                    &termination,
                );
                if entry != 0 && entry <= cut {
                    match read_entry(&snapshot, entry)? {
                        SessionEntry::ToolResult {
                            turn: recorded,
                            result,
                        } if recorded == turn
                            && result.call_id == call.id
                            && result.name == call.name =>
                        {
                            activity.observe_outcome(&result.outcome)
                        }
                        _ => return Err(SessionError::InvalidHistory),
                    }
                }
                activity.mark_unobserved();
                visit(
                    NonZeroUsize::new(batch.first + ordinal).expect("one-based tool number"),
                    &activity,
                );
            }
            if calls.next().is_some() {
                return Err(SessionError::InvalidHistory);
            }
            head = batch.previous;
        }
        Ok(())
    }

    /// Bounded editable text observed from committed inputs across all branches.
    pub fn recent_prompts(&self) -> Result<Vec<String>, SessionError> {
        Ok(self.lock_store()?.context_index.prompt_recall.snapshot())
    }

    /// Anchor an uninterrupted observed turn without reading journal payloads.
    /// The caller owns event continuity. The index verifies the observed
    /// admission immediately follows the retained transcript and the observed
    /// closure is still the selected idle boundary.
    pub fn observed_turn_checkpoint(
        &self,
        prefix: PresentationCursor,
        admission: u64,
        closure: u64,
    ) -> Result<Option<PresentationCursor>, SessionError> {
        let store = self.lock_store()?;
        let index = &store.context_index;
        let revision = store.state.sequence;
        if prefix.session != self.id()
            || prefix.selection != index.selection
            || admission <= prefix.revision
            || closure <= admission
            || store.state.active.is_some()
            || store.state.pending_shell.is_some()
            || index.transcript_checkpoint() != Some(closure)
            || index.transcript_parent(admission) != prefix.through
        {
            return Ok(None);
        }
        Ok(Some(PresentationCursor {
            revision,
            through: closure,
            ..prefix
        }))
    }

    /// Read display facts after a prior idle cut, or rebuild for a different
    /// selection/session. The returned `after` tells which prefix to retain.
    pub fn presentation(
        &self,
        after: Option<PresentationCursor>,
    ) -> Result<SessionPresentation, SessionError> {
        self.read_presentation(DisplayRead::Reconcile(after))
    }

    /// Initialize an ordinary terminal from a bounded suffix of complete
    /// settled units. Never use this to reconcile an already-mounted display.
    /// Interrupted and non-boundary selections still rebuild in full.
    pub fn presentation_seed(&self) -> Result<SessionPresentation, SessionError> {
        self.read_presentation(DisplayRead::Seed)
    }

    fn read_presentation(&self, read: DisplayRead) -> Result<SessionPresentation, SessionError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_connection(&connection)?;
        let (
            snapshot,
            header,
            revision,
            selection,
            head_entry,
            reasoning,
            unfinished,
            usage,
            selected,
            after,
            checkpoint,
            prompts,
        ) = {
            let store = self.lock_store()?;
            let snapshot = connection.unchecked_transaction()?;
            let header = self.check_display_snapshot(&snapshot, store.state.sequence)?;
            let revision = store.state.sequence;
            let index = &store.context_index;
            let unfinished = store.state.active.is_some() || store.state.pending_shell.is_some();
            let checkpoint = if unfinished {
                None
            } else {
                index
                    .transcript_checkpoint()
                    .map(|through| PresentationCursor {
                        session: header.id,
                        selection: index.selection,
                        revision,
                        through,
                    })
            };
            let (after, selected) = match (read, checkpoint) {
                (DisplayRead::Seed, Some(checkpoint)) => {
                    let newest = TailBoundary {
                        entry: checkpoint.through,
                        tools: index.tools_through(checkpoint.through),
                    };
                    let mut cursor = newest.entry;
                    let mut facts = Vec::new();
                    while cursor != 0 && facts.len() < PAGE_RECORDS {
                        let previous = index.transcript_parent(cursor);
                        facts.push(TailFact {
                            entry: cursor,
                            previous: index.is_transcript_boundary(previous).then(|| {
                                TailBoundary {
                                    entry: previous,
                                    tools: index.tools_through(previous),
                                }
                            }),
                        });
                        cursor = previous;
                    }
                    (None, DisplayPlan::Tail { newest, facts })
                }
                (read, _) => {
                    let after = match read {
                        DisplayRead::Reconcile(after) => after.filter(|cut| {
                            cut.session == header.id
                                && cut.selection == index.selection
                                && cut.revision <= revision
                        }),
                        DisplayRead::Seed => None,
                    };
                    match after.and_then(|cut| index.transcript_after(cut.through)) {
                        Some(selected) => (after, DisplayPlan::Selected(selected)),
                        None => (
                            None,
                            DisplayPlan::Selected(
                                index
                                    .transcript_after(0)
                                    .expect("root terminates every ancestry"),
                            ),
                        ),
                    }
                }
            };
            (
                snapshot,
                header,
                revision,
                index.selection,
                index.head(),
                store.state.reasoning,
                unfinished,
                index.usage,
                selected,
                after,
                checkpoint,
                index.prompt_recall.snapshot(),
            )
        };
        let (selected, omitted_prefix) = selected.finish(&snapshot)?;
        let transcript = TranscriptProjection::from_selected(
            selected
                .into_iter()
                .map(|entry| Ok::<_, SessionError>((entry, read_entry(&snapshot, entry)?))),
        )?;
        Ok(SessionPresentation {
            id: header.id,
            revision,
            selection,
            head_entry,
            name: header.name,
            reasoning,
            unfinished,
            after,
            checkpoint,
            omitted_prefix,
            transcript,
            prompts,
            usage,
        })
    }
}
