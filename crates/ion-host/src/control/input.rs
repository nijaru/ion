//! Queue transitions and preparation, independent of Core execution.
use super::*;

impl<M: Serialize + Send + 'static> SessionControl<M> {
    /// Stop intent is retained even if Core has already completed successfully.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
        if let Some(active) = &self.active {
            active.work.stop();
        }
        if let Some((_, preparing)) = &self.preparing {
            preparing.request_stop();
        }
        let mut custody = lock(&self.custody);
        for input in custody.inputs.values_mut().filter(|input| !input.accepted) {
            input.withdrawn = Some(ReturnReason::Stopped);
        }
        drop(custody);
        self.withdraw_inbox(ReturnReason::Stopped);
    }

    /// Clears queued intent, including its preparation, but not an unrelated
    /// initial prompt. Inputs already atomically committed cannot be withdrawn.
    pub fn clear_queue(&mut self) {
        let mut custody = lock(&self.custody);
        let active_root = self.active.as_ref().and_then(|active| active.root);
        for (&id, input) in &mut custody.inputs {
            if !input.accepted && input.delivery != Delivery::Start && active_root != Some(id) {
                input.withdrawn = Some(ReturnReason::Cleared);
            }
        }
        if let Some((id, preparation)) = &self.preparing
            && custody
                .inputs
                .get(id)
                .is_some_and(|input| input.delivery != Delivery::Start)
        {
            preparation.request_stop();
        }
        drop(custody);
        self.withdraw_inbox(ReturnReason::Cleared);
        self.advance();
    }

    /// Withdraw just the newest queued follow-up. A running root is never a
    /// queue item; a preparing input returns only after its worker has joined.
    pub fn withdraw_last_follow_up(&mut self) -> Option<InputId> {
        let active_root = self.active.as_ref().and_then(|active| active.root);
        let id = {
            let mut custody = lock(&self.custody);
            let (&id, input) = custody.inputs.iter_mut().rev().find(|(id, input)| {
                input.delivery == Delivery::FollowUp
                    && !input.accepted
                    && input.withdrawn.is_none()
                    && active_root != Some(**id)
            })?;
            input.withdrawn = Some(ReturnReason::Cleared);
            id
        };
        if let Some((preparing, preparation)) = &self.preparing
            && *preparing == id
        {
            preparation.request_stop();
        }
        self.advance();
        Some(id)
    }

    pub(super) fn withdraw_inbox(&mut self, reason: ReturnReason) {
        let Some(active) = &self.active else {
            return;
        };
        let returned = active.inbox.take_uncommitted();
        let mut custody = lock(&self.custody);
        for input in returned {
            let (_, id, reservation) = input.into_parts();
            drop(reservation);
            custody.steering_order.retain(|queued| *queued != id);
            let reason = match reason {
                ReturnReason::Cleared => ReturnReason::Cleared,
                _ => ReturnReason::Stopped,
            };
            custody.return_input(id, reason);
        }
    }

    pub(super) fn advance(&mut self) {
        let mut custody = lock(&self.custody);
        // A transferred steer stays in the inbox until Core's callback or a
        // synchronized withdrawal. Never return a committed-but-not-yet-observed input.
        let withdrawn: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| {
                (!input.accepted
                    && input.withdrawn.is_some()
                    && !custody.steering_order.contains(&id)
                    && self
                        .preparing
                        .as_ref()
                        .is_none_or(|(preparing, _)| *preparing != id)
                    && self
                        .active
                        .as_ref()
                        .is_none_or(|active| active.root != Some(id)))
                .then_some(id)
            })
            .collect();
        for id in withdrawn {
            let reason = custody
                .inputs
                .get_mut(&id)
                .unwrap()
                .withdrawn
                .take()
                .unwrap();
            custody.return_input(id, reason);
            if self.starting.is_some_and(|(root, _)| root == id) {
                self.starting = None;
            }
        }
        let stale: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| match input.delivery {
                Delivery::Steer(target)
                    if !input.accepted
                        && self.current_operation() != Some(target)
                        && self
                            .preparing
                            .as_ref()
                            .is_none_or(|(preparing, _)| *preparing != id) =>
                {
                    Some(id)
                }
                _ => None,
            })
            .collect();
        for id in stale {
            custody.return_input(id, ReturnReason::TurnEnded);
        }

        // Preparation overlaps Core execution and never occupies Core's slot.
        if self.preparing.is_none()
            && let Some((&id, input)) = custody.inputs.iter().find(|(_, input)| {
                !input.accepted
                    && input.prepared.is_none()
                    && input.expanded.is_some()
                    && input.withdrawn.is_none()
                    && input.original.is_some()
            })
        {
            let UserInput::Prompt { images, .. } = input.original.as_ref().unwrap() else {
                unreachable!("only raw prompts need preparation")
            };
            let mut sources = images.clone();
            for source in &mut sources {
                if let ImageSource::Path(path) = source
                    && !path.is_absolute()
                {
                    *path = self
                        .setup
                        .session
                        .cwd()
                        .expect("relative image admission requires a workspace")
                        .join(&*path);
                }
            }
            let text = input.expanded.as_ref().unwrap().clone();
            let model = self.setup.runtime.agent.model().clone();
            let limits = self.setup.runtime.agent.limits();
            self.preparing = Some((
                id,
                crate::preparation::Preparation::start(move |stop| async move {
                    let images = crate::image_input::prepare_images(
                        &model,
                        limits.image_input,
                        sources,
                        limits.max_request_bytes,
                        stop,
                    )
                    .await?;
                    Ok(Message::user_input(text, images))
                }),
            ));
        }
        // Transfer prepared steering in admission order. A later ready steer
        // cannot overtake an earlier still-preparing steer.
        if let Some(active) = &self.active {
            let ids: Vec<_> = custody
                .inputs
                .iter()
                .filter_map(|(&id, input)| {
                    (input.delivery == Delivery::Steer(active.id)
                        && !input.accepted
                        && !custody.steering_order.contains(&id))
                    .then_some(id)
                })
                .collect();
            for id in ids {
                let input = custody.inputs.get_mut(&id).unwrap();
                if input.withdrawn.is_some() {
                    continue;
                }
                let Some(prepared) = input.prepared.take() else {
                    break;
                };
                // Prevent the preparation scan from treating transferred input as raw.
                input.expanded = None;
                custody.steering_order.push_back(id);
                active.inbox.push_admitted(prepared);
            }
        }
        if self.active.is_none() && !self.closed && !self.stop_requested {
            let next = self.starting.or_else(|| {
                custody.inputs.iter().find_map(|(&id, input)| {
                    (input.delivery == Delivery::FollowUp && input.withdrawn.is_none())
                        .then(|| (id, OperationId(identity())))
                })
            });
            if let Some((root, operation)) = next
                && let Some(prepared) = custody
                    .inputs
                    .get_mut(&root)
                    .and_then(|input| input.prepared.take())
            {
                let (message, _, reservation) = prepared.into_parts();
                // Core owns its message; the Host original and metadata remain
                // until the post-persistence callback, without text matching.
                let input = custody.inputs.get_mut(&root).unwrap();
                input.inflight = Some(reservation);
                // Transferred roots are no longer preparation candidates, even
                // before Core's durable acceptance callback releases the original.
                input.expanded = None;
                let instructions = input.instructions.clone();
                let inbox = Arc::new(SteeringInbox::new(
                    self.setup.runtime.agent.limits(),
                    self.budget.clone(),
                ));
                let state = self.custody.clone();
                let observations = self.observations.clone();
                custody.updates.push_back(ControlUpdate::Dispatched {
                    id: root,
                    operation,
                });
                let work = self.setup.runtime.agent.submit_message_with_steering(
                    self.setup.session.clone(),
                    message,
                    instructions,
                    CancellationToken::new(),
                    inbox.clone(),
                    turn_observer(state, observations, Some(root), operation),
                );
                self.starting = None;
                self.active = Some(Active {
                    id: operation,
                    root: Some(root),
                    work: Work::Turn(work),
                    inbox,
                    metadata: None,
                });
            }
        }
    }

    pub(super) fn finish_preparation(&mut self, result: Result<Message>) {
        let (id, _) = self.preparing.take().expect("retained preparation");
        let mut failed_start = false;
        let mut custody = lock(&self.custody);
        let input = custody
            .inputs
            .get_mut(&id)
            .expect("preparation retains original");
        let reason = input.withdrawn.take().or_else(|| {
            if self.stop_requested || self.closed {
                Some(ReturnReason::Stopped)
            } else if let Delivery::Steer(target) = input.delivery {
                (self.active.as_ref().map(|active| active.id) != Some(target)
                    && self.starting.map(|(_, op)| op) != Some(target))
                .then_some(ReturnReason::TurnEnded)
            } else {
                None
            }
        });
        if let Some(reason) = reason {
            custody.return_input(id, reason);
            if let Some(ControlUpdate::Returned(input)) = custody.updates.back_mut() {
                input.preparation_error = result.err();
            }
        } else {
            match result.and_then(|message| {
                Ok(self
                    .budget
                    .admit(message, id, self.setup.runtime.agent.limits())?)
            }) {
                Ok(prepared) => {
                    let delivery = input.delivery;
                    input.prepared = Some(prepared);
                    custody.updates.push_back(ControlUpdate::Input {
                        id,
                        delivery,
                        phase: InputPhase::Queued,
                    });
                }
                Err(error) => {
                    custody.return_input(id, ReturnReason::Preparation(error));
                    failed_start = self.starting.is_some_and(|(root, _)| root == id);
                }
            }
        }
        if !custody.inputs.contains_key(&id) && self.starting.is_some_and(|(root, _)| root == id) {
            self.starting = None;
        }
        drop(custody);
        if failed_start {
            // Queued work was admitted behind this attempted start, not as an
            // independent replacement for an input that never entered Session.
            self.request_stop();
        }
    }
}
