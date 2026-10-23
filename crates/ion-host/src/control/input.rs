//! Queue transitions and preparation, independent of Core execution.
use super::*;

impl<M: Serialize + Send + 'static> SessionControl<M> {
    /// Stop intent is retained even if Core has already completed successfully.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
        lock(&self.custody).steering_target = None;
        if let Some(questions) = &self.setup.questions {
            questions.cancel_pending(self.owner, "stopped");
        }
        if let Some(active) = &self.active {
            active.work.stop();
        }
        if let Some((_, preparing)) = &self.preparing {
            preparing.request_stop();
        }
        let mut custody = lock(&self.custody);
        for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
            pending.withdrawn = Some(ReturnReason::Stopped);
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
            if input.delivery != Delivery::Start
                && active_root != Some(id)
                && let Some(pending) = input.pending_mut()
            {
                pending.withdrawn = Some(ReturnReason::Cleared);
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
                    && input
                        .pending()
                        .is_some_and(|pending| pending.withdrawn.is_none())
                    && active_root != Some(**id)
            })?;
            input
                .pending_mut()
                .expect("selected pending follow-up")
                .withdrawn = Some(ReturnReason::Cleared);
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
        if self
            .background()
            .is_some_and(|owner| owner.ensure_open().is_err())
        {
            self.request_shutdown();
        }
        let mut custody = lock(&self.custody);
        // A transferred steer stays in the inbox until Core's callback or a
        // synchronized withdrawal. Never return a committed-but-not-yet-observed input.
        let withdrawn: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| {
                let pending = input.pending()?;
                (pending.withdrawn.is_some()
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
                .pending_mut()
                .expect("selected pending withdrawal")
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
                    if input.pending().is_some()
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
            && let Some((id, pending, text)) = custody.inputs.iter().find_map(|(&id, input)| {
                let pending = input.pending()?;
                let InputContent::Images(text) = &pending.content else {
                    return None;
                };
                pending.withdrawn.is_none().then_some((id, pending, text))
            })
        {
            let UserInput::Prompt { images, .. } = &pending.original else {
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
            let text = text.clone();
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
                        && input.pending().is_some_and(|pending| {
                            !matches!(pending.content, InputContent::Transferred { .. })
                        }))
                    .then_some(id)
                })
                .collect();
            for id in ids {
                let pending = custody
                    .inputs
                    .get_mut(&id)
                    .unwrap()
                    .pending_mut()
                    .expect("selected pending steering");
                if pending.withdrawn.is_some() {
                    continue;
                }
                let Some(prepared) = pending.take_ready() else {
                    break;
                };
                custody.steering_order.push_back(id);
                active.inbox.push_admitted(prepared);
            }
        }
        if self.active.is_none() && !self.closed && !self.stop_requested {
            let next = self.starting.or_else(|| {
                custody.inputs.iter().find_map(|(&id, input)| {
                    (input.delivery == Delivery::FollowUp
                        && input
                            .pending()
                            .is_some_and(|pending| pending.withdrawn.is_none()))
                    .then(|| (id, OperationId(identity())))
                })
            });
            if let Some((root, operation)) = next
                && let Some(prepared) = custody
                    .inputs
                    .get_mut(&root)
                    .and_then(|input| input.pending_mut()?.take_ready())
            {
                let (message, _, reservation) = prepared.into_parts();
                // Core owns its message; the Host original and metadata remain
                // until the post-persistence callback, without text matching.
                let pending = custody
                    .inputs
                    .get_mut(&root)
                    .unwrap()
                    .pending_mut()
                    .expect("dispatched root retains its original until acceptance");
                pending.content = InputContent::Transferred {
                    _reservation: Some(reservation),
                };
                let instructions = pending.instructions.clone();
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
                let stop = self.work_stop();
                let turn_stop = self
                    .bind_turn_stop(stop.clone())
                    .map(|binding| binding.scope());
                custody.steering_target = Some(SteeringTarget {
                    operation,
                    stop: stop.clone(),
                });
                let work = self.setup.runtime.agent.submit_message_with_steering(
                    self.setup.session.clone(),
                    message,
                    instructions,
                    stop,
                    inbox.clone(),
                    turn_observer(state, observations, Some(root), operation),
                );
                self.starting = None;
                self.active = Some(Active {
                    id: operation,
                    turn: None,
                    root: Some(root),
                    work: Work::Turn(work),
                    inbox,
                    metadata: None,
                    turn_stop,
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
        let delivery = input.delivery;
        let pending = input
            .pending_mut()
            .expect("preparation precedes acknowledgement");
        let reason = pending.withdrawn.take().or_else(|| {
            if self.stop_requested || self.closed {
                Some(ReturnReason::Stopped)
            } else if let Delivery::Steer(target) = delivery {
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
                    pending.content = InputContent::Ready(prepared);
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
