//! Journal codec and projection boundaries, independent of the storage owner.
//! A reader supplies ordered records and point reads from the same committed cut.
//! A writer commits the encoded batch at `expected_tail` before publishing projections.
use super::*;
use crate::ToolOccurrence;

// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
pub(crate) const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct AppendBasis<'a> {
    pub state: &'a State,
    pub context_index: &'a ContextIndex,
    pub settled: &'a BTreeSet<u64>,
    pub message_count: usize,
}

pub(super) struct PreparedAppend {
    pub expected_tail: u64,
    pub encoded: Vec<Vec<u8>>,
    pub candidate: State,
    pub resolved_tools: Vec<Option<ToolOccurrence>>,
    pub replacement: Option<context_index::WorkingContext>,
    pub replace_pending_message: bool,
    pub new_messages: Vec<ProjectedMessage>,
    pub new_settled: Vec<u64>,
    pub new_context_changes: Vec<ModelContextChange>,
}

impl AppendBasis<'_> {
    pub fn prepare(
        &self,
        entries: &[SessionEntry],
        read: impl Fn(u64) -> Result<SessionEntry, SessionError>,
        validate_artifact: impl Fn(&crate::ArtifactRef) -> Result<(), SessionError>,
    ) -> Result<PreparedAppend, SessionError> {
        let mut replacement = None;
        let mut candidate = self.state.clone();
        // A shared pending shell owns the final context message. Seed only that
        // slot so candidate settlement can replace it without rebuilding history.
        let pending_message = self
            .state
            .pending_shell
            .as_ref()
            .filter(|shell| !shell.exclude_from_context)
            .map(|shell| {
                ProjectedMessage::new(
                    shell.admission_entry,
                    shell_message(
                        &shell.command,
                        &UserShellOutcome::Unknown,
                        false,
                        shell.admission_entry,
                    ),
                )
            });
        let replace_pending_message = pending_message.is_some();
        let mut new_messages: Vec<_> = pending_message.into_iter().collect();
        let message_start = self.message_count - usize::from(replace_pending_message);
        let mut new_context_changes = Vec::new();
        let mut new_settled = Vec::new();
        let mut resolved_tools = Vec::with_capacity(entries.len());
        let encoded = entries
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                for reference in artifacts::references(entry) {
                    validate_artifact(reference)?;
                }
                let transition = candidate.apply(entry, &|coordinate| {
                    self.settled.contains(&coordinate) || new_settled.contains(&coordinate)
                })?;
                resolved_tools.push(transition.resolved_tool);
                if transition.settled {
                    new_settled.push(candidate.sequence);
                }
                projection::append_message(
                    &mut new_messages,
                    candidate.sequence,
                    entry,
                    transition.closed_shell.as_ref(),
                );
                if let SessionEntry::ContextSelected { head_entry } = entry {
                    if replacement.is_some() || offset + 1 != entries.len() {
                        return Err(SessionError::InvalidHistory);
                    }
                    if !self.context_index.can_select(*head_entry) {
                        return Err(SessionError::InvalidContextPoint);
                    }
                    candidate.restore_context(self.context_index.read_context(*head_entry, &read)?);
                    replacement = Some(working_context(
                        self.context_index,
                        *head_entry,
                        &candidate,
                        &read,
                    )?);
                }
                if let SessionEntry::ModelContextChanged { context, .. } = entry
                    && candidate.compaction.is_none()
                    && candidate.replay_epoch_start == 0
                {
                    new_context_changes.push(ModelContextChange {
                        after_message: message_start + new_messages.len(),
                        context: model_context_state(context),
                    });
                }
                encode_entry(entry)
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        Ok(PreparedAppend {
            expected_tail: self.state.sequence,
            encoded,
            candidate,
            resolved_tools,
            replacement,
            replace_pending_message,
            new_messages,
            new_settled,
            new_context_changes,
        })
    }
}

/// A lending cursor: native SQLite can expose the current BLOB without copying
/// it or retaining the raw journal. An adapter must keep the cut pinned through
/// the final context lookup, not just until this cursor reaches EOF.
pub(super) trait EntryRows {
    fn next_record(&mut self) -> Result<Option<EncodedRecord<'_>>, SessionError>;
}

pub(super) struct EncodedRecord<'a> {
    pub sequence: i64,
    pub body: &'a [u8],
}

impl EncodedRecord<'_> {
    pub fn decode(self, expected: u64) -> Result<SessionEntry, SessionError> {
        // Coordinates identify effects, context branches and history reads.
        // Never substitute iteration order for a damaged physical key.
        if expected == 0 || u64::try_from(self.sequence).ok() != Some(expected) {
            return Err(SessionError::InvalidHistory);
        }
        if self.body.len() > MAX_ENTRY_BYTES {
            return Err(SessionError::EntryTooLarge);
        }
        Ok(serde_json::from_slice(self.body)?)
    }
}

pub(super) fn replay(
    rows: impl EntryRows,
    mode: ProjectionMode,
    read: impl Fn(u64) -> Result<SessionEntry, SessionError>,
) -> Result<HistoryProjection, SessionError> {
    project(decode_rows(rows), mode, &read)
}

pub(super) fn decode_rows(
    mut rows: impl EntryRows,
) -> impl Iterator<Item = Result<SessionEntry, SessionError>> {
    let mut sequence = 0u64;
    let mut ended = false;
    std::iter::from_fn(move || {
        if ended {
            return None;
        }
        let next = (|| {
            let Some(record) = rows.next_record()? else {
                return Ok(None);
            };
            sequence = sequence
                .checked_add(1)
                .ok_or(SessionError::InvalidHistory)?;
            record.decode(sequence).map(Some)
        })();
        if !matches!(next, Ok(Some(_))) {
            ended = true;
        }
        next.transpose()
    })
}

pub(super) fn encode_entry(entry: &SessionEntry) -> Result<Vec<u8>, SessionError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rows<'a>(std::slice::Iter<'a, (i64, Vec<u8>)>);

    impl EntryRows for Rows<'_> {
        fn next_record(&mut self) -> Result<Option<EncodedRecord<'_>>, SessionError> {
            Ok(self.0.next().map(|(sequence, body)| EncodedRecord {
                sequence: *sequence,
                body,
            }))
        }
    }

    #[test]
    fn replay_can_restore_an_accepted_turn_from_encoded_rows_without_sqlite() {
        let input = Message::user_input("accepted input".into(), []);
        let mut rows = [(
            1,
            encode_entry(&SessionEntry::TurnStarted {
                turn: 1,
                input: input.clone(),
                model: ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
                completion: Vec::new(),
                request: Some(RequestKey::new("fixture".into(), "once".into()).unwrap()),
                recovery: None,
            })
            .unwrap(),
        )];
        let restore = |records: &[(i64, Vec<u8>)]| {
            replay(Rows(records.iter()), ProjectionMode::Working, |entry| {
                let (sequence, body) = &records[usize::try_from(entry - 1).unwrap()];
                EncodedRecord {
                    sequence: *sequence,
                    body,
                }
                .decode(entry)
            })
        };
        let restored = restore(&rows).unwrap();
        assert_eq!(restored.state.active, Some(1));
        assert_eq!(restored.messages[0].message, input);
        rows[0].0 = 2;
        assert!(matches!(restore(&rows), Err(SessionError::InvalidHistory)));
    }
}
