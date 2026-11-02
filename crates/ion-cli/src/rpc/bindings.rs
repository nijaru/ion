//! One application-owned binding operation, correlated until physical reply delivery.
use super::*;
use ion_app::{BindingChange, BindingFinished, BindingOperationId, BindingResult};

pub(super) struct BindingReply {
    operation: BindingOperationId,
    id: Option<Value>,
    command: &'static str,
    credit: ReplyCredit,
    data: Option<Value>,
}
impl Control {
    pub(super) fn queue_binding(
        &mut self,
        value: &Value,
        id: Option<Value>,
        command: &str,
        credit: ReplyCredit,
    ) -> Result<()> {
        ensure!(
            self.binding_reply.is_none(),
            "a binding reply is still pending"
        );
        self.idle()?;
        let mut data = None;
        let (command, change) = match command {
            "navigate_context" => {
                let expected = serde_json::from_value(
                    value
                        .get("expected")
                        .context("expected context position is required")?
                        .clone(),
                )?;
                let head_entry = value
                    .get("head_entry")
                    .and_then(Value::as_u64)
                    .context("head_entry must be an unsigned integer")?;
                (
                    "navigate_context",
                    BindingChange::NavigateContext {
                        expected,
                        head_entry,
                    },
                )
            }
            "reload_resources" => ("reload_resources", BindingChange::ReloadResources),
            "set_reasoning" => {
                let reasoning = required_string(value, "effort")?
                    .parse::<ion_ai::Reasoning>()
                    .map_err(anyhow::Error::msg)?;
                data = Some(json!({"reasoning":reasoning.to_string()}));
                ("set_reasoning", BindingChange::SelectReasoning(reasoning))
            }
            "set_model" => (
                "set_model",
                BindingChange::SelectModel(ion_ai::ModelRef {
                    provider: required_string(value, "provider")?.to_owned(),
                    model: required_string(value, "model")?.to_owned(),
                }),
            ),
            "new_session" => ("new_session", BindingChange::NewSession),
            "clone_session" => ("clone_session", BindingChange::CloneSession),
            "fork" => {
                let turn = value
                    .get("turn")
                    .and_then(Value::as_u64)
                    .context("turn must be an unsigned integer")?;
                let after = value
                    .get("after")
                    .map(|value| value.as_bool().context("after must be a boolean"))
                    .transpose()?
                    .unwrap_or(false);
                (
                    "fork",
                    BindingChange::ForkSession(if after {
                        ForkPoint::AfterTurn(turn)
                    } else {
                        ForkPoint::BeforeTurn(turn)
                    }),
                )
            }
            "switch_session" => (
                "switch_session",
                BindingChange::SwitchSession(PathBuf::from(required_string(value, "session")?)),
            ),
            "set_name" => {
                let name = match value.get("name") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_str()
                            .context("name must be a string or null")?
                            .to_owned(),
                    ),
                };
                ("set_name", BindingChange::SetName(name))
            }
            _ => unreachable!("only binding commands are deferred here"),
        };
        let operation = self.host.start_binding(change)?;
        self.binding_reply = Some(BindingReply {
            operation,
            id,
            command,
            credit,
            data,
        });
        Ok(())
    }

    pub(super) fn binding_finished<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        finished: BindingFinished,
        output: &mut Output<W>,
    ) -> Result<()> {
        let reply = self
            .binding_reply
            .take()
            .context("binding result has protocol correlation")?;
        ensure!(
            reply.operation == finished.operation,
            "binding operation identity mismatch"
        );
        let result = finished.result.map(|result| match result {
            BindingResult::ResourcesReloaded => json!({"skills":self.host.resources().skills().count(),"prompts":self.host.resources().templates().count()}),
            BindingResult::ModelSelected => json!({"model":self.host.agent().model()}),
            BindingResult::ReasoningSelected => reply.data.expect("reasoning reply captured its literal effort"),
            BindingResult::SessionNamed { name } => json!({"name":name}),
            BindingResult::ContextNavigated(position) => json!({"position":position,"files_unchanged":true}),
            BindingResult::SessionCreated | BindingResult::SessionCloned { .. } | BindingResult::SessionForked { .. } | BindingResult::SessionSwitched => json!({"session":self.session_id(),"model":self.host.agent().model()}),
        });
        if output.is_healthy() {
            output.enqueue_reliable(
                match result {
                    Ok(data) => success(reply.id, reply.command, data),
                    Err(error) => failure(reply.id, reply.command, &format!("{error:#}")),
                },
                reply.credit,
            )?;
        }
        Ok(())
    }
}
