//! Borrowed neutral-message sizing, distinct from retained replay representation.
//!
//! Opaque adapter continuation data can duplicate text/tool payloads, include
//! signatures or contain provider-only material. It is not additional neutral
//! prompt text. These views estimate that prompt; they are not wire-size or
//! provider-token counters and never alter the retained request or history.
use ion_ai::{Content, ImageContent, Message, Role};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub(crate) struct NeutralMessage<'a> {
    role: &'a Role,
    content: &'a [Content],
    // Keep Message's encoded shape, with opaque continuation absent.
    provider_replay: Option<()>,
}

impl<'a> From<&'a Message> for NeutralMessage<'a> {
    fn from(message: &'a Message) -> Self {
        let Message {
            role,
            content,
            provider_replay: _,
        } = message;
        Self {
            role,
            content,
            provider_replay: None,
        }
    }
}

/// Cached image accounting for one immutable projected message.
#[derive(Clone, Copy, Default)]
pub(crate) struct MessageImages {
    pub(crate) count: u64,
    pub(crate) encoded_bytes: usize,
    pub(crate) digest: Option<[u8; 32]>,
}

impl MessageImages {
    pub(crate) fn measure(message: &Message) -> Self {
        let mut encoded_bytes = 0usize;
        let mut count = 0u64;
        let mut identity: Option<Sha256> = None;
        for part in &message.content {
            match part {
                Content::Image(image) => {
                    hash_image(identity.get_or_insert_with(Sha256::new), b'u', image);
                    encoded_bytes = encoded_bytes.saturating_add(image.data().len());
                    count = count.saturating_add(1);
                }
                Content::ToolResult(result) => {
                    for image in &result.images {
                        hash_image(identity.get_or_insert_with(Sha256::new), b't', image);
                        encoded_bytes = encoded_bytes.saturating_add(image.data().len());
                        count = count.saturating_add(1);
                    }
                }
                Content::Text(_) | Content::Thinking(_) | Content::ToolCall(_) => {}
            }
        }
        Self {
            count,
            encoded_bytes,
            digest: identity.map(|hasher| hasher.finalize().into()),
        }
    }
}

fn hash_image(identity: &mut Sha256, source: u8, image: &ImageContent) {
    identity.update([source]);
    hash_field(identity, image.mime_type().as_str().as_bytes());
    hash_field(identity, image.data().as_bytes());
}

fn hash_field(identity: &mut Sha256, value: &[u8]) {
    identity.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    identity.update(value);
}

/// Reusable byte counts and image accounting, not payload copies. Replay may be
/// omitted at a route/epoch boundary without remeasuring the visible content.
#[derive(Clone, Copy)]
pub(crate) struct MessageBytes {
    pub(crate) neutral: usize,
    pub(crate) replay: Option<usize>,
    pub(crate) images: MessageImages,
}

impl MessageBytes {
    pub(crate) fn measure(message: &Message) -> Result<Self, serde_json::Error> {
        Ok(Self {
            neutral: crate::json_size::encoded_len(&NeutralMessage::from(message))?,
            replay: message
                .provider_replay
                .as_ref()
                .map(crate::json_size::encoded_len)
                .transpose()?,
            images: MessageImages::measure(message),
        })
    }

    pub(crate) fn retained(self) -> usize {
        self.replay.map_or(self.neutral, |bytes| {
            // Both shapes include the same field; only its JSON value changes.
            (self.neutral - b"null".len()).saturating_add(bytes)
        })
    }
}

pub(crate) struct NeutralMessages<'a>(pub &'a [Message]);
impl Serialize for NeutralMessages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(NeutralMessage::from))
    }
}
