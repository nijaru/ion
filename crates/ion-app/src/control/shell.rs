//! Cold native preflight stays outside the Session's execution registry.
use super::*;

impl<M: Serialize + Send + 'static> SessionControl<M> {
    /// Local identity and retained preparation, not durable shell acceptance.
    pub fn shell(
        &mut self,
        command: String,
        exclude_from_context: bool,
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        self.launch_shell(command, metadata, move |command| {
            Ok((command.to_owned(), exclude_from_context))
        })
    }

    /// Resolve and revalidate the frozen manifest on the native worker. No
    /// fallback to another definition is possible after that binding is captured.
    pub fn command(
        &mut self,
        name: &str,
        args: &[String],
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        let catalog = self.setup.commands.clone();
        let name = name.to_owned();
        let args = args.to_owned();
        // Account for literal caller input before moving it to the worker.
        let literal = format!("{name} {}", args.join(" "));
        self.launch_shell(literal, metadata, move |_| {
            let mapped = catalog.resolve(&name, &args)?;
            Ok((mapped.command, !mapped.share_with_model))
        })
    }

    fn launch_shell(
        &mut self,
        literal: String,
        metadata: M,
        resolve: impl FnOnce(&str) -> Result<(String, bool)> + Send + 'static,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        let prepared = (|| {
            self.ensure_replaceable()?;
            ensure!(
                lock(&self.custody).occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            let shell = self
                .setup
                .runtime
                .shell
                .clone()
                .context("direct shell is not enabled by this application")?;
            tokio::runtime::Handle::try_current()?;
            let reservation = self.budget.reserve(
                &Message::user_input(literal.clone(), []),
                &metadata,
                self.setup.runtime.agent.limits(),
            )?;
            Ok((shell, reservation))
        })();
        let (shell, reservation) = match prepared {
            Ok(value) => value,
            Err(error) => return Err((metadata, error)),
        };
        let operation = OperationId(identity());
        let stop = self.work_stop();
        let observations = AdmissionObservations::new(self.observations.clone(), operation);
        let prior_stop_requested = self.stop_requested;
        lock(&self.custody).occupied += 1;
        self.admitting = Some(Admitting {
            operation,
            stop: stop.clone(),
            inbox: self.turn_inbox(),
            // A direct shell owns no model Turn or question grant.
            turn_stop: None,
            prior_stop_requested,
            payload: AdmissionPayload::Shell {
                metadata: SnapshotValue::new(metadata),
                reservation,
            },
            observations,
            // Joining/dropping preparation must not stop the adopted Core work.
            work: crate::preparation::Preparation::pending(stop.child_token()),
        });
        self.stop_requested = false;
        self.completions.rearm();
        self.admitting
            .as_mut()
            .expect("installed cold preflight")
            .work
            .launch_blocking(move |stop| {
                ensure!(
                    !stop.is_cancelled(),
                    "shell preflight stopped before dispatch"
                );
                let (command, exclude_from_context) = resolve(&literal)?;
                let remapped = command != literal;
                let prepared = shell.prepare(&command)?;
                // PreparedShell is cold. It may capture this same Session; returning
                // it here avoids a registry -> worker -> Session ownership cycle.
                Ok(AdmissionOutput::Shell {
                    command,
                    exclude_from_context,
                    remapped,
                    prepared,
                })
            });
        Ok(operation)
    }

    pub(super) fn finish_shell_preparation(
        &mut self,
        operation: OperationId,
        stop: CancellationToken,
        metadata: M,
        reservation: InputReservation,
        result: Result<AdmissionOutput>,
        prior_stop_requested: bool,
    ) {
        let prepared = (|| {
            let AdmissionOutput::Shell {
                command,
                exclude_from_context,
                remapped,
                prepared,
            } = result?
            else {
                unreachable!("shell worker returns only cold shell preflight")
            };
            ensure!(
                !stop.is_cancelled(),
                "shell preflight stopped before dispatch"
            );
            // Mapping may expand literal argv. Revalidate the actual command's
            // budget before Core can persist intent or invoke the cold callback.
            let mapped = if remapped {
                Some(self.budget.reserve(
                    &Message::user_input(command.clone(), []),
                    &metadata,
                    self.setup.runtime.agent.limits(),
                )?)
            } else {
                None
            };
            Ok((command, exclude_from_context, prepared, mapped))
        })();
        match prepared {
            Ok((command, exclude_from_context, prepared, mapped)) => {
                let reservation = mapped.unwrap_or(reservation);
                let work = self.setup.session.run_user_shell(
                    command,
                    exclude_from_context,
                    stop,
                    move |_, stop, artifacts| prepared(stop, artifacts),
                );
                // Do not rearm or clear a stop requested during preparation.
                self.active = Some(Active {
                    id: operation,
                    turn: None,
                    root: None,
                    work: Work::Shell(work),
                    inbox: self.turn_inbox(),
                    automated: false,
                    metadata: Some((metadata, reservation)),
                    turn_stop: None,
                });
            }
            Err(error) => {
                if prior_stop_requested {
                    self.stop_requested = true;
                    self.completions
                        .revoke(ion_core::CompletionRevocation::Stopped);
                }
                stop.cancel();
                if let Some((_, preparation)) = &self.preparing {
                    preparation.request_stop();
                }
                self.completions
                    .revoke(ion_core::CompletionRevocation::OperationFailed);
                let mut custody = lock(&self.custody);
                for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
                    pending.withdraw(ReturnReason::PredecessorFailed);
                }
                custody.push_update(ControlUpdate::Settled(Settlement {
                    operation,
                    turn: None,
                    automated: false,
                    outcome: OperationOutcome::NotDispatched(error),
                    stop_requested: self.stop_requested,
                    accepted: Vec::new(),
                    metadata: Some(metadata),
                    reservation: Some(reservation),
                }));
            }
        }
    }
}
