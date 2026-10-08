//! Own bracketed-paste framing before key/reply parsers see untrusted payloads.
use super::InputError;

pub const MAX_PASTE_BYTES: usize = 64 * 1024;
const BEGIN: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

pub(super) enum Frame {
    Bytes(Vec<u8>),
    Paste(String),
    Rejected(InputError),
}

#[derive(Debug, Default)]
pub(super) struct PasteFramer {
    prefix: Vec<u8>,
    active: Option<Active>,
}

#[derive(Debug, Default)]
struct Active {
    // None means overflow: retain only the closing-marker prefix, never a
    // truncated payload that a consumer could accidentally submit.
    body: Option<Vec<u8>>,
    end: Vec<u8>,
}

impl Active {
    fn append(&mut self, bytes: &[u8], frames: &mut Vec<Frame>) {
        if let Some(body) = &mut self.body {
            if bytes.len() > MAX_PASTE_BYTES.saturating_sub(body.len()) {
                self.body = None;
                frames.push(Frame::Rejected(InputError::PasteTooLarge));
            } else {
                body.extend_from_slice(bytes);
            }
        }
    }
}

impl PasteFramer {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut normal = Vec::new();
        for &byte in bytes {
            if let Some(active) = &mut self.active {
                if byte == END[active.end.len()] {
                    active.end.push(byte);
                    if active.end.len() == END.len() {
                        if let Some(body) = active.body.take() {
                            frames.push(match String::from_utf8(body) {
                                Ok(text) => Frame::Paste(text),
                                Err(_) => Frame::Rejected(InputError::InvalidPasteUtf8),
                            });
                        }
                        self.active = None;
                    }
                } else {
                    let end = std::mem::take(&mut active.end);
                    active.append(&end, &mut frames);
                    if byte == END[0] {
                        active.end.push(byte);
                    } else {
                        active.append(&[byte], &mut frames);
                    }
                }
            } else if byte == BEGIN[self.prefix.len()] {
                self.prefix.push(byte);
                if self.prefix.len() == BEGIN.len() {
                    if !normal.is_empty() {
                        frames.push(Frame::Bytes(std::mem::take(&mut normal)));
                    }
                    self.prefix.clear();
                    self.active = Some(Active {
                        body: Some(Vec::new()),
                        end: Vec::new(),
                    });
                }
            } else {
                normal.append(&mut self.prefix);
                if byte == BEGIN[0] {
                    self.prefix.push(byte);
                } else {
                    normal.push(byte);
                }
            }
        }
        if !normal.is_empty() {
            frames.push(Frame::Bytes(normal));
        }
        frames
    }

    pub(super) fn flush_prefix(&mut self) -> Vec<u8> {
        // A lone Escape is a key; a recognizable CSI/paste candidate is not.
        // Retain the latter across key grace expiry so a delayed marker cannot
        // release its body (including Enter) as executable keyboard input.
        if self.prefix.len() == 1 {
            std::mem::take(&mut self.prefix)
        } else {
            Vec::new()
        }
    }

    pub(super) fn finish(&mut self) -> Option<InputError> {
        let incomplete_prefix = self.prefix.len() > 1;
        self.prefix.clear();
        let incomplete_body = self
            .active
            .take()
            .is_some_and(|active| active.body.is_some());
        (incomplete_prefix || incomplete_body).then_some(InputError::IncompletePaste)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_discards_storage_until_close_and_exact_limit_is_valid() {
        let mut framer = PasteFramer::default();
        framer.feed(BEGIN);
        let payload = vec![b'x'; MAX_PASTE_BYTES];
        assert!(framer.feed(&payload).is_empty());
        assert!(
            matches!(framer.feed(END).as_slice(), [Frame::Paste(text)] if text.len() == MAX_PASTE_BYTES)
        );
        framer.feed(BEGIN);
        assert!(matches!(
            framer.feed(&vec![b'x'; MAX_PASTE_BYTES + 1]).as_slice(),
            [Frame::Rejected(InputError::PasteTooLarge)]
        ));
        for _ in 0..32 {
            assert!(framer.feed(&payload).is_empty());
            assert!(framer.active.as_ref().unwrap().body.is_none());
        }
        assert!(framer.feed(END).is_empty());
        assert!(framer.active.is_none());
        framer.feed(BEGIN);
        framer.feed(b"incomplete");
        assert_eq!(framer.finish(), Some(InputError::IncompletePaste));
    }
}
