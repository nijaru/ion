//! Model-facing MCP resources: bounded read-only listing and reading.
use std::{io::Write, path::PathBuf, sync::Arc};

use futures_util::future::join_all;
use ion_ai::{BoxFuture, ImageContent, ImageMime, ToolCall, normalize_image};
use ion_core::{
    CodingToolOutput, ToolDefinition, ToolExecutor, ToolExposure, ToolInvocation, ToolPresentation,
    ToolRegistration,
};
use rmcp::model::{
    ClientRequest, ListResourceTemplatesRequest, ListResourcesRequest, PaginatedRequestParams,
    ReadResourceRequest, ReadResourceRequestParams, Resource, ResourceContents, ResourceTemplate,
    ServerResult,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    Server,
    conversion::{self, save_full_mcp_output},
    request::RequestError,
    tool_error,
};

const LIST_RESOURCES: &str = "list_mcp_resources";
const LIST_TEMPLATES: &str = "list_mcp_resource_templates";
const READ_RESOURCE: &str = "read_mcp_resource";
/// Keep one resource payload below the model-facing inline bound. Larger
/// observations remain available through a private JSON file.
const MAX_INLINE_RESULT_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_PAGES: usize = 64;
const RESOURCE_PREVIEW_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy)]
enum Kind {
    Resources,
    Templates,
    Read,
}

struct ResourceExecutor {
    servers: Vec<Arc<Server>>,
    kind: Kind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    server: Option<String>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    server: String,
    uri: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ListedResource<'a> {
    server: &'a str,
    uri: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ListedTemplate<'a> {
    server: &'a str,
    uri_template: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'a str>,
}

#[derive(Default)]
struct ReadConversion {
    contents: Vec<Value>,
    images: Vec<ImageContent>,
    warnings: Vec<String>,
    failed: bool,
    preview: Option<String>,
}

pub(super) fn registrations(servers: &[Arc<Server>]) -> Vec<ToolRegistration> {
    if !servers.iter().any(|server| server.resources) {
        return Vec::new();
    }
    let servers = servers.to_vec();
    let read_servers = servers.clone();
    [
        (
            Kind::Resources,
            LIST_RESOURCES,
            "Lists resources provided by MCP servers. Resources share data that provides context to language models, such as files, database schemas, or application-specific information. Omit server to list every resource-capable server; use cursor with a server argument to continue one server's listing.",
        ),
        (
            Kind::Templates,
            LIST_TEMPLATES,
            "Lists parameterized resource templates provided by MCP servers. Omit server to list every resource-capable server; use cursor with a server argument to continue one server's listing.",
        ),
    ]
    .into_iter()
    .map(|(kind, name, description)| {
        ToolRegistration::new(
            ToolDefinition {
                spec: ion_ai::ToolSpec {
                    name: name.into(),
                    description: description.into(),
                    input_schema: json!({
                        "type":"object",
                        "additionalProperties":false,
                        "properties":{
                            "server":{"type":"string","description":"MCP server name. Omit to list every server with resources."},
                            "cursor":{"type":"string","description":"Opaque cursor from a previous call with the same server; omit for the first page."}
                        }
                    }),
                },
                presentation: ToolPresentation::external(),
                exposure: ToolExposure::Direct,
            },
            Arc::new(ResourceExecutor {
                servers: servers.clone(),
                kind,
            }),
        )
    })
    .chain(std::iter::once(ToolRegistration::new(
        ToolDefinition {
            spec: ion_ai::ToolSpec {
                name: READ_RESOURCE.into(),
                description: "Read a specific resource from an MCP server given the server name and resource URI.".into(),
                input_schema: json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["server","uri"],
                    "properties":{
                        "server":{"type":"string","description":"MCP server name exactly as configured."},
                        "uri":{"type":"string","description":"Resource URI returned by list_mcp_resources or a resource template."}
                    }
                }),
            },
            presentation: ToolPresentation::external(),
            exposure: ToolExposure::Direct,
        },
        Arc::new(ResourceExecutor {
            servers: read_servers,
            kind: Kind::Read,
        }),
    )))
    .collect()
}

impl ToolExecutor for ResourceExecutor {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        let call = invocation.call();
        let stop = invocation.stop().clone();
        Box::pin(async move {
            let mut output = match self.kind {
                Kind::Resources => list(call, &self.servers, &stop, Kind::Resources).await,
                Kind::Templates => list(call, &self.servers, &stop, Kind::Templates).await,
                Kind::Read => read(call, &self.servers, &stop).await,
            };
            let selected = call
                .arguments
                .get("server")
                .and_then(Value::as_str)
                .map(str::trim);
            for server in self.servers.iter().filter(|server| server.resources) {
                if selected.is_none_or(|name| name.is_empty() || name == server.name) {
                    server.attach_stderr(&mut output);
                }
            }
            output
        })
    }
}

async fn list(
    call: &ToolCall,
    servers: &[Arc<Server>],
    stop: &CancellationToken,
    kind: Kind,
) -> CodingToolOutput {
    let arguments: ListArguments = match serde_json::from_value(call.arguments.clone()) {
        Ok(arguments) => arguments,
        Err(error) => {
            return tool_error(format!(
                "MCP resource listing arguments must be an object with optional server and cursor strings: {error}"
            ));
        }
    };
    let server_name = arguments
        .server
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let cursor = arguments
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if server_name.is_none() && cursor.is_some() {
        return tool_error("MCP resource listing cursor requires a server argument".into());
    }
    if let Some(name) = server_name {
        let server = match resource_server(servers, name) {
            Ok(server) => server,
            Err(error) => return tool_error(error),
        };
        let (items, next_cursor, cleanup_error) =
            match list_page(server, kind, cursor.map(str::to_owned), stop).await {
                Ok(page) => page,
                Err(error) => return tool_error(error),
            };
        let name = server.name.clone();
        return conversion::run(move || {
            let mut value = Map::new();
            value.insert("server".into(), Value::String(name.clone()));
            value.insert(list_key(kind).into(), Value::Array(items));
            if let Some(cursor) = next_cursor {
                value.insert("nextCursor".into(), Value::String(cursor));
            }
            if let Some(error) = cleanup_error {
                value.insert("cleanupError".into(), Value::String(error));
            }
            let mut output = bound_read_result(Value::Object(value), Vec::new(), false);
            if output.value.get("truncated").is_some_and(Value::is_boolean)
                && let Some(object) = output.value.as_object_mut()
            {
                object.insert("server".into(), Value::String(name));
            }
            output
        })
        .await;
    }

    let mut visible: Vec<_> = servers.iter().filter(|server| server.resources).collect();
    visible.sort_by(|left, right| left.name.cmp(&right.name));
    let pages = join_all(visible.into_iter().map(|server| async move {
        let result = list_all(server, kind, stop).await;
        (server.name.clone(), result)
    }))
    .await;
    conversion::run(move || {
        let mut items = Vec::new();
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for (server, result) in pages {
            match result {
                Ok((mut page, cleanup_error)) => {
                    items.append(&mut page);
                    if let Some(error) = cleanup_error {
                        warnings.push(json!({"server":server, "error":error}));
                    }
                }
                Err(error) => errors.push(json!({"server":server, "error":error})),
            }
        }
        let mut value = Map::new();
        value.insert(list_key(kind).into(), Value::Array(items));
        if !errors.is_empty() {
            value.insert("errors".into(), Value::Array(errors));
        }
        if !warnings.is_empty() {
            value.insert("warnings".into(), Value::Array(warnings));
        }
        bound_read_result(Value::Object(value), Vec::new(), false)
    })
    .await
}

fn list_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Resources | Kind::Read => "resources",
        Kind::Templates => "resourceTemplates",
    }
}

async fn list_page(
    server: &Arc<Server>,
    kind: Kind,
    cursor: Option<String>,
    stop: &CancellationToken,
) -> Result<(Vec<Value>, Option<String>, Option<String>), String> {
    let request = match kind {
        Kind::Resources => ClientRequest::ListResourcesRequest(ListResourcesRequest {
            method: Default::default(),
            params: cursor
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor))),
            extensions: Default::default(),
        }),
        Kind::Templates => {
            ClientRequest::ListResourceTemplatesRequest(ListResourceTemplatesRequest {
                method: Default::default(),
                params: cursor
                    .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor))),
                extensions: Default::default(),
            })
        }
        Kind::Read => return Err("internal MCP resource request kind mismatch".into()),
    };
    let (result, cleanup_error) = server
        .request(request, stop)
        .await
        .map_err(|error| request_error(server, error))?;
    let name = server.name.clone();
    conversion::run(move || match (kind, result) {
        (Kind::Resources, ServerResult::ListResourcesResult(result)) => Ok((
            result
                .resources
                .iter()
                .filter(|resource| !is_mcp_app(&resource.uri, resource.mime_type.as_deref()))
                .map(|resource| {
                    serde_json::to_value(listed_resource(&name, resource))
                        .expect("a listed resource is serializable")
                })
                .collect(),
            result.next_cursor,
            cleanup_error,
        )),
        (Kind::Templates, ServerResult::ListResourceTemplatesResult(result)) => Ok((
            result
                .resource_templates
                .iter()
                .filter(|template| {
                    !is_mcp_app(&template.uri_template, template.mime_type.as_deref())
                })
                .map(|template| {
                    serde_json::to_value(listed_template(&name, template))
                        .expect("a listed resource template is serializable")
                })
                .collect(),
            result.next_cursor,
            cleanup_error,
        )),
        _ => Err(format!(
            "MCP server {} returned an unexpected response for its resource listing",
            name
        )),
    })
    .await
}

async fn list_all(
    server: &Arc<Server>,
    kind: Kind,
    stop: &CancellationToken,
) -> Result<(Vec<Value>, Option<String>), String> {
    let mut items = Vec::new();
    let mut cursor = None;
    let mut cleanup_error = None;
    for _ in 0..MAX_RESOURCE_PAGES {
        let (mut page, next_cursor, cleanup) = list_page(server, kind, cursor, stop).await?;
        items.append(&mut page);
        if cleanup.is_some() {
            cleanup_error = cleanup;
        }
        match next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok((items, cleanup_error)),
        }
    }
    Err(format!(
        "MCP server {} exceeded the {MAX_RESOURCE_PAGES}-page resource listing bound; list one server with a cursor instead",
        server.name
    ))
}

async fn read(
    call: &ToolCall,
    servers: &[Arc<Server>],
    stop: &CancellationToken,
) -> CodingToolOutput {
    let arguments: ReadArguments = match serde_json::from_value(call.arguments.clone()) {
        Ok(arguments) => arguments,
        Err(error) => {
            return tool_error(format!(
                "MCP resource read arguments need server and uri strings: {error}"
            ));
        }
    };
    let server_name = arguments.server.trim();
    let uri = arguments.uri.trim();
    if server_name.is_empty() || uri.is_empty() {
        return tool_error("MCP resource read requires nonblank server and uri arguments".into());
    }
    let server = match resource_server(servers, server_name) {
        Ok(server) => server,
        Err(error) => return tool_error(error),
    };
    let request = ClientRequest::ReadResourceRequest(ReadResourceRequest::new(
        ReadResourceRequestParams::new(uri),
    ));
    let (result, cleanup_error) = match server.request(request, stop).await {
        Ok(result) => result,
        Err(error) => return tool_error(request_error(server, error)),
    };
    let ServerResult::ReadResourceResult(result) = result else {
        return tool_error(format!(
            "MCP server {} returned an unexpected response for resources/read",
            server.name
        ));
    };
    let name = server.name.clone();
    let uri = uri.to_owned();
    conversion::run(move || read_result(&name, &uri, result.contents, cleanup_error)).await
}

fn read_result(
    server: &str,
    uri: &str,
    contents: Vec<ResourceContents>,
    cleanup_error: Option<String>,
) -> CodingToolOutput {
    let conversion = match convert_read_contents(contents) {
        Ok(conversion) => conversion,
        Err(error) => return tool_error(format!("MCP server {server} {error}")),
    };
    let mut value = Map::new();
    value.insert("server".into(), Value::String(server.into()));
    value.insert("uri".into(), Value::String(uri.into()));
    value.insert("contents".into(), Value::Array(conversion.contents));
    if !conversion.warnings.is_empty() {
        value.insert(
            "warnings".into(),
            Value::Array(conversion.warnings.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(error) = cleanup_error {
        value.insert("cleanupError".into(), Value::String(error));
    }
    let mut output = bound_read_result(Value::Object(value), conversion.images, conversion.failed);
    if let Some(preview) = conversion.preview
        && output.value.get("truncated").is_some_and(Value::is_boolean)
        && let Some(object) = output.value.as_object_mut()
    {
        object.insert("preview".into(), Value::String(preview));
    }
    if output.value.get("truncated").is_some_and(Value::is_boolean)
        && let Some(object) = output.value.as_object_mut()
    {
        object.insert("server".into(), Value::String(server.into()));
        object.insert("uri".into(), Value::String(uri.into()));
    }
    output
}

fn convert_read_contents(contents: Vec<ResourceContents>) -> Result<ReadConversion, String> {
    let empty = contents.is_empty();
    let mut conversion = ReadConversion::default();
    for content in contents {
        match content {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                ..
            } => {
                if conversion.preview.is_none() {
                    conversion.preview = Some(preview(&text));
                }
                conversion.contents.push(json!({
                    "uri": uri,
                    "mimeType": mime_type,
                    "text": text,
                }));
            }
            ResourceContents::BlobResourceContents {
                uri,
                mime_type,
                blob,
                ..
            } => {
                let bytes =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &blob)
                        .map_err(|_| format!("resource {uri} returned invalid base64 data"))?;
                let image_mime = mime_type
                    .as_deref()
                    .is_some_and(|mime| mime.to_ascii_lowercase().starts_with("image/"));
                if image_mime && let Some(detected) = ImageMime::detect(&bytes) {
                    if mime_type
                        .as_deref()
                        .is_some_and(|mime| mime != detected.as_str())
                    {
                        return Err(format!(
                            "resource {uri} image MIME type does not match its data"
                        ));
                    }
                    let loaded = normalize_image(&bytes).map_err(|error| {
                        format!("resource {uri} returned an invalid image: {error}")
                    })?;
                    conversion.contents.push(json!({
                        "uri": uri,
                        "mimeType": loaded.content.mime_type().as_str(),
                        "image": true,
                    }));
                    conversion.images.push(loaded.content);
                } else if image_mime && !is_text_mime(mime_type.as_deref()) {
                    return Err(format!(
                        "resource {uri} returned an unsupported image format"
                    ));
                } else if is_text_mime(mime_type.as_deref()) {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if conversion.preview.is_none() {
                        conversion.preview = Some(preview(&text));
                    }
                    conversion.contents.push(json!({
                        "uri": uri,
                        "mimeType": mime_type,
                        "text": text,
                    }));
                } else {
                    let size = bytes.len();
                    match save_resource_blob(&bytes, &uri) {
                        Ok(path) => conversion.contents.push(json!({
                            "uri": uri,
                            "mimeType": mime_type,
                            "savedPath": path,
                            "size": size,
                        })),
                        Err(error) => {
                            conversion.failed = true;
                            conversion.warnings.push(format!(
                                "resource {uri} could not be saved to a private file: {error}"
                            ));
                            conversion.contents.push(json!({
                                "uri": uri,
                                "mimeType": mime_type,
                                "size": size,
                                "savedError": error.to_string(),
                            }));
                        }
                    }
                }
            }
            _ => {
                return Err("resource returned an unsupported content variant".into());
            }
        }
    }
    if empty {
        conversion.warnings.push("Resource is empty.".into());
    }
    Ok(conversion)
}

fn listed_resource<'a>(server: &'a str, resource: &'a Resource) -> ListedResource<'a> {
    ListedResource {
        server,
        uri: &resource.uri,
        name: &resource.name,
        title: resource.title.as_deref(),
        description: resource.description.as_deref(),
        mime_type: resource.mime_type.as_deref(),
        size: resource.size,
    }
}

fn listed_template<'a>(server: &'a str, template: &'a ResourceTemplate) -> ListedTemplate<'a> {
    ListedTemplate {
        server,
        uri_template: &template.uri_template,
        name: &template.name,
        title: template.title.as_deref(),
        description: template.description.as_deref(),
        mime_type: template.mime_type.as_deref(),
    }
}

fn resource_server<'a>(servers: &'a [Arc<Server>], name: &str) -> Result<&'a Arc<Server>, String> {
    if let Some(server) = servers
        .iter()
        .find(|server| server.name == name && server.resources)
    {
        return Ok(server);
    }
    if servers.iter().any(|server| server.name == name) {
        return Err(format!("MCP server {name} has no resources"));
    }
    let available = servers
        .iter()
        .filter(|server| server.resources)
        .map(|server| server.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(if available.is_empty() {
        format!("MCP server {name} is not configured and no MCP server exposes resources")
    } else {
        format!("MCP server {name} is not configured; resource servers: {available}")
    })
}

fn request_error(server: &Server, error: RequestError) -> String {
    let reason = match error {
        RequestError::Closed => return format!("MCP server {} is closed", server.name),
        RequestError::Cancelled => {
            return format!(
                "MCP server {} resource request was cancelled; the server may still be reading",
                server.name
            );
        }
        RequestError::Retired => "connection retired; the resource read is unknown".into(),
        RequestError::Failed(error) => error,
    };
    format!(
        "MCP server {} resource request failed: {reason}",
        server.name
    )
}

fn is_text_mime(mime_type: Option<&str>) -> bool {
    let Some(mime_type) = mime_type else {
        return false;
    };
    let media_type = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    media_type.starts_with("text/")
        || media_type == "application/json"
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
}

fn is_mcp_app(uri: &str, mime_type: Option<&str>) -> bool {
    if uri.starts_with("ui://") {
        return true;
    }
    let Some(mime_type) = mime_type else {
        return false;
    };
    mime_type.split(';').skip(1).any(|parameter| {
        let mut pair = parameter.split('=').map(str::trim);
        pair.next() == Some("profile")
            && pair
                .next()
                .is_some_and(|value| value.trim_matches('"').eq_ignore_ascii_case("mcp-app"))
    })
}

fn preview(text: &str) -> String {
    if text.len() <= RESOURCE_PREVIEW_BYTES {
        return text.to_owned();
    }
    let mut end = RESOURCE_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn bound_read_result(value: Value, images: Vec<ImageContent>, failed: bool) -> CodingToolOutput {
    let encoded = match serde_json::to_vec(&value) {
        Ok(encoded) => encoded,
        Err(error) => {
            return tool_error(format!("MCP resource result cannot be encoded: {error}"));
        }
    };
    if encoded.len() <= MAX_INLINE_RESULT_BYTES {
        return CodingToolOutput {
            value,
            images,
            is_error: failed,
        };
    }
    let (full_output_path, full_output_error, notice) = match save_full_mcp_output(&encoded) {
        Ok(path) => (
            Some(path),
            None,
            format!(
                "MCP resource output exceeded the {MAX_INLINE_RESULT_BYTES}-byte inline bound. Read full_output_path with read using byte_offset=0, continuing at next_byte, or bounded exec when available; the file is a private JSON snapshot of the observation."
            ),
        ),
        Err(error) => (
            None,
            Some(error.to_string()),
            format!(
                "MCP resource output exceeded the {MAX_INLINE_RESULT_BYTES}-byte inline bound and could not be saved to a private file. Reduce the resource or retry."
            ),
        ),
    };
    CodingToolOutput {
        value: json!({
            "truncated": true,
            "original_bytes": encoded.len(),
            "full_output_path": full_output_path,
            "full_output_error": full_output_error,
            "notice": notice,
        }),
        images,
        is_error: failed,
    }
}

fn save_resource_blob(bytes: &[u8], uri: &str) -> anyhow::Result<PathBuf> {
    conversion::write_private_artifact(
        conversion::private_artifact_path("ion-mcp-resource", &extension_of(uri))?,
        |file| {
            file.write_all(bytes)?;
            Ok(())
        },
    )
}

fn extension_of(uri: &str) -> String {
    let path = uri
        .split(['?', '#'])
        .next()
        .unwrap_or(uri)
        .trim_end_matches('/');
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return ".bin".into();
    };
    if extension.is_empty()
        || extension.len() > 8
        || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return ".bin".into();
    }
    format!(".{extension}")
}
