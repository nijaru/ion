//! Borrowed neutral-message sizing, distinct from retained replay representation.
//!
//! Opaque adapter continuation data can duplicate text/tool payloads, include
//! signatures or contain provider-only material. It is not additional neutral
//! prompt text. These views estimate that prompt; they are not wire-size or
//! provider-token counters and never alter the retained request or history.
use ion_ai::{Content, Message, Role};
use serde::{Serialize, Serializer};

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

pub(crate) struct NeutralMessages<'a>(pub &'a [Message]);
impl Serialize for NeutralMessages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(NeutralMessage::from))
    }
}
