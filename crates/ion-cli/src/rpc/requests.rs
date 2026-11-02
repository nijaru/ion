//! Keyed lookup is read-only; actual admission and its decision stay in the app.
use super::*;
use access::AccessData;
use ion_app::{RequestAdmission, RequestDecision};
use ion_core::RequestKey;

impl Control {
    pub(super) fn queue_request(
        &mut self,
        value: &Value,
        id: Option<Value>,
        credit: ReplyCredit,
    ) -> Result<()> {
        fields(value, &["type", "id", "key", "message"])?;
        let key = request_key(value)?;
        let input = ion_ai::Message::user_input(required_string(value, "message")?.to_owned(), []);
        let lookup = self.host.prepare_request(key, input);
        let epoch = self.request_epoch;
        let metadata = RpcMetadata {
            id: id.clone(),
            command: "submit_request",
            credit: credit.clone(),
        };
        self.queue_access(id, "submit_request", credit, async move {
            Ok(AccessData::Request(
                Box::new(lookup.await?),
                metadata,
                epoch,
            ))
        });
        Ok(())
    }
    pub(super) fn queue_request_read(
        &mut self,
        value: &Value,
        id: Option<Value>,
        credit: ReplyCredit,
    ) -> Result<()> {
        fields(value, &["type", "id", "key"])?;
        let key = request_key(value)?;
        let reader = self.host.session().reader()?;
        self.queue_access(id, "get_request", credit, async move {
            Ok(AccessData::Json(
                json!({"receipt":reader.request_receipt(&key).await?}),
            ))
        });
        Ok(())
    }

    pub(super) fn request_decision<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        decision: RequestDecision<RpcMetadata>,
        output: &mut Output<W>,
    ) -> Result<()> {
        let input = decision.id;
        match decision.result {
            Ok(RequestAdmission::Started { id, receipt }) => {
                ensure!(input == id, "request decision input mismatch");
                let item = self
                    .correlations
                    .get_mut(&id)
                    .context("request decision has protocol correlation")?;
                ensure!(!item.acknowledged, "request admission replied once");
                item.acknowledged = true;
                item.turn = Some(receipt.turn);
                if output.is_healthy() {
                    output.enqueue_reliable(
                        success(
                            item.id.clone(),
                            "submit_request",
                            json!({"disposition":"started","receipt":receipt}),
                        ),
                        item.credit.clone(),
                    )?;
                }
            }
            Ok(RequestAdmission::Existing { receipt, metadata }) => {
                self.correlations
                    .remove(&input)
                    .context("existing request has pending correlation")?;
                if output.is_healthy() {
                    output.enqueue_reliable(
                        success(
                            metadata.id,
                            metadata.command,
                            json!({"disposition":"existing","receipt":receipt}),
                        ),
                        metadata.credit,
                    )?;
                }
            }
            Ok(RequestAdmission::Unresolved(input)) => {
                self.correlations
                    .remove(&input.id)
                    .context("unresolved request has pending correlation")?;
                if output.is_healthy() {
                    output.enqueue_reliable(failure(input.metadata.id.clone(), input.metadata.command, &format!("admission unresolved; reconcile the same key before retrying: {:#}", input.reason)), input.metadata.credit.clone())?;
                    output.enqueue_reliable(json!({"type":"unresolved_input","id":input.metadata.id,"kind":"submit_request","session":input.session,"operation":input.operation,"key":input.key,"input":input.input,"can_retry":false}), input.metadata.credit)?;
                }
            }
            Err(rejected) => {
                self.correlations
                    .remove(&input)
                    .context("rejected request has pending correlation")?;
                if output.is_healthy() {
                    let mut record = failure(
                        rejected.metadata.id.clone(),
                        rejected.metadata.command,
                        &format!("{:#}", rejected.reason),
                    );
                    record["input"] = json!(rejected.input);
                    record["disposition"] = json!("not_accepted");
                    output.enqueue_reliable(record, rejected.metadata.credit)?;
                }
            }
        }
        Ok(())
    }
}
fn request_key(value: &Value) -> Result<RequestKey> {
    serde_json::from_value(value.get("key").context("key is required")?.clone())
        .context("invalid request key")
}
