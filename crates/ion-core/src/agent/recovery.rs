//! Continuation enters the same retained operation and drive loop as new work.
use super::*;
use crate::{
    AuthorityScope, RecoveryBinding, RecoveryPoint, ToolCatalog, request::RequestSettings,
};

pub(super) struct TurnStart {
    pub turn: u64,
    pub interrupted: usize,
    pub origin: TurnOrigin,
    pub catalog: Option<ToolCatalog>,
}

pub(super) enum TurnOrigin {
    User {
        input: Message,
    },
    Resumed {
        point: RecoveryPoint,
    },
    Automated {
        notices: Vec<crate::CompletionNotice>,
    },
}

pub(super) struct TurnRun {
    pub turn: u64,
    pub catalog: Option<ToolCatalog>,
    pub route_reason: ModelRouteReason,
}

impl Agent {
    /// Opt in to explicit continuation. The application attests that this versioned
    /// reference identifies the actual principal, environment, routes and grants.
    /// Never use a model/tool name or secret as a substitute. Callable declarations,
    /// instructions and controls remain pinned for each admitted Turn. Prepare
    /// dynamic sources first; their refresh takes effect on a subsequent Turn.
    pub fn with_recovery_scope(mut self, authority: AuthorityScope) -> Self {
        self.recovery_scope = Some(authority);
        self
    }

    pub(super) fn request_settings<'a>(&'a self, instructions: &'a str) -> RequestSettings<'a> {
        RequestSettings {
            model: &self.model,
            instructions,
            limits: self.limits,
            authority: self.recovery_scope.as_ref(),
        }
    }

    pub(super) fn admission_binding(
        &self,
        session: &Session,
        instructions: &str,
    ) -> Result<(Option<RecoveryBinding>, Option<ToolCatalog>), AgentError> {
        let Some(_) = self.recovery_scope else {
            return Ok((None, None));
        };
        let previous = session.model_context()?;
        let catalog = self.tools.snapshot_with_previous(
            previous
                .as_ref()
                .map_or(&[], |context| context.tools.as_slice()),
        );
        let binding = self
            .request_settings(instructions)
            .binding(session, &catalog)?;
        Ok((binding, Some(catalog)))
    }

    /// Explicitly continue an unfinished Turn from one exact committed checkpoint.
    /// Missing observations become Unknown; no old call or guest is replayed.
    /// Retrying this checkpoint is stale, not another execution authorization.
    pub fn resume_turn<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        expected: RecoveryPoint,
        instructions: String,
        stop: CancellationToken,
        steering: Option<Arc<SteeringInbox<M>>>,
        observe: F,
    ) -> Result<(RecoveryPoint, crate::Operation<String, AgentError>), AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        let result = session.operations.start_prepared(stop.clone(), || {
            self.validate_context(&session, &stop)?;
            if session.model_preference()?.as_ref() != Some(&self.model) {
                return Err(AgentError::Session(SessionError::RecoveryAuthorityChanged));
            }
            self.service.validate_controls(
                &self.model,
                &self.limits.controls(session.reasoning()?, true),
            )?;
            let (binding, catalog) = self.admission_binding(&session, &instructions)?;
            let binding = binding.ok_or(SessionError::RecoveryAuthorityChanged)?;
            let (point, interrupted) = session.resume_turn(expected, &binding)?;
            let agent = self.clone();
            let session = session.clone();
            Ok((
                point,
                Some(async move {
                    let (mut observe, failed) = contained_observer(observe, stop.clone());
                    let result = agent
                        .run_admitted(
                            &session,
                            TurnStart {
                                turn: point.turn,
                                interrupted,
                                origin: TurnOrigin::Resumed { point },
                                catalog,
                            },
                            instructions,
                            stop,
                            steering.as_deref(),
                            &mut observe,
                        )
                        .await;
                    observation_result(result, failed.load(std::sync::atomic::Ordering::Relaxed))
                }),
            ))
        })?;
        Ok((
            result.0,
            result.1.expect("continuation always retains an operation"),
        ))
    }
}
