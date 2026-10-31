//! Validation and result-capacity reservation before effects.
use super::*;

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
                                .admit(message, id, self.setup.runtime.agent.limits())?;
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
    pub fn submit_request(
        &mut self,
        key: &ion_core::RequestKey,
        input: Message,
        metadata: M,
    ) -> Result<RequestAdmission<M>, Box<RejectedInput<M>>> {
        self.submit_linked_request(key, input, metadata, self.work_stop())
    }

    pub(crate) fn submit_linked_request(
        &mut self,
        key: &ion_core::RequestKey,
        input: Message,
        metadata: M,
        stop: CancellationToken,
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
        let admission = match self.admit(Delivery::Start, &original, &metadata) {
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
        let AdmissionContent::Ready(message) = admission.content else {
            unreachable!("literal messages are prepared")
        };
        let prepared = match self
            .budget
            .admit(message, id, self.setup.runtime.agent.limits())
        {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(Box::new(RejectedInput {
                    delivery: Delivery::Start,
                    input: original,
                    metadata,
                    reason: reason.into(),
                }));
            }
        };
        let (message, _, inflight) = prepared.into_parts();
        let operation = OperationId(identity());
        let inbox = self.turn_inbox();
        let instructions = self.setup.resources.instructions().to_owned();
        let mut custody = lock(&self.custody);
        custody.inputs.insert(
            id,
            Input {
                delivery: Delivery::Start,
                metadata: SnapshotValue::new(metadata),
                reservation: admission.reservation,
                state: InputState::Pending(Box::new(PendingInput {
                    original,
                    instructions: instructions.clone(),
                    content: InputContent::Transferred {
                        _reservation: Some(inflight),
                    },
                    withdrawn: None,
                })),
                steering_status: None,
            },
        );
        // Hold custody until the callback has a complete root to acknowledge.
        let turn_stop = self.bind_turn_stop(stop.clone());
        self.completions.rearm();
        let result = self.setup.runtime.agent.submit_request(
            self.setup.session.clone(),
            ion_core::SubmissionRequest {
                key: key.clone(),
                input: message,
            },
            instructions,
            stop.clone(),
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
                custody.push_update(ControlUpdate::Dispatched { id, operation });
                custody.steering_target = Some(SteeringTarget {
                    operation,
                    stop: stop.clone(),
                });
                self.stop_requested = false;
                self.active = Some(Active {
                    id: operation,
                    turn: Some(receipt.turn),
                    root: Some(id),
                    work: Work::Turn(work),
                    inbox,
                    automated: false,
                    metadata: None,
                    turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
                });
                Ok(RequestAdmission::Started { id, receipt })
            }
            result => {
                self.restore_turn_stop(turn_stop);
                let input = custody
                    .inputs
                    .remove(&id)
                    .expect("unaccepted request retains custody");
                match result {
                    Ok(ion_core::RequestSubmission { receipt, .. }) => {
                        Ok(RequestAdmission::Existing {
                            receipt,
                            metadata: input.metadata.into_inner(),
                        })
                    }
                    Err(reason) => Err(Box::new(RejectedInput {
                        delivery: Delivery::Start,
                        input: match input.state {
                            InputState::Pending(pending) => pending.original,
                            InputState::Accepted(_) => {
                                unreachable!("rejected request was not accepted")
                            }
                        },
                        metadata: input.metadata.into_inner(),
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
        let inbox = self.turn_inbox();
        let stop = self.work_stop();
        let turn_stop = self.bind_turn_stop(stop.clone());
        // Publish the lease under the same fence as acceptance/end callbacks.
        let mut custody = lock(&self.custody);
        self.completions.rearm();
        let result = self.setup.runtime.agent.resume_turn(
            self.setup.session.clone(),
            point,
            self.setup.resources.instructions().to_owned(),
            stop.clone(),
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
            Err(error) => {
                drop(custody);
                self.restore_turn_stop(turn_stop);
                return Err((metadata, error.into()));
            }
        };
        custody.occupied += 1;
        custody.steering_target = Some(SteeringTarget { operation, stop });
        drop(custody);
        self.stop_requested = false;
        self.active = Some(Active {
            id: operation,
            turn: Some(point.turn),
            root: None,
            work: Work::Turn(work),
            inbox,
            automated: false,
            metadata: Some((metadata, reservation)),
            turn_stop: turn_stop.as_ref().map(|binding| binding.scope()),
        });
        Ok(RecoveryAdmission { operation, point })
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
            self.work_stop(),
            move |_, stop| tools(stop),
        );
        self.install_direct(id, Work::Shell(work), metadata, reservation);
        Ok(id)
    }

    /// Invoke one enabled user command with literal argv. The frozen catalog is
    /// resolved first, then its manifest bytes and project grant are revalidated
    /// against the current filesystem before the existing direct-shell preflight
    /// and durable admission. A changed, revoked or deleted source is rejected;
    /// dispatch never falls back to another definition.
    pub fn command(
        &mut self,
        name: &str,
        args: &[String],
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        if let Err(error) = self.ensure_replaceable() {
            return Err((metadata, error));
        }
        let mapped = match self.setup.commands.resolve(name, args) {
            Ok(mapped) => mapped,
            Err(error) => return Err((metadata, error)),
        };
        self.shell(mapped.command, !mapped.share_with_model, metadata)
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
