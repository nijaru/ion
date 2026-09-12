use std::collections::VecDeque;

use crate::session::command::{
    ConversationReceipt, ConversationSpec, EntryReceipt, EntryRequest, InputReceipt, InputRequest,
    SessionError, TaskReceipt, TaskRequest,
};
use crate::session::transaction::{MutationBatch, Transaction};
use crate::store::MemoryStore;
use crate::view::{CommitEvent, ObservationBatch, SessionSnapshot};
use crate::{CommitSeq, ConversationId, RequestKey, SessionId};

const OBSERVATION_CAPACITY: usize = 128;

#[derive(Debug)]
pub struct Session {
    store: MemoryStore,
    observations: VecDeque<CommitEvent>,
    dropped_through: Option<CommitSeq>,
}

impl Session {
    pub fn new() -> Result<Self, SessionError> {
        Self::with_id(SessionId::new())
    }

    pub fn with_id(session_id: SessionId) -> Result<Self, SessionError> {
        let mut session = Self {
            store: MemoryStore::new(session_id),
            observations: VecDeque::new(),
            dropped_through: None,
        };
        session.transact(|transaction| transaction.create_root())?;
        Ok(session)
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.store.state().session_id
    }

    #[must_use]
    pub fn root_conversation(&self) -> ConversationId {
        self.store
            .state()
            .root_conversation
            .expect("initialized session has root conversation")
    }

    pub fn create_conversation(
        &mut self,
        spec: ConversationSpec,
    ) -> Result<ConversationReceipt, SessionError> {
        let (conversation_id, commit_seq) =
            self.transact(|transaction| transaction.create_conversation(spec))?;
        Ok(ConversationReceipt {
            conversation_id,
            commit_seq,
        })
    }

    pub fn append_entry(&mut self, request: EntryRequest) -> Result<EntryReceipt, SessionError> {
        let (entry_id, commit_seq) =
            self.transact(|transaction| transaction.append_entry(request))?;
        Ok(EntryReceipt {
            entry_id,
            commit_seq,
        })
    }

    pub fn create_task(&mut self, request: TaskRequest) -> Result<TaskReceipt, SessionError> {
        let (task_id, commit_seq) =
            self.transact(|transaction| transaction.create_task(request))?;
        Ok(TaskReceipt {
            task_id,
            commit_seq,
        })
    }

    pub fn admit_input(&mut self, request: InputRequest) -> Result<InputReceipt, SessionError> {
        if let Some(key) = request.request_key.as_ref()
            && let Some(receipt) = self.replay_input(key, &request)?
        {
            return Ok(receipt);
        }

        let (input_id, commit_seq) =
            self.transact(|transaction| transaction.admit_input(request))?;
        Ok(InputReceipt {
            input_id,
            commit_seq,
            replayed: false,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        let state = self.store.state();
        SessionSnapshot {
            session_id: state.session_id,
            root_conversation: state
                .root_conversation
                .expect("initialized session has root conversation"),
            last_commit: state
                .last_commit
                .expect("initialized session has first commit"),
            conversations: state.conversations.values().copied().collect(),
            entries: state.entries.values().cloned().collect(),
            inputs: state.inputs.values().cloned().collect(),
            tasks: state.tasks.values().cloned().collect(),
        }
    }

    #[must_use]
    pub fn observations_after(&self, cursor: Option<CommitSeq>) -> ObservationBatch {
        let reset_required = match (cursor, self.dropped_through) {
            (Some(cursor), Some(dropped)) => cursor <= dropped,
            _ => false,
        };
        if reset_required {
            return ObservationBatch {
                reset_required: true,
                events: Vec::new(),
            };
        }

        let events = self
            .observations
            .iter()
            .filter(|event| cursor.is_none_or(|cursor| event.commit_seq > cursor))
            .cloned()
            .collect();
        ObservationBatch {
            reset_required: false,
            events,
        }
    }

    fn replay_input(
        &self,
        key: &RequestKey,
        request: &InputRequest,
    ) -> Result<Option<InputReceipt>, SessionError> {
        let state = self.store.state();
        let Some(input_id) = state.request_keys.get(key).copied() else {
            return Ok(None);
        };
        let existing = state.inputs.get(&input_id).ok_or_else(|| {
            SessionError::Invariant("request key points to missing input".to_owned())
        })?;
        if existing.target != request.target
            || existing.sender != request.sender
            || existing.mode != request.mode
            || existing.body != request.body
        {
            return Err(SessionError::IdempotencyConflict(key.clone()));
        }
        let commit_seq = state.input_commits.get(&input_id).copied().ok_or_else(|| {
            SessionError::Invariant("admitted input is missing its commit sequence".to_owned())
        })?;
        Ok(Some(InputReceipt {
            input_id,
            commit_seq,
            replayed: true,
        }))
    }

    fn transact<T>(
        &mut self,
        build: impl FnOnce(&mut Transaction) -> Result<T, SessionError>,
    ) -> Result<(T, CommitSeq), SessionError> {
        let mut transaction = Transaction::new(self.store.state());
        let value = build(&mut transaction)?;
        let batch = transaction.finish()?;
        let event = batch.event();
        let commit_seq = event.commit_seq;
        self.commit(batch)?;
        self.publish(event);
        Ok((value, commit_seq))
    }

    fn commit(&mut self, batch: MutationBatch) -> Result<(), SessionError> {
        self.store
            .commit(batch)
            .map_err(|error| SessionError::Invariant(error.to_string()))
    }

    fn publish(&mut self, event: CommitEvent) {
        if self.observations.len() == OBSERVATION_CAPACITY
            && let Some(dropped) = self.observations.pop_front()
        {
            self.dropped_through = Some(dropped.commit_seq);
        }
        self.observations.push_back(event);
    }
}
