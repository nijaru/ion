//! Display snapshot. Unlike SessionView, this does not hydrate model messages or
//! decode inactive branches (except their user prompts for editor recall).
use super::*;
use crate::{TranscriptProjection, UsageTotals};

/// The selected transcript and session-wide display metadata at one committed cut.
/// Prompts and usage deliberately include inactive branches.
pub struct SessionPresentation {
    pub id: uuid::Uuid,
    pub revision: u64,
    pub selection: u64,
    pub head_entry: u64,
    pub name: Option<String>,
    pub reasoning: Reasoning,
    pub unfinished: bool,
    pub transcript: TranscriptProjection,
    pub prompts: Vec<String>,
    pub usage: UsageTotals,
}

pub(super) fn prompt(entry: &SessionEntry) -> Option<&str> {
    let input = match entry {
        SessionEntry::TurnStarted {
            input, completion, ..
        } if completion.is_empty() => input,
        SessionEntry::Steering { input, .. } => input,
        _ => return None,
    };
    input.content.iter().find_map(|part| match part {
        Content::Text(text) => Some(text.as_str()),
        _ => None,
    })
}

impl Session {
    pub fn presentation(&self) -> Result<SessionPresentation, SessionError> {
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
            recall,
        ) = {
            let store = self.lock_store()?;
            let snapshot = connection.unchecked_transaction()?;
            let header = read_header(&snapshot)?;
            // The read must have the same committed frontier as the in-memory
            // index, even if the selected branch and prompt list are empty.
            let frontier: i64 =
                snapshot.query_row("SELECT COALESCE(MAX(seq), 0) FROM entries", [], |row| {
                    row.get(0)
                })?;
            let revision = u64::try_from(frontier).map_err(|_| SessionError::InvalidHistory)?;
            if header.id != self.id()
                || header.provider_session_id != self.provider_session_id()
                || revision != store.state.sequence
            {
                return Err(SessionError::InvalidHistory);
            }
            let index = &store.context_index;
            (
                snapshot,
                header,
                revision,
                index.selection,
                index.head(),
                store.state.reasoning,
                store.state.active.is_some() || store.state.pending_shell.is_some(),
                index.usage,
                index.transcript_path(),
                index.prompts.clone(),
            )
        };
        let transcript = TranscriptProjection::from_selected(
            selected
                .into_iter()
                .map(|entry| Ok::<_, SessionError>((entry, read_entry(&snapshot, entry)?))),
        )?;
        let prompts = recall
            .into_iter()
            .map(|entry| {
                prompt(&read_entry(&snapshot, entry)?)
                    .map(str::to_owned)
                    .ok_or(SessionError::InvalidHistory)
            })
            .collect::<Result<_, _>>()?;
        Ok(SessionPresentation {
            id: header.id,
            revision,
            selection,
            head_entry,
            name: header.name,
            reasoning,
            unfinished,
            transcript,
            prompts,
            usage,
        })
    }
}
