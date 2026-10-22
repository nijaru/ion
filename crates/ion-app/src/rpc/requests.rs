//! Literal keyed delivery; the shared Host/Core retain admission and execution.
use super::*;
use ion_core::RequestKey;
use ion_host::RequestAdmission;

impl Control {
    pub(super) fn submit_request(
        &mut self,
        value: &Value,
        id: Option<Value>,
        credit: ReplyCredit,
    ) -> Result<Value> {
        fields(value, &["type", "id", "key", "message"])?;
        let key = request_key(value)?;
        let input = ion_ai::Message::user_input(required_string(value, "message")?.to_owned(), []);
        let command = "submit_request";
        let metadata = RpcMetadata {
            id: id.clone(),
            command,
            credit: credit.clone(),
        };
        let (disposition, receipt) = match self
            .host
            .submit_request(&key, input, metadata)
            .map_err(|rejected| rejected.reason)?
        {
            RequestAdmission::Started { id: input, receipt } => {
                self.correlations.insert(
                    input,
                    Correlation {
                        id,
                        command,
                        credit,
                        acknowledged: true,
                        position: 0,
                        turn: Some(receipt.turn),
                    },
                );
                ("started", receipt)
            }
            RequestAdmission::Existing { receipt, .. } => ("existing", receipt),
        };
        Ok(json!({"disposition":disposition,"receipt":receipt}))
    }

    pub(super) fn get_request(&self, value: &Value) -> Result<Value> {
        fields(value, &["type", "id", "key"])?;
        let receipt = self.host.session().request_receipt(&request_key(value)?)?;
        Ok(json!({"receipt":receipt}))
    }
}

fn request_key(value: &Value) -> Result<RequestKey> {
    serde_json::from_value(value.get("key").context("key is required")?.clone())
        .context("invalid request key")
}
