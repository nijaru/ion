//! SQLite connection owner for the replacement Session runtime.

mod connection;
mod model_state;
mod ownership;
pub(crate) mod schema;
mod semantic;
mod tool_state;

use std::path::Path;

use rusqlite::Connection;

use super::{
    CreatedModelAttempt, CreatedModelStep, DriveBasis, FinishedTurn, RecordedModelAttempt,
    SelectedModelResponse, StoreError, StoreMetadata,
};
use crate::observation::ObservationHub;
use crate::session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, StartTurnRequest, StartedTurn,
};
use crate::{
    CommitReceipt, CommitSeq, ConversationConfig, ConversationId, EntryId, EntryPage,
    InstalledConfig, ModelAttemptState, ModelAttemptTiming, RequestManifest, SessionId,
    SessionSnapshot, SnapshotRequest, StepId, TurnId, TurnSettings,
};

pub(crate) struct SqliteDatabase {
    connection: Connection,
    _ownership: ownership::Ownership,
    observations: ObservationHub,
    fenced: bool,
}

impl SqliteDatabase {
    pub(crate) fn tool_records(&self, step: StepId) -> Result<super::ToolRecords, StoreError> {
        tool_state::records(&self.connection, step)
    }

    pub(crate) fn tool_mutate(
        &mut self,
        operation: super::ToolMutation,
    ) -> Result<super::ToolMutationResult, StoreError> {
        let result = self.mutate(|connection| tool_state::mutate(connection, operation))?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }
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

    pub(crate) fn cancel_turn(&mut self, turn: TurnId) -> Result<CancellationResult, StoreError> {
        let result = self.mutate(|connection| semantic::cancel_turn(connection, turn))?;
        if let CancellationResult::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn abandon_turn(&mut self, turn: TurnId) -> Result<AbandonResult, StoreError> {
        let result = self.mutate(|connection| semantic::abandon_turn(connection, turn))?;
        if let AbandonResult::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn drive_basis(&self, turn: TurnId) -> Result<DriveBasis, StoreError> {
        model_state::drive_basis(&self.connection, turn)
    }

    pub(crate) fn create_initial_model_step(
        &mut self,
        turn: TurnId,
        manifest: RequestManifest,
    ) -> Result<CreatedModelStep, StoreError> {
        let result =
            self.mutate(|connection| model_state::create_initial_step(connection, turn, manifest))?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn create_fallback_model_step(
        &mut self,
        predecessor: StepId,
        settings: TurnSettings,
        manifest: RequestManifest,
        reason: String,
    ) -> Result<CreatedModelStep, StoreError> {
        let result = self.mutate(|connection| {
            model_state::create_fallback_step(connection, predecessor, settings, manifest, reason)
        })?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn commit_model_attempt_intent(
        &mut self,
        step: StepId,
        generation: u64,
        timing: ModelAttemptTiming,
    ) -> Result<CreatedModelAttempt, StoreError> {
        let result = self.mutate(|connection| {
            model_state::commit_attempt_intent(connection, step, generation, timing)
        })?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn record_model_start_receipt(
        &mut self,
        attempt: crate::AttemptId,
        receipt_value: crate::ProviderStartReceipt,
    ) -> Result<RecordedModelAttempt, StoreError> {
        let result = self.mutate(|connection| {
            model_state::record_start_receipt(connection, attempt, receipt_value)
        })?;
        if let RecordedModelAttempt::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn settle_model_attempt(
        &mut self,
        attempt: crate::AttemptId,
        state: ModelAttemptState,
    ) -> Result<RecordedModelAttempt, StoreError> {
        let result =
            self.mutate(|connection| model_state::settle_attempt(connection, attempt, state))?;
        if let RecordedModelAttempt::Committed { receipt, .. } = &result {
            self.observations.publish(receipt.clone());
        }
        Ok(result)
    }

    pub(crate) fn select_final_model_response(
        &mut self,
        attempt: crate::AttemptId,
    ) -> Result<SelectedModelResponse, StoreError> {
        let result =
            self.mutate(|connection| model_state::select_final_response(connection, attempt))?;
        self.observations.publish(result.receipt.clone());
        Ok(result)
    }

    pub(crate) fn finish_cancelled_turn(
        &mut self,
        turn: TurnId,
    ) -> Result<FinishedTurn, StoreError> {
        let result =
            self.mutate(|connection| model_state::finish_cancelled_turn(connection, turn))?;
        self.observations.publish(result.receipt.clone());
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
