//! Validation and result-capacity reservation before effects.
use super::*;

struct Admission {
    reservation: InputReservation,
    expanded: Option<String>,
    prepared: Option<AcceptedInput<InputId>>,
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn submit(
        &mut self,
        delivery: Delivery,
        input: UserInput,
        metadata: M,
    ) -> Result<InputId, Box<RejectedInput<M>>> {
        let id = InputId(identity());
        match self.admit(id, delivery, &input, &metadata) {
            Err(reason) => Err(Box::new(RejectedInput {
                delivery,
                input,
                metadata,
                reason,
            })),
            Ok(Admission {
                reservation,
                expanded,
                prepared,
            }) => {
                let phase = if prepared.is_some() {
                    InputPhase::Queued
                } else {
                    InputPhase::Preparing
                };
                let mut custody = lock(&self.custody);
                custody.occupied += 1;
                custody.inputs.insert(
                    id,
                    Input {
                        delivery,
                        original: Some(input),
                        metadata,
                        reservation,
                        prepared,
                        inflight: None,
                        instructions: self.setup.resources.instructions().to_owned(),
                        expanded,
                        acceptance: None,
                        withdrawn: None,
                    },
                );
                custody.updates.push_back(ControlUpdate::Input {
                    id,
                    delivery,
                    phase,
                });
                drop(custody);
                if delivery == Delivery::Start {
                    self.stop_requested = false;
                    self.starting = Some((id, OperationId(identity())));
                }
                self.advance();
                Ok(id)
            }
        }
    }

    /// Durably admit a literal message, or reconcile the same delivery without
    /// dispatch. This does not persist pending queues, metadata, or live authority.
    /// Callers authorize the key's namespace; it is not an authentication token.
    pub fn submit_request(
        &mut self,
        key: &ion_core::RequestKey,
        input: Message,
        metadata: M,
    ) -> Result<RequestAdmission<M>, Box<RejectedInput<M>>> {
        let known = self.setup.session.reconcile_request(key, &input);
        match known {
            Ok(Some(receipt)) => return Ok(RequestAdmission::Existing { receipt, metadata }),
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery: Delivery::Start,
                    input: UserInput::Message(input),
                    metadata,
                    reason: reason.into(),
                }));
            }
            Ok(None) => {}
        }
        let id = InputId(identity());
        let original = UserInput::Message(input);
        let admission = match self.admit(id, Delivery::Start, &original, &metadata) {
            Ok(admission) => admission,
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery: Delivery::Start,
                    input: original,
                    metadata,
                    reason,
                }));
            }
        };
        let (message, _, inflight) = admission
            .prepared
            .expect("literal messages are prepared")
            .into_parts();
        let operation = OperationId(identity());
        let inbox = Arc::new(SteeringInbox::new(
            self.setup.runtime.agent.limits(),
            self.budget.clone(),
        ));
        let instructions = self.setup.resources.instructions().to_owned();
        let mut custody = lock(&self.custody);
        custody.inputs.insert(
            id,
            Input {
                delivery: Delivery::Start,
                original: Some(original),
                metadata,
                reservation: admission.reservation,
                prepared: None,
                inflight: Some(inflight),
                instructions: instructions.clone(),
                expanded: None,
                acceptance: None,
                withdrawn: None,
            },
        );
        // Hold custody until the callback has a complete root to acknowledge.
        let result = self.setup.runtime.agent.submit_request(
            self.setup.session.clone(),
            ion_core::SubmissionRequest {
                key: key.clone(),
                input: message,
            },
            instructions,
            CancellationToken::new(),
            Some(inbox.clone()),
            turn_observer(
                self.custody.clone(),
                self.observations.clone(),
                Some(id),
                operation,
            ),
        );
        match result {
            Ok(ion_core::RequestSubmission {
                receipt,
                operation: Some(work),
            }) => {
                custody.occupied += 1;
                custody
                    .updates
                    .push_back(ControlUpdate::Dispatched { id, operation });
                self.stop_requested = false;
                self.active = Some(Active {
                    id: operation,
                    turn: Some(receipt.turn),
                    root: Some(id),
                    work: Work::Turn(work),
                    inbox,
                    metadata: None,
                });
                Ok(RequestAdmission::Started { id, receipt })
            }
            result => {
                let input = custody
                    .inputs
                    .remove(&id)
                    .expect("unaccepted request retains custody");
                match result {
                    Ok(ion_core::RequestSubmission { receipt, .. }) => {
                        Ok(RequestAdmission::Existing {
                            receipt,
                            metadata: input.metadata,
                        })
                    }
                    Err(reason) => Err(Box::new(RejectedInput {
                        delivery: Delivery::Start,
                        input: input.original.expect("unaccepted original"),
                        metadata: input.metadata,
                        reason: reason.into(),
                    })),
                }
            }
        }
    }

    /// Continue existing committed work, not another user input. The application
    /// must reconstruct and attest the saved authority before calling this method.
    pub fn resume_turn(
        &mut self,
        point: ion_core::RecoveryPoint,
        metadata: M,
    ) -> Result<RecoveryAdmission, (M, anyhow::Error)> {
        let prepared = (|| {
            self.ensure_replaceable()?;
            ensure!(
                lock(&self.custody).occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            Ok(self.budget.reserve(
                &Message::user_input("continue interrupted Turn".into(), []),
                &metadata,
                self.setup.runtime.agent.limits(),
            )?)
        })();
        let reservation = match prepared {
            Ok(value) => value,
            Err(error) => return Err((metadata, error)),
        };
        let operation = OperationId(identity());
        let inbox = Arc::new(SteeringInbox::new(
            self.setup.runtime.agent.limits(),
            self.budget.clone(),
        ));
        let result = self.setup.runtime.agent.resume_turn(
            self.setup.session.clone(),
            point,
            self.setup.resources.instructions().to_owned(),
            CancellationToken::new(),
            Some(inbox.clone()),
            turn_observer(
                self.custody.clone(),
                self.observations.clone(),
                None,
                operation,
            ),
        );
        let (point, work) = match result {
            Ok(value) => value,
            Err(error) => return Err((metadata, error.into())),
        };
        lock(&self.custody).occupied += 1;
        self.stop_requested = false;
        self.active = Some(Active {
            id: operation,
            turn: Some(point.turn),
            root: None,
            work: Work::Turn(work),
            inbox,
            metadata: Some((metadata, reservation)),
        });
        Ok(RecoveryAdmission { operation, point })
    }

    fn admit(
        &self,
        id: InputId,
        delivery: Delivery,
        input: &UserInput,
        metadata: &M,
    ) -> Result<Admission> {
        ensure!(!self.closed, "session control is closed");
        ensure!(
            lock(&self.custody).occupied < MAX_ADMISSIONS,
            "session result/admission capacity is full; drain lifecycle updates"
        );
        match delivery {
            Delivery::Start => self.ensure_replaceable()?,
            Delivery::Steer(target) => {
                ensure!(
                    !self.stop_requested && self.current_operation() == Some(target),
                    "steering target is no longer active"
                );
                ensure!(
                    self.starting.is_some()
                        || self
                            .active
                            .as_ref()
                            .is_some_and(|active| matches!(active.work, Work::Turn(_))),
                    "steering requires a model Turn"
                );
            }
            Delivery::FollowUp => {
                ensure!(
                    !self.stop_requested,
                    "stop intent suppresses follow-ups; start explicitly"
                );
                if self.current_operation().is_none() {
                    let _idle = self.setup.session.reserve_idle()?;
                }
            }
        }
        let limits = self.setup.runtime.agent.limits();
        let (message, expanded, ready) = match input {
            UserInput::Message(message) => (message.clone(), None, true),
            UserInput::Prompt { text, images } => {
                ensure!(
                    !text.trim().is_empty() || !images.is_empty(),
                    "prompt is empty"
                );
                if !images.is_empty() {
                    crate::image_input::require_image_input(
                        self.setup.runtime.agent.model(),
                        limits.image_input,
                    )?;
                    for source in images {
                        if let ImageSource::Path(path) = source {
                            ensure!(
                                self.setup.runtime.local_images,
                                "local image inputs are not enabled by this application"
                            );
                            ensure!(
                                path.is_absolute() || self.setup.session.cwd().is_some(),
                                "relative image input requires a working directory"
                            );
                        }
                    }
                    tokio::runtime::Handle::try_current()?;
                }
                let expanded = self
                    .setup
                    .resources
                    .expand_command(text, limits.max_request_bytes)
                    .transpose()?
                    .unwrap_or_else(|| text.clone());
                // The placeholder only reserves/validates raw image-only inputs;
                // it is never used as model content.
                let reserve_text = if expanded.trim().is_empty() && !images.is_empty() {
                    "[image]".into()
                } else {
                    expanded.clone()
                };
                (
                    Message::user_input(reserve_text, []),
                    Some(expanded),
                    images.is_empty(),
                )
            }
        };
        let reservation = self.budget.reserve(&message, &(input, metadata), limits)?;
        let prepared = if ready {
            Some(self.budget.admit(message, id, limits)?)
        } else {
            None
        };
        Ok(Admission {
            reservation,
            expanded,
            prepared,
        })
    }

    /// Direct actions reserve reliable result/metadata capacity before Core admission.
    pub fn shell(
        &mut self,
        command: String,
        exclude_from_context: bool,
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        let prepared = (|| {
            self.ensure_replaceable()?;
            ensure!(
                lock(&self.custody).occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            let tools = self
                .setup
                .runtime
                .shell
                .as_ref()
                .context("direct shell is not enabled by this application")?
                .prepare(&command)?;
            let reservation = self.budget.reserve(
                &Message::user_input(command.clone(), []),
                &metadata,
                self.setup.runtime.agent.limits(),
            )?;
            Ok((tools, reservation))
        })();
        let (tools, reservation) = match prepared {
            Ok(value) => value,
            Err(error) => return Err((metadata, error)),
        };
        let id = OperationId(identity());
        lock(&self.custody).occupied += 1;
        let work = self.setup.session.run_user_shell(
            command,
            exclude_from_context,
            CancellationToken::new(),
            move |_, stop| tools(stop),
        );
        self.install_direct(id, Work::Shell(work), metadata, reservation);
        Ok(id)
    }

    pub fn compact(&mut self, metadata: M) -> Result<OperationId, (M, anyhow::Error)> {
        let prepared = (|| {
            self.ensure_replaceable()?;
            ensure!(
                lock(&self.custody).occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            Ok(self.budget.reserve(
                &Message::user_input("compact".into(), []),
                &metadata,
                self.setup.runtime.agent.limits(),
            )?)
        })();
        let reservation = match prepared {
            Ok(value) => value,
            Err(error) => return Err((metadata, error)),
        };
        let id = OperationId(identity());
        let observations = self.observations.clone();
        lock(&self.custody).occupied += 1;
        let work = self.setup.runtime.agent.compact(
            self.setup.session.clone(),
            CancellationToken::new(),
            move |event| {
                let _ = observations.send(Observation::Event {
                    operation: id,
                    event: Arc::new(event),
                });
            },
        );
        self.install_direct(id, Work::Compact(work), metadata, reservation);
        Ok(id)
    }

    fn install_direct(
        &mut self,
        id: OperationId,
        work: Work,
        metadata: M,
        reservation: InputReservation,
    ) {
        self.stop_requested = false;
        self.active = Some(Active {
            id,
            turn: None,
            root: None,
            work,
            inbox: Arc::new(SteeringInbox::new(
                self.setup.runtime.agent.limits(),
                self.budget.clone(),
            )),
            metadata: Some((metadata, reservation)),
        });
    }
}
