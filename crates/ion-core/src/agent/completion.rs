//! Automated admission uses the ordinary engine without fabricating user input.
use super::*;

/// One admitted automated Turn and its retained execution, not a user receipt.
pub struct CompletionSubmission {
    pub turn: u64,
    pub operation: crate::Operation<String, AgentError>,
}

impl Agent {
    /// Admit one new automated Turn for queued wake-authorized readiness.
    /// The trusted receiving host must independently authorize execution. This
    /// method never resets Stop, expands resources, or acknowledges reports.
    pub async fn submit_completions<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        instructions: String,
        stop: CancellationToken,
        inbox: Arc<SteeringInbox<M>>,
        observe: F,
    ) -> Result<Option<CompletionSubmission>, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        if inbox.completions.is_none() {
            return Ok(None);
        }
        let io = session.io()?;
        let turn_io = session.io()?;
        let agent = self.clone();
        io.run_with(move |session| {
            let mut observe = Some(observe);
            let (turn, operation) = session.operations.start_prepared(stop.clone(), || {
                agent.validate_context(session, &stop)?;
                let mut batch = inbox
                    .completions
                    .as_ref()
                    .expect("completion inbox checked before admission")
                    .batch(session, None, false)?;
                let notices = batch.notices();
                if notices.is_empty() {
                    return Ok((None, None));
                }
                let input = crate::completion::notice_message(&notices);
                agent.validate_submission(session, &input, &stop)?;
                let (binding, catalog) = agent.admission_binding(session, &instructions)?;
                batch.begin();
                let admission = session.admit_turn_input(
                    input,
                    agent.model.clone(),
                    None,
                    binding,
                    notices.clone(),
                );
                let TurnAdmission::New {
                    turn,
                    entry,
                    interrupted,
                    ..
                } = (match admission {
                    Ok(admission) => admission,
                    Err(error) => {
                        if !error.is_commit_uncertain() {
                            batch.reject();
                        }
                        return Err(AgentError::from(error));
                    }
                })
                else {
                    unreachable!("anonymous completion admission cannot reconcile");
                };
                batch.commit(turn);
                let observe = observe
                    .take()
                    .expect("automated admission owns its observer");
                Ok((
                    Some(turn),
                    Some(async move {
                        let (mut observe, failed) = contained_observer(observe, stop.clone());
                        let result = agent
                            .run_admitted(
                                turn_io,
                                TurnStart {
                                    turn,
                                    interrupted,
                                    origin: TurnOrigin::Automated { entry, notices },
                                    catalog,
                                },
                                instructions,
                                stop,
                                Some(&inbox),
                                &mut observe,
                            )
                            .await;
                        observation_result(
                            result,
                            failed.load(std::sync::atomic::Ordering::Relaxed),
                        )
                    }),
                ))
            })?;
            Ok(turn.map(|turn| CompletionSubmission {
                turn,
                operation: operation.expect("admitted completion retains its operation"),
            }))
        })
        .await
    }
}
