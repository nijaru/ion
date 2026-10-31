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
    pub fn submit_completions<F, M: Send + 'static>(
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
        let Some(completions) = inbox.completions.as_ref() else {
            return Ok(None);
        };
        let (turn, operation) = session.operations.start_prepared(stop.clone(), || {
            self.validate_context(&session, &stop)?;
            let batch = completions.batch(&session, None, false)?;
            let notices = batch.notices();
            if notices.is_empty() {
                return Ok((None, None));
            }
            let input = crate::completion::notice_message(&notices);
            self.validate_submission(&session, &input, &stop)?;
            let (binding, catalog) = self.admission_binding(&session, &instructions)?;
            let TurnAdmission::New {
                turn, interrupted, ..
            } = session.admit_turn_input(
                input,
                self.model.clone(),
                None,
                binding,
                notices.clone(),
            )?
            else {
                unreachable!("anonymous completion admission cannot reconcile");
            };
            batch.commit(turn);
            let agent = self.clone();
            let session = session.clone();
            let inbox = inbox.clone();
            Ok((
                Some(turn),
                Some(async move {
                    let (mut observe, failed) = contained_observer(observe, stop.clone());
                    let result = agent
                        .run_admitted(
                            &session,
                            TurnStart {
                                turn,
                                interrupted,
                                origin: TurnOrigin::Automated { notices },
                                catalog,
                            },
                            instructions,
                            stop,
                            Some(&inbox),
                            &mut observe,
                        )
                        .await;
                    observation_result(result, failed.load(std::sync::atomic::Ordering::Relaxed))
                }),
            ))
        })?;
        Ok(turn.map(|turn| CompletionSubmission {
            turn,
            operation: operation.expect("admitted completion retains its operation"),
        }))
    }
}
