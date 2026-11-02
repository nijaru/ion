//! Admission preflight, retained preparation custody and adoption of actual Core work.
use super::*;

fn admission_unknown(error: &anyhow::Error) -> bool {
    fn uncertain(error: &SessionError) -> bool {
        error.is_commit_uncertain()
            || matches!(
                error,
                SessionError::Operation(ion_core::OperationError::TaskFailed(_))
            )
    }
    error.chain().any(|source| {
        source.downcast_ref::<SessionError>().is_some_and(uncertain)
            || source.downcast_ref::<AgentError>().is_some_and(
                |error| matches!(error, AgentError::Session(error) if uncertain(error)),
            )
            || source
                .downcast_ref::<ion_core::OperationError>()
                .is_some_and(|error| matches!(error, ion_core::OperationError::TaskFailed(_)))
            || source.is::<tokio::task::JoinError>()
    })
}

/// Check a literal, unit-metadata launch batch before application setup has effects. Retain
/// both original-input and ready-message accounting for the entire batch. Local
/// identities use their maximum encoded width; live custody rechecks its state.
pub fn preflight_messages(
    messages: impl IntoIterator<Item = Result<Message>>,
    limits: ion_core::AgentLimits,
) -> Result<Vec<Message>> {
    let messages = messages.into_iter();
    ensure_capacity(messages.size_hint().0, 0)?;
    let budget = ion_core::InputBudget::default();
    let mut reservations = Vec::new();
    let mut output = Vec::new();
    for message in messages {
        ensure_capacity(output.len() + 1, 0)?;
        let original = UserInput::Message(message?);
        let UserInput::Message(message) = &original else {
            unreachable!("preflight constructs literal messages")
        };
        reservations.push(budget.reserve(message, &(&original, ()), limits)?);
        reservations.push(budget.reserve(message, &InputId(u64::MAX), limits)?);
        let UserInput::Message(message) = original else {
            unreachable!("preflight constructs literal messages")
        };
        output.push(message);
    }
    ensure!(!output.is_empty(), "input batch is empty");
    Ok(output)
}

fn ensure_capacity(count: usize, occupied: usize) -> Result<()> {
    ensure!(
        count <= MAX_ADMISSIONS - occupied,
        "session result/admission capacity is full; drain lifecycle updates"
    );
    Ok(())
}

struct Admission {
    reservation: InputReservation,
    content: AdmissionContent,
}

enum AdmissionContent {
    Images(Message),
    Ready(Message),
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn submit(
        &mut self,
        delivery: Delivery,
        input: UserInput,
        metadata: M,
    ) -> Result<InputId, Box<RejectedInput<M>>> {
        self.submit_inputs(delivery, vec![(input, metadata)])
            .map(|mut ids| ids.pop().expect("one submitted input"))
            .map_err(|rejected| {
                let RejectedBatch { mut inputs, reason } = *rejected;
                let (input, metadata) = inputs.pop().expect("one rejected input");
                Box::new(RejectedInput {
                    delivery,
                    input,
                    metadata,
                    reason,
                })
            })
    }

    /// Queue an explicit Start followed by ordered FollowUps, or return every
    /// original without queuing any. Admission is local and volatile, not a
    /// transaction over subsequent Turns or asynchronous image preparation.
    pub fn submit_batch(
        &mut self,
        inputs: Vec<(UserInput, M)>,
    ) -> Result<Vec<InputId>, Box<RejectedBatch<M>>> {
        self.submit_inputs(Delivery::Start, inputs)
    }

    fn submit_inputs(
        &mut self,
        delivery: Delivery,
        inputs: Vec<(UserInput, M)>,
    ) -> Result<Vec<InputId>, Box<RejectedBatch<M>>> {
        let admissions = (|| {
            ensure!(!inputs.is_empty(), "input batch is empty");
            ensure_capacity(inputs.len(), lock(&self.custody).occupied)?;
            inputs
                .iter()
                .enumerate()
                .map(|(index, (input, metadata))| {
                    // An explicit new Start can clear retained stop intent only
                    // after the whole batch passes. Validate its tail against that
                    // unchanged Start eligibility, not against stale follow-up intent.
                    let admission = self.admit(delivery, input, metadata);
                    if inputs.len() == 1 {
                        admission
                    } else {
                        admission.with_context(|| format!("input {}", index + 1))
                    }
                })
                .collect::<Result<Vec<_>>>()
        })();
        let admissions = match admissions {
            Ok(admissions) => admissions,
            Err(reason) => return Err(Box::new(RejectedBatch { inputs, reason })),
        };
        let mut custody = lock(&self.custody);
        let staged: Result<Vec<_>> = (|| {
            // Live steering may have filled capacity during validation.
            ensure_capacity(inputs.len(), custody.occupied)?;
            let mut staged = Vec::with_capacity(admissions.len());
            for Admission {
                reservation,
                content,
            } in admissions
            {
                // Identity order is admission order, not validation start order.
                let id = InputId(identity());
                let (content, phase) = match content {
                    AdmissionContent::Images(message) => {
                        (InputContent::Images(message), InputPhase::Preparing)
                    }
                    AdmissionContent::Ready(message) => {
                        let prepared =
                            self.budget
                                .prepare(message, id, self.setup.runtime.agent.limits())?;
                        (InputContent::Ready(prepared), InputPhase::Queued)
                    }
                };
                staged.push((id, reservation, content, phase));
            }
            Ok(staged)
        })();
        let staged = match staged {
            Ok(staged) => staged,
            Err(reason) => return Err(Box::new(RejectedBatch { inputs, reason })),
        };
        let mut ids = Vec::with_capacity(staged.len());
        for (index, ((input, metadata), (id, reservation, content, phase))) in
            inputs.into_iter().zip(staged).enumerate()
        {
            let input_delivery = if index == 0 {
                delivery
            } else {
                Delivery::FollowUp
            };
            custody.occupied += 1;
            custody.inputs.insert(
                id,
                Input {
                    delivery: input_delivery,
                    metadata: SnapshotValue::new(metadata),
                    reservation,
                    state: InputState::Pending(Box::new(PendingInput {
                        original: input,
                        instructions: self.setup.resources.instructions().to_owned(),
                        content,
                        withdrawn: None,
                    })),
                    steering_status: None,
                },
            );
            custody.push_update(ControlUpdate::Input {
                id,
                delivery: input_delivery,
                phase,
            });
            ids.push(id);
        }
        drop(custody);
        if delivery == Delivery::Start {
            self.stop_requested = false;
            self.completions.rearm();
            self.starting = Some((ids[0], OperationId(identity())));
        }
        self.advance();
        Ok(ids)
    }

    /// Durably admit a literal message, or reconcile the same delivery without
    /// dispatch. This does not persist pending queues, metadata, or live authority.
    /// Callers authorize the key's namespace; it is not an authentication token.
    pub async fn submit_request(
        &mut self,
        key: &ion_core::RequestKey,
        input: Message,
        metadata: M,
    ) -> Result<RequestAdmission<M>, Box<RejectedInput<M>>> {
        self.submit_linked_request(key, input, metadata, self.work_stop())
            .await
    }

    /// Owns only a read lane and literal input, never the controller or metadata.
    /// This lookup intentionally precedes closed/busy/capacity preflight.
    pub fn prepare_request(
        &self,
        key: ion_core::RequestKey,
        input: Message,
    ) -> impl std::future::Future<Output = Result<RequestPlan>> + Send + 'static + use<M> {
        let session = self.session().id();
        let reader = self.session().reader();
        async move {
            let existing = reader?.reconcile_request(&key, &input).await?;
            Ok(RequestPlan {
                session,
                key,
                input,
                existing,
            })
        }
    }

    /// Launch without borrowing control across I/O. Pending is local custody,
    /// not acceptance. The reliable RequestAdmission update carries the decision.
    pub fn launch_request(
        &mut self,
        plan: RequestPlan,
        metadata: M,
    ) -> Result<RequestLaunch<M>, Box<RejectedInput<M>>> {
        self.launch_linked_request(plan, metadata, self.work_stop())
    }

    pub(crate) async fn submit_linked_request(
        &mut self,
        key: &ion_core::RequestKey,
        input: Message,
        metadata: M,
        stop: CancellationToken,
    ) -> Result<RequestAdmission<M>, Box<RejectedInput<M>>> {
        let mut plan = match self.prepare_request(key.clone(), input.clone()).await {
            Ok(plan) => plan,
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery: Delivery::Start,
                    input: UserInput::Message(input),
                    metadata,
                    reason,
                }));
            }
        };
        plan.input = input;
        match self.launch_linked_request(plan, metadata, stop)? {
            RequestLaunch::Existing { receipt, metadata } => {
                Ok(RequestAdmission::Existing { receipt, metadata })
            }
            RequestLaunch::Pending(id) => self.wait_request(id).await,
        }
    }

    fn launch_linked_request(
        &mut self,
        plan: RequestPlan,
        metadata: M,
        stop: CancellationToken,
    ) -> Result<RequestLaunch<M>, Box<RejectedInput<M>>> {
        let RequestPlan {
            session,
            key,
            input,
            existing,
        } = plan;
        let original = UserInput::Message(input);
        let prepared = (|| {
            ensure!(
                session == self.session().id(),
                "request plan belongs to another Session"
            );
            if existing.is_some() {
                return Ok(None);
            }
            tokio::runtime::Handle::try_current()?;
            let admission = self.admit(Delivery::Start, &original, &metadata)?;
            let AdmissionContent::Ready(message) = admission.content else {
                unreachable!("literal messages are prepared")
            };
            let id = InputId(identity());
            let prepared = self
                .budget
                .prepare(message, id, self.setup.runtime.agent.limits())?;
            Ok(Some((id, admission.reservation, prepared)))
        })();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery: Delivery::Start,
                    input: original,
                    metadata,
                    reason,
                }));
            }
        };
        let Some((id, reservation, prepared)) = prepared else {
            return Ok(RequestLaunch::Existing {
                receipt: existing.expect("reconciled plan"),
                metadata,
            });
        };
        let (input, _, inflight) = prepared.into_parts();
        let operation = OperationId(identity());
        let agent = self.setup.runtime.agent.clone();
        let session = self.session().clone();
        let instructions = self.setup.resources.instructions().to_owned();
        let inbox = self.turn_inbox();
        let work_stop = stop.clone();
        let work_inbox = inbox.clone();
        let request = ion_core::SubmissionRequest {
            key: key.clone(),
            input,
        };
        self.begin_admission(
            operation,
            stop,
            inbox,
            AdmissionPayload::Request {
                id,
                key,
                original,
                metadata: SnapshotValue::new(metadata),
                reservation,
                inflight,
            },
            move |observer| async move {
                Ok(AdmissionOutput::Request(
                    agent
                        .submit_request(
                            session,
                            request,
                            instructions,
                            work_stop,
                            Some(work_inbox),
                            observer,
                        )
                        .await?,
                ))
            },
        );
        Ok(RequestLaunch::Pending(id))
    }

    /// Wait only for this reliable decision; unrelated lifecycle updates remain
    /// in their original order. Abandoning this wait leaves the decision retained.
    pub async fn wait_request(
        &mut self,
        id: InputId,
    ) -> Result<RequestAdmission<M>, Box<RejectedInput<M>>> {
        loop {
            let update = lock(&self.custody).take_matching(|update| {
                matches!(update, ControlUpdate::RequestAdmission(decision) if decision.id == id)
            });
            if let Some(ControlUpdate::RequestAdmission(decision)) = update {
                return decision.result;
            }
            let pending = self.admitting.as_ref().is_some_and(|admission| {
                matches!(&admission.payload, AdmissionPayload::Request { id: pending, .. } if *pending == id)
            });
            assert!(
                pending,
                "request decision already taken or identity is not pending"
            );
            drop(self.drive(ActivityRetention::Retain).await);
        }
    }

    /// Continue existing committed work, not another user input. The application
    /// must reconstruct and attest the saved authority before calling this method.
    pub async fn resume_turn(
        &mut self,
        point: ion_core::RecoveryPoint,
        metadata: M,
    ) -> Result<RecoveryAdmission, RecoveryFailure<M>> {
        let operation = self.launch_recovery(point, metadata)?;
        loop {
            let update = lock(&self.custody).take_matching(|update| {
                matches!(update, ControlUpdate::RecoveryAdmission(decision) if decision.operation == operation)
            });
            if let Some(ControlUpdate::RecoveryAdmission(decision)) = update {
                return decision.result;
            }
            drop(self.drive(ActivityRetention::Retain).await);
        }
    }

    pub fn launch_recovery(
        &mut self,
        point: ion_core::RecoveryPoint,
        metadata: M,
    ) -> Result<OperationId, RecoveryFailure<M>> {
        let prepared = (|| {
            tokio::runtime::Handle::try_current()?;
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
            Err(reason) => {
                return Err(RecoveryFailure {
                    point,
                    metadata,
                    reason,
                    kind: AdmissionFailureKind::NotAccepted,
                });
            }
        };
        let operation = OperationId(identity());
        let agent = self.setup.runtime.agent.clone();
        let session = self.session().clone();
        let instructions = self.setup.resources.instructions().to_owned();
        let inbox = self.turn_inbox();
        let stop = self.work_stop();
        let work_stop = stop.clone();
        let work_inbox = inbox.clone();
        self.begin_admission(
            operation,
            stop,
            inbox,
            AdmissionPayload::Recovery {
                point,
                metadata: SnapshotValue::new(metadata),
                reservation,
            },
            move |observer| async move {
                let (point, work) = agent
                    .resume_turn(
                        session,
                        point,
                        instructions,
                        work_stop,
                        Some(work_inbox),
                        observer,
                    )
                    .await?;
                Ok(AdmissionOutput::Recovery { point, work })
            },
        );
        Ok(operation)
    }

    pub(super) fn begin_admission<F, W>(
        &mut self,
        operation: OperationId,
        stop: CancellationToken,
        inbox: Arc<SteeringInbox<InputId>>,
        payload: AdmissionPayload<M>,
        future: F,
    ) where
        F: FnOnce(Box<dyn FnMut(AgentEvent) + Send + 'static>) -> W + Send + 'static,
        W: std::future::Future<Output = Result<AdmissionOutput>> + Send + 'static,
    {
        assert!(
            self.admitting.is_none() && self.active.is_none(),
            "one actual admission owner"
        );
        lock(&self.custody).occupied += 1;
        let prior_stop_requested = self.stop_requested;
        let explicit = !matches!(&payload, AdmissionPayload::Completion);
        let turn_stop = self.bind_turn_stop(stop.clone());
        let observations = AdmissionObservations::new(self.observations.clone(), operation);
        let observer = observations.observer();
        let (launch, ready) = tokio::sync::oneshot::channel();
        let work =
            crate::preparation::Preparation::start(CancellationToken::new(), move |_| async move {
                ready.await.context("admission custody was not installed")?;
                future(observer).await
            });
        self.admitting = Some(Admitting {
            operation,
            stop,
            inbox,
            turn_stop,
            prior_stop_requested,
            payload,
            observations,
            work,
        });
        if explicit {
            self.stop_requested = false;
            self.completions.rearm();
        }
        // Even a multithreaded runtime cannot write before original custody is
        // installed. There is no suspension between installation and this send.
        launch.send(()).expect("retained admission awaits launch");
    }

    pub(super) fn finish_admission(&mut self, result: Result<AdmissionOutput>) {
        let Admitting {
            operation,
            stop,
            inbox,
            turn_stop,
            prior_stop_requested,
            payload,
            observations,
            work,
        } = self
            .admitting
            .take()
            .expect("retained admission preparation");
        drop(work);
        let accepted = matches!(
            &result,
            Ok(AdmissionOutput::Request(ion_core::RequestSubmission {
                operation: Some(_),
                ..
            }) | AdmissionOutput::Recovery { .. }
                | AdmissionOutput::Completion(Some(_))
                | AdmissionOutput::Shell { .. })
        );
        if !accepted && prior_stop_requested {
            self.stop_requested = true;
            self.completions
                .revoke(ion_core::CompletionRevocation::Stopped);
        }
        match payload {
            AdmissionPayload::Request {
                id,
                key,
                original,
                metadata,
                reservation,
                inflight,
            } => {
                drop(inflight);
                match result {
                    Ok(AdmissionOutput::Request(ion_core::RequestSubmission {
                        receipt,
                        operation: Some(work),
                    })) => {
                        let turn = receipt.turn;
                        let fence = work.fence();
                        self.active = Some(Active {
                            id: operation,
                            turn: Some(turn),
                            root: Some(id),
                            work: Work::Turn(work),
                            inbox,
                            automated: false,
                            metadata: None,
                            turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
                        });
                        let mut custody = lock(&self.custody);
                        custody.inputs.insert(
                            id,
                            Input {
                                delivery: Delivery::Start,
                                metadata,
                                reservation,
                                state: InputState::Accepted(InputAcceptance { operation, turn }),
                                steering_status: None,
                            },
                        );
                        custody.push_update(ControlUpdate::RequestAdmission(RequestDecision {
                            id,
                            result: Ok(RequestAdmission::Started { id, receipt }),
                            reservation: None,
                        }));
                        custody.push_update(ControlUpdate::Dispatched { id, operation });
                        custody.push_update(ControlUpdate::Accepted {
                            id,
                            operation,
                            turn,
                        });
                        if !self.stop_requested && !self.closed {
                            custody.steering_target = Some(SteeringTarget {
                                operation,
                                stop,
                                fence,
                            });
                        }
                    }
                    result => {
                        stop.cancel();
                        if result.is_err() {
                            self.completions
                                .revoke(ion_core::CompletionRevocation::OperationFailed);
                        }
                        self.restore_turn_stop(turn_stop);
                        if let Some((_, preparation)) = &self.preparing {
                            preparation.request_stop();
                        }
                        let metadata = metadata.into_inner();
                        let decision = match result {
                            Ok(AdmissionOutput::Request(ion_core::RequestSubmission {
                                receipt,
                                operation: None,
                            })) => Ok(RequestAdmission::Existing { receipt, metadata }),
                            Err(reason) if admission_unknown(&reason) => {
                                Ok(RequestAdmission::Unresolved(UnresolvedRequest {
                                    session: self.session().id(),
                                    key,
                                    id,
                                    operation,
                                    input: original,
                                    metadata,
                                    reason,
                                }))
                            }
                            Err(reason) => Err(Box::new(RejectedInput {
                                delivery: Delivery::Start,
                                input: original,
                                metadata,
                                reason,
                            })),
                            Ok(_) => unreachable!("request worker returns request admission"),
                        };
                        let mut custody = lock(&self.custody);
                        for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
                            pending.withdraw(ReturnReason::PredecessorFailed);
                        }
                        custody.push_update(ControlUpdate::RequestAdmission(RequestDecision {
                            id,
                            result: decision,
                            reservation: Some(reservation),
                        }));
                    }
                }
            }
            AdmissionPayload::Recovery {
                point: expected,
                metadata,
                reservation,
            } => match result {
                Ok(AdmissionOutput::Recovery { point, work }) => {
                    let fence = work.fence();
                    self.active = Some(Active {
                        id: operation,
                        turn: Some(point.turn),
                        root: None,
                        work: Work::Turn(work),
                        inbox,
                        automated: false,
                        metadata: Some((metadata.into_inner(), reservation)),
                        turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
                    });
                    let mut custody = lock(&self.custody);
                    custody.push_update(ControlUpdate::RecoveryAdmission(RecoveryDecision {
                        operation,
                        result: Ok(RecoveryAdmission { operation, point }),
                        reservation: None,
                    }));
                    if !self.stop_requested && !self.closed {
                        custody.steering_target = Some(SteeringTarget {
                            operation,
                            stop,
                            fence,
                        });
                    }
                }
                Err(reason) => {
                    stop.cancel();
                    self.completions
                        .revoke(ion_core::CompletionRevocation::OperationFailed);
                    self.restore_turn_stop(turn_stop);
                    let kind = if admission_unknown(&reason) {
                        AdmissionFailureKind::Unknown
                    } else {
                        AdmissionFailureKind::NotAccepted
                    };
                    if let Some((_, preparation)) = &self.preparing {
                        preparation.request_stop();
                    }
                    let failure = RecoveryFailure {
                        point: expected,
                        metadata: metadata.into_inner(),
                        reason,
                        kind,
                    };
                    let mut custody = lock(&self.custody);
                    for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
                        pending.withdraw(ReturnReason::PredecessorFailed);
                    }
                    custody.push_update(ControlUpdate::RecoveryAdmission(RecoveryDecision {
                        operation,
                        result: Err(failure),
                        reservation: Some(reservation),
                    }));
                }
                Ok(_) => unreachable!("recovery worker returns recovery admission"),
            },
            AdmissionPayload::Shell {
                metadata,
                reservation,
            } => {
                self.finish_shell_preparation(
                    operation,
                    stop,
                    metadata.into_inner(),
                    reservation,
                    result,
                    prior_stop_requested,
                );
            }
            AdmissionPayload::Completion => match result {
                Ok(AdmissionOutput::Completion(Some(ion_core::CompletionSubmission {
                    turn,
                    operation: work,
                }))) => {
                    let fence = work.fence();
                    self.active = Some(Active {
                        id: operation,
                        turn: Some(turn),
                        root: None,
                        work: Work::Turn(work),
                        inbox,
                        automated: true,
                        metadata: None,
                        turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
                    });
                    let mut custody = lock(&self.custody);
                    custody.push_update(ControlUpdate::Automated {
                        operation,
                        turn: Some(turn),
                    });
                    if !self.stop_requested && !self.closed {
                        custody.steering_target = Some(SteeringTarget {
                            operation,
                            stop,
                            fence,
                        });
                    }
                }
                Ok(AdmissionOutput::Completion(None)) => {
                    self.restore_turn_stop(turn_stop);
                    lock(&self.custody).occupied -= 1;
                }
                Err(error) => {
                    self.restore_turn_stop(turn_stop);
                    self.request_stop();
                    let unknown = admission_unknown(&error);
                    let error = error.downcast::<AgentError>().unwrap_or_else(|error| {
                        if unknown {
                            SessionError::Operation(ion_core::OperationError::TaskFailed(
                                error.to_string(),
                            ))
                            .into()
                        } else {
                            SessionError::Storage(error.to_string()).into()
                        }
                    });
                    let mut custody = lock(&self.custody);
                    custody.push_update(ControlUpdate::Automated {
                        operation,
                        turn: None,
                    });
                    custody.push_update(ControlUpdate::Settled(Settlement {
                        automated: true,
                        operation,
                        turn: None,
                        outcome: OperationOutcome::Turn(Err(error)),
                        stop_requested: true,
                        accepted: Vec::new(),
                        metadata: None,
                        reservation: None,
                    }));
                }
                Ok(_) => unreachable!("completion worker returns completion admission"),
            },
        }
        if accepted {
            observations.publish();
        } else {
            observations.discard();
        }
    }

    fn admit(&self, delivery: Delivery, input: &UserInput, metadata: &M) -> Result<Admission> {
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
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
                            .is_some_and(|active| match &active.work {
                                Work::Turn(operation) => !operation.fence().is_finished(),
                                _ => false,
                            }),
                    "steering requires a model Turn"
                );
            }
            Delivery::FollowUp => {
                ensure!(
                    !self.stop_requested,
                    "stop intent suppresses follow-ups; start explicitly"
                );
                if self.current_operation().is_none() {
                    ensure!(
                        self.binding.is_none(),
                        "join or take the binding result before queuing follow-ups"
                    );
                    let _idle = self.setup.session.reserve_idle()?;
                }
            }
        }
        let limits = self.setup.runtime.agent.limits();
        let (message, image_message) = match input {
            UserInput::Message(message) => (message.clone(), None),
            UserInput::Prompt {
                text,
                images,
                files,
            } => {
                ensure!(
                    !text.trim().is_empty() || !images.is_empty() || !files.is_empty(),
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
                let reserve_text =
                    if expanded.trim().is_empty() && files.is_empty() && !images.is_empty() {
                        "[image]".into()
                    } else {
                        expanded.clone()
                    };
                (
                    crate::prompt_files::compose_prompt(reserve_text, files, []),
                    (!images.is_empty())
                        .then(|| crate::prompt_files::compose_prompt(expanded, files, [])),
                )
            }
        };
        let reservation = self.budget.reserve(&message, &(input, metadata), limits)?;
        Ok(Admission {
            reservation,
            content: match image_message {
                Some(message) => AdmissionContent::Images(message),
                None => AdmissionContent::Ready(message),
            },
        })
    }

    /// Start explicit compaction; focus is request-local, never a new User Turn.
    pub fn compact(
        &mut self,
        instructions: Option<String>,
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        let instructions = instructions.filter(|text| !text.trim().is_empty());
        let prepared = (|| {
            self.ensure_replaceable()?;
            ensure!(
                lock(&self.custody).occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            Ok(self.budget.reserve(
                &Message::user_input(instructions.as_deref().unwrap_or("compact").to_owned(), []),
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
            instructions,
            self.setup.session.clone(),
            self.work_stop(),
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
        self.completions.rearm();
        self.active = Some(Active {
            id,
            turn: None,
            root: None,
            work,
            inbox: self.turn_inbox(),
            automated: false,
            metadata: Some((metadata, reservation)),
            turn_stop: None,
        });
    }
}
