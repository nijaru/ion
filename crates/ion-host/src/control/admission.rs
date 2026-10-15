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
                        accepted: false,
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
                            .is_some_and(|active| active.root.is_some()),
                    "steering requires a coding Turn"
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
        let limits = self.setup.agent.limits();
        let (message, expanded, ready) = match input {
            UserInput::Message(message) => (message.clone(), None, true),
            UserInput::Prompt { text, images } => {
                ensure!(
                    !text.trim().is_empty() || !images.is_empty(),
                    "prompt is empty"
                );
                if !images.is_empty() {
                    crate::image_input::require_image_input(&self.setup.selected)?;
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
            let tools = LocalTools::new(self.setup.session.cwd())?;
            let reservation = self.budget.reserve(
                &Message::user_input(command.clone(), []),
                &metadata,
                self.setup.agent.limits(),
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
            move |command, stop| async move { tools.run_user_shell(&command, stop).await },
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
                self.setup.agent.limits(),
            )?)
        })();
        let reservation = match prepared {
            Ok(value) => value,
            Err(error) => return Err((metadata, error)),
        };
        let id = OperationId(identity());
        let observations = self.observations.clone();
        lock(&self.custody).occupied += 1;
        let work = self.setup.agent.compact(
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
            root: None,
            work,
            inbox: Arc::new(SteeringInbox::new(
                self.setup.agent.limits(),
                self.budget.clone(),
            )),
            metadata: Some((metadata, reservation)),
        });
    }
}
