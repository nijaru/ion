//! A tool's request-bound invocation and operation-specific observation.
use std::sync::Arc;

use ion_ai::{BoxFuture, ToolCall};
use ion_core::{ToolExecutor, ToolOutput};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, ServerResult,
};
use tokio_util::sync::CancellationToken;

use super::{Server, conversion, request::RequestError, schema::OutputSchema, tool_error};

/// Original identity and connection are frozen with the model-visible definition.
pub(super) struct McpExecutor {
    pub server: Arc<Server>,
    pub original_name: String,
    pub output_schema: Option<Arc<OutputSchema>>,
}

impl ToolExecutor for McpExecutor {
    fn execute<'a>(
        &'a self,
        invocation: ion_core::ToolInvocation<'a>,
    ) -> BoxFuture<'a, ToolOutput> {
        let call = invocation.call();
        let stop = invocation.stop().clone();
        Box::pin(async move {
            let mut output = match self.call(call, stop).await {
                Ok((result, cleanup_error)) => {
                    let name = call.name.clone();
                    let schema = self.output_schema.clone();
                    let mut output = conversion::run(move || {
                        conversion::convert_tool_result(&name, result, schema.as_deref())
                    })
                    .await;
                    if let Some(error) = cleanup_error {
                        output.value["cleanup_error"] = serde_json::Value::String(error);
                    }
                    output
                }
                Err(output) => output,
            };
            self.server.attach_stderr(&mut output);
            output
        })
    }
}

impl McpExecutor {
    async fn call(
        &self,
        call: &ToolCall,
        stop: CancellationToken,
    ) -> Result<(CallToolResult, Option<String>), ToolOutput> {
        let Some(args) = call.arguments.as_object() else {
            return Err(tool_error(
                "MCP tool arguments must be an object".to_owned(),
            ));
        };
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new(self.original_name.clone()).with_arguments(args.clone()),
        ));
        let (response, cleanup_error) =
            self.server
                .request(request, &stop)
                .await
                .map_err(|error| match error {
                    RequestError::Closed => {
                        tool_error(format!("MCP server {} is closed", self.server.name))
                    }
                    RequestError::Cancelled => tool_error(format!(
                        "MCP tool {} cancelled; effects may be unknown",
                        call.name
                    )),
                    RequestError::Retired => {
                        self.failed("connection retired; effects may be unknown")
                    }
                    RequestError::Failed(error) => self.failed(error),
                })?;
        match response {
            ServerResult::CallToolResult(result) => Ok((result, cleanup_error)),
            ServerResult::InputRequiredResult(_) => Err(self.failed(
                "unsupported input_required continuation; no additional tools/call was issued",
            )),
            ServerResult::CreateTaskResult(_) => {
                Err(self
                    .failed("unsupported task result; no task polling or continuation was issued"))
            }
            _ => Err(self.failed("unexpected tools/call response")),
        }
    }

    fn failed(&self, error: impl std::fmt::Display) -> ToolOutput {
        tool_error(format!(
            "MCP server {} tool call failed: {error}",
            self.server.name
        ))
    }
}
