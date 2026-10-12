//! Connection-owned input preparation, distinct from accepted Core inputs.
use ion_host::image_input::ImageSource;

use super::*;
use tokio::task::JoinHandle;

pub(super) enum InputKind {
    Prompt,
    Steer(Arc<SteeringInbox>),
    FollowUp,
}

impl InputKind {
    fn command(&self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer(_) => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

pub(super) struct PendingInput {
    pub kind: InputKind,
    pub id: Option<Value>,
    pub stop: CancellationToken,
    pub withdraw_follow_ups: bool,
    pub task: JoinHandle<Result<Message>>,
}

impl Control {
    pub(super) fn prepare_input(
        &mut self,
        value: &Value,
        id: Option<Value>,
        command: &str,
    ) -> Result<Option<Value>> {
        ensure!(
            self.pending_input.is_none(),
            "input preparation is already in progress"
        );
        let kind = match command {
            "prompt" => {
                self.idle()?;
                InputKind::Prompt
            }
            "steer" | "follow_up" => {
                let active = self
                    .active
                    .as_ref()
                    .context("no active Turn; use prompt instead")?;
                ensure!(
                    !active.stop.is_cancelled(),
                    "operation is stopping; use prompt after settlement"
                );
                let steering = active
                    .operation
                    .steering()
                    .context("no active Turn; use prompt instead")?;
                if command == "steer" {
                    InputKind::Steer(steering.clone())
                } else {
                    InputKind::FollowUp
                }
            }
            _ => unreachable!("input command"),
        };
        let prompt = expand_input(
            self.binding.resources(),
            required_string(value, "message")?.to_owned(),
        )?;
        let sources = value.get("images").map_or(Ok(Vec::new()), |images| {
            images
                .as_array()
                .context("images must be an array of local paths or inline images")?
                .iter()
                .map(|image| {
                    if let Some(path) = image.as_str() {
                        Ok(ImageSource::Path(PathBuf::from(path)))
                    } else {
                        Ok(ImageSource::Encoded {
                            mime_type: required_string(image, "mime_type")?.to_owned(),
                            data: required_string(image, "data")?.to_owned(),
                        })
                    }
                })
                .collect::<Result<Vec<_>>>()
        })?;
        ensure!(
            !prompt.trim().is_empty() || !sources.is_empty(),
            "message is empty"
        );
        if sources.is_empty() {
            return self.admit_prepared(kind, Message::user_input(prompt, []), id);
        }
        ion_host::image_input::require_image_input(self.binding.selected())?;
        let stop = CancellationToken::new();
        let preparation = self.binding.prepare_images(sources, stop.clone());
        let task = tokio::spawn(async move { Ok(Message::user_input(prompt, preparation.await?)) });
        self.pending_input = Some(PendingInput {
            kind,
            id,
            stop,
            withdraw_follow_ups: false,
            task,
        });
        Ok(None)
    }

    fn admit_prepared(
        &mut self,
        kind: InputKind,
        input: Message,
        id: Option<Value>,
    ) -> Result<Option<Value>> {
        let command = kind.command();
        let data = match kind {
            InputKind::Prompt => {
                self.start_message(input, id, None)?;
                return Ok(None);
            }
            InputKind::Steer(target) => {
                ensure!(
                    self.active
                        .as_ref()
                        .and_then(|active| active.operation.steering())
                        .is_some_and(|current| Arc::ptr_eq(current, &target)),
                    "original Turn has ended"
                );
                target.push_message(input, ())?;
                json!({"disposition":"queued"})
            }
            InputKind::FollowUp => {
                let input =
                    self.input_budget
                        .admit(input, id.clone(), self.binding.agent().limits())?;
                self.follow_ups.push_back(input);
                json!({"disposition":"queued","position":self.follow_ups.len()})
            }
        };
        Ok(Some(success(id, command, data)))
    }

    pub(super) fn finish_preparation(
        &mut self,
        joined: std::result::Result<Result<Message>, tokio::task::JoinError>,
    ) -> Completion {
        let pending = self.pending_input.take().expect("joined input preparation");
        let command = pending.kind.command();
        let return_follow_ups = pending.withdraw_follow_ups && self.active.is_none();
        let (input, error) = match joined {
            Ok(input) => (input, None),
            Err(error) => {
                let error = anyhow::Error::new(error).context("RPC input preparation task failed");
                (Err(anyhow!("{error:#}")), Some(error))
            }
        };
        let outcome = input.and_then(|input| {
            ensure!(!pending.stop.is_cancelled(), "input preparation cancelled");
            self.admit_prepared(pending.kind, input, pending.id.clone())
        });
        let record = match outcome {
            Ok(record) => record,
            Err(failure_error) => Some(failure(pending.id, command, &format!("{failure_error:#}"))),
        };
        let mut records = record.into_iter().collect::<Vec<_>>();
        // Abort may target preparation after the preceding Turn already joined.
        // It still withdraws dispatch of input waiting behind that preparation.
        if return_follow_ups {
            records.extend(self.take_uncommitted_follow_ups());
        }
        Completion { records, error }
    }

    pub(super) fn cancel_preparation(&mut self) {
        if let Some(pending) = &mut self.pending_input {
            pending.withdraw_follow_ups = true;
            pending.stop.cancel();
        }
    }
}
