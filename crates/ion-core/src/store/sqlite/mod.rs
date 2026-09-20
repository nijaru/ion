//! SQLite connection owner for the replacement Session runtime.

mod connection;
mod ownership;
mod semantic;
pub(crate) mod schema;

use std::path::Path;

use rusqlite::Connection;

use super::{StoreError, StoreMetadata};
use crate::observation::ObservationHub;
use crate::session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, StartTurnRequest, StartedTurn,
};
use crate::{
    CommitReceipt, CommitSeq, ConversationConfig, ConversationId, EntryId, EntryPage,
    InstalledConfig, SessionId, SessionSnapshot, SnapshotRequest, TurnId,
};

pub(crate) struct SqliteDatabase {
    connection: Connection,
    _ownership: ownership::Ownership,
    observations: ObservationHub,
    fenced: bool,
}

impl SqliteDatabase {
    pub(crate) fn create(
        path: &Path,
        session_id: SessionId,
        config: ConversationConfig,
        observations: ObservationHub,
    ) -> Result<(Self, StoreMetadata, CommitReceipt), StoreError> {
        if path.exists() {
            return Err(StoreError::AlreadyExists(path.to_path_buf()));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let mut connection = connection::create(path)?;
        schema::initialize(&connection, session_id)?;
        let (metadata, receipt) = semantic::create_primary(&mut connection, session_id, config)?;
        observations.publish(receipt.clone());
        Ok((
            Self {
                connection,
                _ownership: ownership,
                observations,
                fenced: false,
            },
            metadata,
            receipt,
        ))
    }

    pub(crate) fn open(
        path: &Path,
        observations: ObservationHub,
    ) -> Result<(Self, StoreMetadata), StoreError> {
        if !path.exists() {
            return Err(StoreError::Unknown(path.to_path_buf()));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::open(path)?;
        schema::verify(&connection)?;
        let metadata = semantic::metadata(&connection)?;
        Ok((
            Self {
                connection,
                _ownership: ownership,
                observations,
                fenced: false,
            },
            metadata,
        ))
    }

    pub(crate) fn create_conversation(
        &mut self,
        config: ConversationConfig,
    ) -> Result<CreatedConversation, StoreError> {
        let result = self.mutate(|connection| semantic::create_conversation(connection, config))?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn current_config(
        &self,
        conversation: ConversationId,
    ) -> Result<InstalledConfig, StoreError> {
        semantic::current_config(&self.connection, conversation)
    }

    pub(crate) fn config_as_of(
        &self,
        conversation: ConversationId,
        revision: CommitSeq,
    ) -> Result<InstalledConfig, StoreError> {
        semantic::config_as_of(&self.connection, conversation, revision)
    }

    pub(crate) fn configure(
        &mut self,
        conversation: ConversationId,
        expected_revision: CommitSeq,
        config: ConversationConfig,
    ) -> Result<ConfiguredConversation, StoreError> {
        let result = self.mutate(|connection| {
            semantic::configure(connection, conversation, expected_revision, config)
        })?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn admit_input(
        &mut self,
        conversation: ConversationId,
        request: AdmitInputRequest,
    ) -> Result<Admission, StoreError> {
        let result =
            self.mutate(|connection| semantic::admit_input(connection, conversation, request))?;
        if let Admission::Created { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn start_turn(
        &mut self,
        request: StartTurnRequest,
    ) -> Result<StartedTurn, StoreError> {
        let result = self.mutate(|connection| semantic::start_turn(connection, request))?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn cancel_turn(
        &mut self,
        turn: TurnId,
    ) -> Result<CancellationResult, StoreError> {
        let result = self.mutate(|connection| semantic::cancel_turn(connection, turn))?;
        if let CancellationResult::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn abandon_turn(
        &mut self,
        turn: TurnId,
    ) -> Result<AbandonResult, StoreError> {
        let result = self.mutate(|connection| semantic::abandon_turn(connection, turn))?;
        if let AbandonResult::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn snapshot(
        &mut self,
        request: SnapshotRequest,
    ) -> Result<SessionSnapshot, StoreError> {
        semantic::snapshot(&mut self.connection, request)
    }

    pub(crate) fn page_entries(
        &self,
        conversation: ConversationId,
        before: Option<EntryId>,
        limit: usize,
    ) -> Result<EntryPage, StoreError> {
        semantic::page_entries(&self.connection, conversation, before, limit)
    }

    fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if self.fenced {
            return Err(StoreError::Fenced {
                cause: "an earlier persistence operation was ambiguous".to_owned(),
            });
        }
        match operation(&mut self.connection) {
            Ok(value) => Ok(value),
            Err(error) if error.is_semantic_rejection() => Err(error),
            Err(error) => {
                self.fenced = true;
                Err(StoreError::Fenced {
                    cause: error.to_string(),
                })
            }
        }
    }
}
