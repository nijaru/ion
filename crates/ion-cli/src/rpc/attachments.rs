//! Strict wire input; source preparation and filesystem authority stay in Host.
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_app::{UserInput, attachments::AttachmentSource};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prompt {
    #[serde(rename = "type")]
    _kind: String,
    #[serde(default, rename = "id")]
    _id: Option<Value>,
    message: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Attachment {
    File {
        path: PathBuf,
    },
    ImageFile {
        path: PathBuf,
    },
    Image {
        mime_type: ion_ai::ImageMime,
        data: String,
    },
}

pub(super) fn prompt(value: &Value) -> Result<UserInput> {
    let prompt: Prompt = serde_json::from_value(value.clone()).context("invalid prompt input")?;
    let attachments = prompt
        .attachments
        .into_iter()
        .map(|attachment| {
            Ok(match attachment {
                Attachment::File { path } => AttachmentSource::File(path),
                Attachment::ImageFile { path } => AttachmentSource::ImageFile(path),
                Attachment::Image { mime_type, data } => {
                    // The 8 MiB frame cap includes base64, keeping decoded bytes
                    // below the application's 32 MiB source limit.
                    let bytes = STANDARD.decode(data).context("invalid base64 image data")?;
                    AttachmentSource::ImageBytes {
                        bytes,
                        mime_type: Some(mime_type),
                    }
                }
            })
        })
        .collect::<Result<_>>()?;
    Ok(UserInput::Prompt {
        text: prompt.message,
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_sources_are_literal_and_memory_bytes_keep_the_declared_mime() {
        let input = prompt(&json!({"type":"prompt", "id":7, "message":"literal",
            "attachments":[{"type":"file","path":"~/literal.md"},
                {"type":"image_file","path":"picture"},
                {"type":"image","mime_type":"image/jpeg","data":"YWJj"}]}))
        .unwrap();
        let UserInput::Prompt { text, attachments } = input else {
            panic!("prompt")
        };
        assert_eq!(text, "literal");
        assert!(
            matches!(&attachments[0], AttachmentSource::File(path) if path == std::path::Path::new("~/literal.md"))
        );
        assert!(
            matches!(&attachments[1], AttachmentSource::ImageFile(path) if path == std::path::Path::new("picture"))
        );
        assert!(
            matches!(&attachments[2], AttachmentSource::ImageBytes {bytes, mime_type: Some(ion_ai::ImageMime::Jpeg)} if bytes == b"abc")
        );
    }

    #[test]
    fn malformed_or_obsolete_fields_cannot_silently_drop_attachments() {
        for fields in [
            json!({"images":["image.png"]}),
            json!({"attachments":["image.png"]}),
            json!({"attachments":[{"type":"file","path":"x","data":"ignored"}]}),
            json!({"attachments":[{"type":"image","mime_type":"image/png","data":"not base64"}]}),
            json!({"attachments":[{"type":"image","mime_type":"unknown","data":""}]}),
            json!({"attachments":[{"type":"image","mime_type":"image/png","data":"","path":"ignored"}]}),
        ] {
            let mut value = json!({"type":"prompt","message":"literal"});
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            assert!(prompt(&value).is_err(), "{value}");
        }
    }
}
