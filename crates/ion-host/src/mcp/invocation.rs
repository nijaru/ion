//! One request-bound invocation, its local send, cancellation and observation.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Result as AnyResult;
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{BoxFuture, ImageMime, MAX_SOURCE_BYTES, ToolCall, normalize_image};
use ion_core::{CodingToolOutput, ToolExecutor};
use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, ContentBlock,
        ServerResult,
    },
    service::PeerRequestOptions,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Server, send::Completion, tool_error};

/// Original identity and connection are frozen with the model-visible definition.
pub(super) struct McpExecutor {
    pub server: Arc<Server>,
    pub original_name: String,
}

impl ToolExecutor for McpExecutor {
    fn execute<'a>(
        &'a self,
        invocation: ion_core::ToolInvocation<'a>,
    ) -> BoxFuture<'a, CodingToolOutput> {
        let call = invocation.call();
        let stop = invocation.stop().clone();
        Box::pin(async move {
            match self.call(call, stop).await {
                Ok((result, cleanup_error)) => {
                    let name = call.name.clone();
                    // Once observed, conversion remains owned through stop and
                    // connection closure, including queued blocking dispatch.
                    let mut output =
                        tokio::task::spawn_blocking(move || convert_tool_result(&name, result))
                            .await
                            .unwrap_or_else(|failure| {
                                if failure.is_panic() {
                                    std::panic::resume_unwind(failure.into_panic());
                                }
                                panic!(
                                    "MCP result worker stopped without an observation: {failure}"
                                );
                            });
                    if let Some(error) = cleanup_error {
                        output.value["cleanup_error"] = serde_json::Value::String(error);
                    }
                    output
                }
                Err(output) => output,
            }
        })
    }
}

impl McpExecutor {
    async fn call(
        &self,
        call: &ToolCall,
        stop: CancellationToken,
    ) -> Result<(CallToolResult, Option<String>), CodingToolOutput> {
        let Some(args) = call.arguments.as_object() else {
            return Err(tool_error(
                "MCP tool arguments must be an object".to_owned(),
            ));
        };
        let mut request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new(self.original_name.clone()).with_arguments(args.clone()),
        ));
        let sent = Completion::attach(&mut request);
        let client = tokio::select! {
            biased;
            () = self.server.retiring.cancelled() => return Err(self.closed()),
            () = stop.cancelled() => return Err(self.cancelled(call)),
            client = self.server.client.read() => client,
        };
        let mut handle = {
            let Some(connection) = client.as_ref() else {
                return Err(self.closed());
            };
            // No subscriptions, timeout watchers or awaited work follow queue
            // admission with no_options. A cancelled pending admission has not
            // issued a request; an admitted one returns its owned handle.
            tokio::select! {
                biased;
                () = self.server.retiring.cancelled() => return Err(self.closed()),
                () = stop.cancelled() => return Err(self.cancelled(call)),
                handle = connection.client.send_cancellable_request(request, PeerRequestOptions::no_options()) => {
                    handle.map_err(|error| self.failed(error))?
                }
            }
        };
        // Hold the connection through observation, as before. Stop releases it
        // before retirement; the handle still retains the exact original peer.
        let response = tokio::select! {
            biased;
            response = &mut handle.rx => response,
            () = self.server.retiring.cancelled() => {
                drop(client);
                let cleanup = self.server.shutdown().await;
                let _ = sent.await;
                cleanup.map_err(|cleanup| self.failed(cleanup))?;
                return Err(self.failed("connection retired; effects may be unknown"));
            }
            () = stop.cancelled() => {
                drop(client);
                let stopping = async {
                    tokio::join!(sent, handle.cancel(Some("Ion operation cancelled".into())))
                };
                tokio::pin!(stopping);
                // This is a graceful local flush deadline, not an execution
                // timeout or a promise that the remote operation has stopped.
                match tokio::time::timeout(Duration::from_secs(3), &mut stopping).await {
                    Ok((_, Ok(()))) => {}
                    Ok((_, Err(error))) => {
                        self.server.shutdown().await.map_err(|cleanup| self.failed(cleanup))?;
                        return Err(self.failed(error));
                    }
                    Err(_) => {
                        // A partial native frame or pending HTTP POST cannot be
                        // abandoned while keeping its connection usable.
                        let cleanup = self.server.shutdown().await;
                        let _ = stopping.await;
                        cleanup.map_err(|cleanup| self.failed(cleanup))?;
                    }
                }
                return Err(self.cancelled(call));
            }
        };
        drop(client);
        // A response can race the SDK send task's completion. Keep that local
        // task owned too; retirement interrupts stuck POSTs without discarding
        // the response that has already been observed.
        let mut sent = sent;
        let closing = async {
            tokio::select! {
                () = stop.cancelled() => {}
                () = self.server.retiring.cancelled() => {}
            }
        };
        let cleanup_error = tokio::select! {
            biased;
            _ = &mut sent => None,
            () = closing => {
                let cleanup = self.server.shutdown().await;
                let _ = sent.await;
                cleanup.err().map(|error| format!("{error:#}"))
            }
        };
        match response
            .map_err(|_| self.failed("transport closed"))?
            .map_err(|error| self.failed(error))?
        {
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

    fn closed(&self) -> CodingToolOutput {
        tool_error(format!("MCP server {} is closed", self.server.name))
    }

    fn failed(&self, error: impl std::fmt::Display) -> CodingToolOutput {
        tool_error(format!(
            "MCP server {} tool call failed: {error}",
            self.server.name
        ))
    }

    fn cancelled(&self, call: &ToolCall) -> CodingToolOutput {
        tool_error(format!(
            "MCP tool {} cancelled; effects may be unknown",
            call.name
        ))
    }
}

pub(super) fn convert_tool_result(name: &str, result: CallToolResult) -> CodingToolOutput {
    let mut content = Vec::new();
    let mut images = Vec::new();
    for part in result.content {
        match part {
            ContentBlock::Text(text) => content.push(text.text),
            ContentBlock::Image(image) => {
                if image.data.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 {
                    return tool_error(format!(
                        "MCP tool {name} image exceeds 32 MiB source bound"
                    ));
                }
                let bytes = match STANDARD.decode(&image.data) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return tool_error(format!(
                            "MCP tool {name} returned invalid base64 image data"
                        ));
                    }
                };
                let Some(mime) = ImageMime::detect(&bytes) else {
                    return tool_error(format!(
                        "MCP tool {name} returned an unsupported image format"
                    ));
                };
                if mime.as_str() != image.mime_type {
                    return tool_error(format!(
                        "MCP tool {name} image MIME type does not match its data"
                    ));
                }
                let loaded = match normalize_image(&bytes) {
                    Ok(image) => image,
                    Err(error) => {
                        return tool_error(format!(
                            "MCP tool {name} returned an invalid image: {error}"
                        ));
                    }
                };
                content.push(format!("[image: {}]", loaded.content.mime_type().as_str()));
                if let Some(note) = loaded.note {
                    content.push(note);
                }
                images.push(loaded.content);
            }
            _ => {
                return tool_error(format!(
                    "MCP tool {name} returned unsupported non-image media or resource content"
                ));
            }
        }
    }
    let is_error = result.is_error.unwrap_or(false);
    let value =
        json!({"content":content.join("\n"),"structured_content":result.structured_content});
    let encoded = match serde_json::to_string(&value) {
        Ok(encoded) => encoded,
        Err(error) => {
            return tool_error(format!("MCP tool {name} result cannot be encoded: {error}"));
        }
    };
    let value = if encoded.len() <= 64 * 1024 {
        value
    } else {
        let (full_output_path, full_output_error) = match save_full_mcp_output(encoded.as_bytes()) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let prefix_end = floor_char_boundary(&encoded, 10 * 1024);
        let suffix_start = floor_char_boundary(&encoded, encoded.len() - 10 * 1024);
        json!({
            "content": format!(
                "MCP result truncated from {} bytes. Read full_output_path with read using byte_offset=0, continuing at next_byte, or bounded exec when available.\n{}\n[... omitted middle ...]\n{}",
                encoded.len(), &encoded[..prefix_end], &encoded[suffix_start..]
            ),
            "truncated": true,
            "full_output_path": full_output_path,
            "full_output_error": full_output_error,
        })
    };
    CodingToolOutput {
        value,
        images,
        is_error,
    }
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

pub(super) fn save_full_mcp_output(bytes: &[u8]) -> AnyResult<PathBuf> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot name MCP output: {error}"))?;
    let path = std::env::temp_dir().join(format!(
        "ion-mcp-output-{}-{:032x}.json",
        std::process::id(),
        u128::from_ne_bytes(nonce)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(path)
}
