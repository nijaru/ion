//! Owned, credit-bounded reads. None borrows the controller across native I/O.
use super::*;
use futures_util::future::BoxFuture;

pub(super) enum AccessData {
    Json(Value),
    Encoded(Vec<u8>),
    Request(ion_app::RequestPlan, RpcMetadata, u64),
}
pub(super) struct AccessReply {
    pub id: Option<Value>,
    pub command: &'static str,
    pub credit: ReplyCredit,
    pub result: Result<AccessData>,
}
pub(super) type AccessFuture = BoxFuture<'static, AccessReply>;

impl Control {
    pub(super) fn queue_access(
        &mut self,
        id: Option<Value>,
        command: &'static str,
        credit: ReplyCredit,
        work: impl std::future::Future<Output = Result<AccessData>> + Send + 'static,
    ) {
        self.accesses.push(Box::pin(async move {
            AccessReply {
                id,
                command,
                credit,
                result: work.await,
            }
        }));
    }

    /// The transport owns the credit before any read/materialization starts.
    pub(super) fn defer_command(
        &mut self,
        value: &Value,
        id: Option<Value>,
        command: &str,
        credit: ReplyCredit,
    ) -> Result<bool> {
        match command {
            "submit_request" => self.queue_request(value, id, credit)?,
            "get_request" => self.queue_request_read(value, id, credit)?,
            "inspect" | "get_context" | "list_turns" => {
                let command = match command {
                    "inspect" => "inspect",
                    "get_context" => "get_context",
                    _ => "list_turns",
                };
                let reader = self.host.session().reader()?;
                let reply_id = id.clone();
                self.queue_access(id, command, credit, async move {
                    let result = reader.map_view(move |view| -> Result<AccessData> {
                        let data = match command {
                            "inspect" => inspection::to_value(&view),
                            "get_context" => Ok(json!({"position":view.context_position(),"points":view.context_points().iter().map(|point| json!({"entry":point.entry,"head_entry":point.head_entry,"restore_input":point.restore_input,"selected_path":point.selected_path,"preview":preview_input(&point.message)})).collect::<Vec<_>>()})),
                            _ => Ok(json!(view.turns().iter().map(|item| json!({"turn":item.turn,"preview":preview_input(&item.input),"ended":item.end.is_some()})).collect::<Vec<_>>())),
                        }?;
                        let bytes = crate::publication::encode(crate::publication::Record::Json(success(reply_id, command, data)))?;
                        Ok(AccessData::Encoded(bytes))
                    }).await??;
                    Ok(result)
                });
            }
            "get_state" => {
                // This Host cut precedes committed facts. Never refresh receipt
                // acknowledgements after obtaining an older Session status.
                let state = self.host.state();
                let session = self.host.session().id();
                let model = self.host.agent().model().clone();
                let servers = self.host.mcp_servers();
                let reader = self.host.session().reader()?;
                self.queue_access(id, "get_state", credit, async move {
                    let status = reader.status().await?;
                    let operation = state.operation.map(|operation| match operation.kind {
                        ion_app::OperationKind::Turn => "turn", ion_app::OperationKind::Compact => "compact", ion_app::OperationKind::Shell => "shell",
                    });
                    Ok(AccessData::Json(json!({"session":session,"cwd":status.cwd,"name":status.name,"model":model,"reasoning":status.reasoning.to_string(),"busy":state.busy,"operation":operation,"binding":state.binding,"preparing_input":state.preparing_input,"mcp_reconnect":state.mcp_reconnect,"mcp_servers":servers,"completion_delivery":state.completion_delivery,"entries":status.entries,"follow_ups":state.pending_follow_ups})))
                });
            }
            "list_sessions" => {
                let catalog = self.host.catalog().clone();
                let io = self.host.session().io()?;
                self.queue_access(id, "list_sessions", credit, async move {
                    let value = io.run(move |_| Ok(catalog.list().map(|items| json!(items.iter().map(|item| json!({"id":item.id,"name":item.name,"preview":item.preview,"turns":item.turns,"model":item.model})).collect::<Vec<_>>())))).await??;
                    Ok(AccessData::Json(value))
                });
            }
            "list_models" => {
                let services = self.services.clone();
                let io = self.host.session().io()?;
                self.queue_access(id, "list_models", credit, async move {
                    let value = io.run(move |_| Ok(services.models().choices(services.credentials()).map(|items| json!(items.iter().map(|item| json!({"provider":item.selected.provider,"model":item.selected.model,"label":item.label,"image_input":item.selected.image_input})).collect::<Vec<_>>())))).await??;
                    Ok(AccessData::Json(value))
                });
            }
            "navigate_context" | "reload_resources" | "set_reasoning" | "set_model"
            | "new_session" | "clone_session" | "fork" | "switch_session" | "set_name" => {
                self.queue_binding(value, id, command, credit)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Stop/EOF never admits a request whose read-only lookup was still pending.
    pub(super) fn finish_access(
        &mut self,
        reply: AccessReply,
        admitting: bool,
    ) -> Option<(crate::publication::Record, ReplyCredit)> {
        let AccessReply {
            id,
            command,
            credit,
            result,
        } = reply;
        let result = match result {
            Ok(AccessData::Encoded(bytes)) => {
                return Some((crate::publication::Record::Encoded(bytes), credit));
            }
            Ok(AccessData::Json(value)) => Ok(value),
            Ok(AccessData::Request(plan, metadata, epoch)) => {
                if let Some(receipt) = plan.existing() {
                    return Some((
                        success(
                            id,
                            command,
                            json!({"disposition":"existing","receipt":receipt}),
                        )
                        .into(),
                        credit,
                    ));
                }
                if !admitting || epoch != self.request_epoch {
                    return Some((
                        failure(id, command, "request withdrawn before input admission").into(),
                        credit,
                    ));
                }
                match self.host.launch_request(plan, metadata) {
                    Ok(ion_app::RequestLaunch::Pending(input)) => {
                        self.correlations.insert(
                            input,
                            Correlation {
                                id,
                                command: "submit_request",
                                credit,
                                acknowledged: false,
                                position: 0,
                                turn: None,
                            },
                        );
                        return None;
                    }
                    Ok(ion_app::RequestLaunch::Existing { receipt, .. }) => {
                        Ok(json!({"disposition":"existing","receipt":receipt}))
                    }
                    Err(rejected) => Err(rejected.reason),
                }
            }
            Err(error) => Err(error),
        };
        Some((
            match result {
                Ok(value) => success(id, command, value),
                Err(error) => failure(id, command, &format!("{error:#}")),
            }
            .into(),
            credit,
        ))
    }
}
