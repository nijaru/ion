//! Local MCP observation conversion and private artifact delivery.
use ion_ai::{ImageContent, LoadedImage};
use ion_core::CodingToolOutput;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::json;

use super::{
    artifact::{save_full_mcp_output, save_raw_mcp_result},
    resource_content::{self, ResourceContent, ResourceData},
};

/// Once a response is observed, its local processing remains owned through stop
/// and connection shutdown, including queued blocking dispatch. Do not race this
/// join against cancellation or turn a worker panic into a model-facing refusal.
pub(super) async fn run<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|failure| {
            if failure.is_panic() {
                std::panic::resume_unwind(failure.into_panic());
            }
            panic!("MCP result worker stopped without an observation: {failure}");
        })
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
        ContentBlock::Image(image) => resource_content::image(&image.data, &image.mime_type)
            .map(image_parts)
            .map_err(|reason| format!("MCP tool {name} {reason}")),
        ContentBlock::Audio(audio) => Err(format!(
            "MCP tool {name} returned unsupported audio media ({}; not delivered)",
            audio.mime_type
        )),
        ContentBlock::Resource(resource) => {
            let ResourceContent {
                uri,
                mime_type,
                data,
            } = resource_content::convert(&resource.resource)?;
            match data {
                ResourceData::Text(text) => Ok((vec![format!("Resource {uri}:\n{text}")], None)),
                ResourceData::Image(image) => {
                    let (mut pieces, image) = image_parts(image);
                    pieces.insert(0, format!("Resource {uri}:"));
                    Ok((pieces, image))
                }
                ResourceData::Blob { size, saved } => {
                    let path = saved.map_err(|error| {
                        format!("resource {uri} could not be saved to a private file: {error}")
                    })?;
                    Ok((
                        vec![format!(
                            "Embedded resource saved to a private file: {}",
                            json!({
                                "uri": uri, "mimeType": mime_type, "savedPath": path, "size": size
                            })
                        )],
                        None,
                    ))
                }
            }
        }
        // Links remain lower-trust metadata: never fetched or treated as local paths.
        ContentBlock::ResourceLink(link) => Ok((
            vec![format!(
                "Resource link (metadata only, not fetched): {}",
                json!({
                    "uri": link.uri, "name": link.name, "title": link.title,
                    "description": link.description, "mimeType": link.mime_type, "size": link.size,
                })
            )],
            None,
        )),
        _ => Err(format!(
            "MCP tool {name} returned unsupported non-image media or resource content (not delivered)"
        )),
    }
}

fn image_parts(loaded: LoadedImage) -> (Vec<String>, Option<ImageContent>) {
    let mut pieces = vec![format!("[image: {}]", loaded.content.mime_type().as_str())];
    if let Some(note) = loaded.note {
        pieces.push(note);
    }
    (pieces, Some(loaded.content))
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
