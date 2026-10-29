//! Supplied MCP resource bytes. URIs are metadata, never implicit reads.
use std::{borrow::Cow, io::Write, path::PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{ImageMime, LoadedImage, MAX_SOURCE_BYTES, normalize_image};
use rmcp::model::ResourceContents;

use super::artifact;

pub(super) struct ResourceContent<'a> {
    pub uri: &'a str,
    pub mime_type: Option<&'a str>,
    pub data: ResourceData<'a>,
}

pub(super) enum ResourceData<'a> {
    Text(Cow<'a, str>),
    Image(LoadedImage),
    Blob {
        size: usize,
        saved: Result<PathBuf, String>,
    },
}

pub(super) fn convert(resource: &ResourceContents) -> Result<ResourceContent<'_>, String> {
    match resource {
        ResourceContents::TextResourceContents {
            uri,
            mime_type,
            text,
            ..
        } => Ok(ResourceContent {
            uri,
            mime_type: mime_type.as_deref(),
            data: ResourceData::Text(Cow::Borrowed(text)),
        }),
        ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } => {
            let data = if let Some(mime) = mime_type
                .as_deref()
                .filter(|mime| mime.to_ascii_lowercase().starts_with("image/"))
            {
                ResourceData::Image(
                    image(blob, mime).map_err(|reason| format!("resource {uri} {reason}"))?,
                )
            } else {
                let bytes = STANDARD
                    .decode(blob)
                    .map_err(|_| format!("resource {uri} returned invalid base64 data"))?;
                if is_text_mime(mime_type.as_deref()) {
                    ResourceData::Text(Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()))
                } else {
                    let size = bytes.len();
                    let saved =
                        artifact::private_artifact_path("ion-mcp-resource", &extension_of(uri))
                            .and_then(|path| {
                                artifact::write_private_artifact(path, |file| {
                                    file.write_all(&bytes)?;
                                    Ok(())
                                })
                            })
                            .map_err(|error| format!("{error:#}"));
                    ResourceData::Blob { size, saved }
                }
            };
            Ok(ResourceContent {
                uri,
                mime_type: mime_type.as_deref(),
                data,
            })
        }
        _ => Err("resource returned an unsupported content variant".into()),
    }
}

pub(super) fn image(data: &str, mime_type: &str) -> Result<LoadedImage, String> {
    if data.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 {
        return Err("image exceeds 32 MiB source bound".into());
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| "returned invalid base64 image data")?;
    let mime = ImageMime::detect(&bytes).ok_or("returned an unsupported image format")?;
    if mime.as_str() != mime_type {
        return Err("image MIME type does not match its data".into());
    }
    normalize_image(&bytes).map_err(|error| format!("returned an invalid image: {error}"))
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
