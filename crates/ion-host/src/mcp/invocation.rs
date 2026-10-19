//! One request-bound invocation, its local send, cancellation and observation.
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Result as AnyResult;
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{
    BoxFuture, ImageContent, ImageMime, LoadedImage, MAX_SOURCE_BYTES, ToolCall, normalize_image,
};
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
            let mut output = match self.call(call, stop).await {
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
        let response = match response {
            Ok(response) => response.map_err(|error| self.failed(error))?,
            Err(_) => {
                let reason = self
                    .server
                    .transport_failure()
                    .await
                    .unwrap_or_else(|| "transport closed".to_owned());
                return Err(self.failed(reason));
            }
        };
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

/// Inline conversion notices stay bounded so a result with many tiny
/// unsupported blocks cannot expand into one warning per block.
const MAX_INLINE_NOTICES: usize = 8;
const MAX_NOTICE_CHARS: usize = 256;
pub(super) fn convert_tool_result(name: &str, result: CallToolResult) -> CodingToolOutput {
    // The remote error/success flag is the observed outcome. Local conversion
    // preserves it exactly and reports its own delivery failures beside it,
    // never as a replacement execution error.
    let is_error = result.is_error.unwrap_or(false);
    let mut content = Vec::new();
    let mut images = Vec::new();
    let mut notices = Vec::new();
    let mut retained_blocks = 0usize;
    let mut omitted_blocks = 0usize;
    for (index, part) in result.content.iter().enumerate() {
        match convert_block(name, part) {
            Ok((pieces, image)) => {
                retained_blocks += 1;
                content.extend(pieces);
                images.extend(image);
            }
            Err(reason) => {
                omitted_blocks += 1;
                if notices.len() < MAX_INLINE_NOTICES {
                    notices.push(bound_notice(format!(
                        "content block {index} omitted: {reason}"
                    )));
                }
            }
        }
    }
    if omitted_blocks > notices.len() {
        notices.push(format!(
            "{} further omitted content blocks",
            omitted_blocks - notices.len()
        ));
    }
    // Retain the original full response privately whenever conversion omitted
    // content. Its JSON streams into the artifact; only the retrieval
    // reference enters the model context.
    let (raw_result_path, raw_result_error) = if omitted_blocks > 0 {
        match save_raw_mcp_result(&result) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        }
    } else {
        (None, None)
    };
    let mut value = json!({
        "content": content.join("\n"),
        "structured_content": result.structured_content,
        "is_error": is_error,
    });
    if omitted_blocks > 0 {
        value["conversion_notices"] = json!(notices);
        value["retained_blocks"] = json!(retained_blocks);
        value["omitted_blocks"] = json!(omitted_blocks);
        value["raw_result_path"] = json!(raw_result_path);
        value["raw_result_error"] = json!(raw_result_error);
    }
    let encoded = serde_json::to_string(&value).expect("a JSON value always encodes");
    if encoded.len() <= 64 * 1024 {
        return CodingToolOutput {
            value,
            images,
            is_error,
        };
    }
    let (full_output_path, full_output_error) = match save_full_mcp_output(encoded.as_bytes()) {
        Ok(path) => (Some(path), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let prefix_end = floor_char_boundary(&encoded, 10 * 1024);
    let suffix_start = floor_char_boundary(&encoded, encoded.len() - 10 * 1024);
    // Truncation replaces the inline observation with a bounded preview: the
    // outcome flag, conversion notices and retrieval references stay top-level,
    // and oversized structured content cannot bypass the inline bound.
    let structured_len = value
        .get("structured_content")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::to_string(value).ok())
        .map_or(0, |encoded| encoded.len());
    let structured_omitted = structured_len > 4 * 1024;
    if structured_omitted {
        value["structured_content"] = serde_json::Value::Null;
        value["structured_content_omitted"] = json!(true);
    }
    value["content"] = json!(format!(
        "MCP result truncated from {} bytes. Read full_output_path with read using byte_offset=0, continuing at next_byte, or bounded exec when available.{}\n{}\n[... omitted middle ...]\n{}",
        encoded.len(),
        if structured_omitted {
            " Structured content is in the full output."
        } else {
            ""
        },
        &encoded[..prefix_end],
        &encoded[suffix_start..]
    ));
    value["truncated"] = json!(true);
    value["full_output_path"] = json!(full_output_path);
    value["full_output_error"] = json!(full_output_error);
    CodingToolOutput {
        value,
        images,
        is_error,
    }
}

/// One block's local delivery. The error describes an undeliverable block; it
/// never replaces the server's observed outcome.
fn convert_block(
    name: &str,
    part: &ContentBlock,
) -> Result<(Vec<String>, Option<ImageContent>), String> {
    match part {
        ContentBlock::Text(text) => Ok((vec![text.text.clone()], None)),
        ContentBlock::Image(image) => {
            let loaded = convert_image(name, &image.data, &image.mime_type)?;
            let mut pieces = vec![format!("[image: {}]", loaded.content.mime_type().as_str())];
            if let Some(note) = loaded.note {
                pieces.push(note);
            }
            Ok((pieces, Some(loaded.content)))
        }
        ContentBlock::Audio(audio) => Err(format!(
            "MCP tool {name} returned unsupported audio media ({}; not delivered)",
            audio.mime_type
        )),
        ContentBlock::Resource(_) => Err(format!(
            "MCP tool {name} returned unsupported embedded resource content (not delivered)"
        )),
        // Resource-link metadata is lower-trust data: never fetched and never
        // assumed to name a local file.
        ContentBlock::ResourceLink(link) => Err(format!(
            "MCP tool {name} returned an unsupported resource link (name={}, uri={}; metadata only, not fetched)",
            link.name, link.uri
        )),
        _ => Err(format!(
            "MCP tool {name} returned unsupported non-image media or resource content (not delivered)"
        )),
    }
}

fn convert_image(name: &str, data: &str, mime_type: &str) -> Result<LoadedImage, String> {
    if data.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 {
        return Err(format!("MCP tool {name} image exceeds 32 MiB source bound"));
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| format!("MCP tool {name} returned invalid base64 image data"))?;
    let mime = ImageMime::detect(&bytes)
        .ok_or_else(|| format!("MCP tool {name} returned an unsupported image format"))?;
    if mime.as_str() != mime_type {
        return Err(format!(
            "MCP tool {name} image MIME type does not match its data"
        ));
    }
    normalize_image(&bytes)
        .map_err(|error| format!("MCP tool {name} returned an invalid image: {error}"))
}

fn bound_notice(notice: String) -> String {
    if notice.len() <= MAX_NOTICE_CHARS {
        return notice;
    }
    let end = floor_char_boundary(&notice, MAX_NOTICE_CHARS);
    format!("{}\u{2026}", &notice[..end])
}
fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The original wire response, streamed to a private artifact. Nothing is
/// cloned or base64-copied into the model context.
pub(super) fn save_raw_mcp_result(result: &CallToolResult) -> AnyResult<PathBuf> {
    write_private_artifact(
        private_artifact_path("ion-mcp-raw-result", ".json")?,
        |file| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, result)?;
            writer.flush()?;
            Ok(())
        },
    )
}
pub(super) fn save_full_mcp_output(bytes: &[u8]) -> AnyResult<PathBuf> {
    write_private_artifact(private_artifact_path("ion-mcp-output", ".json")?, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

fn private_artifact_path(prefix: &str, extension: &str) -> AnyResult<PathBuf> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot name MCP artifact: {error}"))?;
    let serial = u128::from_ne_bytes(nonce);
    Ok(std::env::temp_dir().join(format!(
        "{prefix}-{}-{serial:032x}{extension}",
        std::process::id()
    )))
}

/// A private 0600 artifact. A failed write removes its partial file instead of
/// leaving untrustworthy evidence, and reports the real error.
pub(super) fn write_private_artifact(
    path: PathBuf,
    write: impl FnOnce(&mut fs::File) -> AnyResult<()>,
) -> AnyResult<PathBuf> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = write(&mut file) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}
